// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
//! Explicit device-bound storage; never consumes a PIN or changes enrollment.
use crate::{
    backend::{read_private, run_helper},
    secure_container::{SecureContainer, SecureContainerConfig},
    Error,
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const STATE: &str = "/var/lib/nereid-auth/device";
const MOUNT: &str = "/mnt/device";
const HELPER: &str = "/usr/libexec/nereid-auth/backend.py";
const MAX_RECORD: usize = 76 + 4096;

fn validate_record(bytes: &[u8]) -> Result<(), Error> {
    if !(77..=MAX_RECORD).contains(&bytes.len()) || &bytes[..4] != b"NDW1" {
        return Err("Invalid device key record".into());
    }
    let size = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
    if size == 0 || size > 4096 || bytes.len() != 76 + size || bytes[8..16] != [0; 8] {
        return Err("Invalid device key bounds or authentication policy".into());
    }
    Ok(())
}

fn decode_reply(op: u32, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if bytes.len() < 16
        || &bytes[..4] != b"NDR1"
        || u32::from_le_bytes(bytes[4..8].try_into()?) != op
        || bytes[8..12] != [0; 4]
        || u32::from_le_bytes(bytes[12..16].try_into()?) as usize != bytes.len() - 16
    {
        return Err("Device key operation failed; stopped".into());
    }
    match op {
        7 if bytes.len() >= 48 => validate_record(&bytes[48..])?,
        8 if bytes.len() == 48 => (),
        _ => return Err("Invalid device key reply payload".into()),
    }
    Ok(Zeroizing::new(bytes[16..].to_vec()))
}

fn request(op: u32, record: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if op == 8 {
        validate_record(record)?;
    } else if op != 7 || !record.is_empty() {
        return Err("Invalid device key request".into());
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(20 + record.len()));
    frame.extend_from_slice(b"NGD1");
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&[0; 8]); // no UID, handle or PIN
    frame.extend_from_slice(&(record.len() as u32).to_le_bytes());
    frame.extend_from_slice(record);
    decode_reply(op, &run_helper(Path::new(HELPER), &frame, 48 + MAX_RECORD)?)
}

fn private_directory(path: &Path, create: bool) -> Result<File, Error> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => File::open(path.parent().ok_or("Missing parent")?)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    let st = file.metadata()?;
    if !st.is_dir() || st.uid() != 0 || st.mode() & 0o7777 != 0o700 {
        return Err("Device storage directory must be root-owned mode 0700".into());
    }
    Ok(file)
}

fn write_new(directory: &File, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(Path::new(STATE).join(name))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    directory.sync_all()?;
    Ok(())
}

fn require_absent(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => Err("Device storage already exists or needs explicit recovery".into()),
    }
}

fn wait_for_devices() -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !["/dev/qseecom", "/dev/ion", "/dev/mmcblk0rpmb"]
        .iter()
        .all(|p| Path::new(p).exists())
    {
        if Instant::now() >= deadline {
            return Err("Device storage hardware readiness timeout".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// `provision` is an explicit one-time operation. `open` cannot create any key
/// or container. A durable guard blocks reuse after any interrupted provisioning.
pub fn run(operation: &str) -> Result<(), Error> {
    if !matches!(operation, "provision" | "open" | "close") {
        return Err("Usage: nereid-device-storage provision|open|close".into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/run/nereid-device-storage.lock")?;
    let st = lock.metadata()?;
    if !st.is_file()
        || st.uid() != 0
        || st.mode() & 0o7777 != 0o600
        || st.nlink() != 1
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err("Device storage busy or unsafe lock".into());
    }
    // Reuse the existing persistent parent; never recreate it on volatile root.
    let _parent = private_directory(Path::new("/var/lib/nereid-auth"), false)?;
    let directory = private_directory(Path::new(STATE), operation == "provision")?;
    // A directory FD inside a mounted filesystem makes ordinary unmount busy.
    // Validate and immediately release it, especially for the close command.
    drop(private_directory(Path::new(MOUNT), operation != "close")?);
    let container = SecureContainer::new(SecureContainerConfig {
        state_directory: STATE.into(),
        mount_point: MOUNT.into(),
        mapping_name: "nereid-device".into(),
        image_bytes: 64 * 1024 * 1024,
        ..Default::default()
    })?;
    if operation == "close" {
        container.close()?;
        println!("Device storage unmounted and closed");
        return Ok(());
    }
    container.preflight()?;
    require_absent(&Path::new(STATE).join("wrapping.pending"))?;
    if operation == "provision" {
        for name in ["volume-key", "secure.luks", "secure.luks.state"] {
            require_absent(&Path::new(STATE).join(name))?;
        }
        write_new(
            &directory,
            "wrapping.pending",
            b"NEREID-DEVICE-WRAPPING-1\n",
        )?;
        wait_for_devices()?;
        let wrapped = request(7, &[])?;
        // Persist only the authenticated ciphertext/blob, never the secret.
        write_new(&directory, "volume-key", &wrapped[32..])?;
        let opened = request(8, &wrapped[32..])?;
        let mismatch = opened
            .iter()
            .zip(&wrapped[..32])
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if mismatch != 0 {
            return Err("Device wrap/unwrap secret mismatch; recovery required".into());
        }
        println!("Device-bound no-PIN wrap/unwrap matched");
        let key = Zeroizing::new(<[u8; 32]>::try_from(&opened[..])?);
        container.provision(&key)?;
        fs::remove_file(Path::new(STATE).join("wrapping.pending"))?;
        directory.sync_all()?;
        println!("Provisioned 64 MiB sparse device container (closed)");
    } else {
        let record = read_private(&Path::new(STATE).join("volume-key"), MAX_RECORD)?
            .ok_or("Device key is not provisioned")?;
        validate_record(&record)?;
        wait_for_devices()?;
        let opened = request(8, &record)?;
        let key = Zeroizing::new(<[u8; 32]>::try_from(&opened[..])?);
        container.open(&key)?;
        println!("Device storage mounted at /mnt/device");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_records_exclude_pin_records_and_sid() {
        let mut r = vec![0; 77];
        r[..4].copy_from_slice(b"NDW1");
        r[4] = 1;
        assert!(validate_record(&r).is_ok());
        r[8] = 1;
        assert!(validate_record(&r).is_err());
        r[8] = 0;
        r[..4].copy_from_slice(b"NKW1");
        assert!(validate_record(&r).is_err());
        r[..4].copy_from_slice(b"NDW1");
        r.push(0);
        assert!(validate_record(&r).is_err());
    }
    #[test]
    fn reply_never_releases_secrets_on_error_or_protocol_confusion() {
        let mut r = vec![0; 48];
        r[..4].copy_from_slice(b"NDR1");
        r[4] = 8;
        r[12] = 32;
        assert!(decode_reply(8, &r).is_ok());
        assert!(decode_reply(7, &r).is_err());
        r[8] = 1;
        assert!(decode_reply(8, &r).is_err());
        r[8] = 0;
        r[..4].copy_from_slice(b"NGR2");
        assert!(decode_reply(8, &r).is_err());
        r[..4].copy_from_slice(b"NDR1");
        r.push(0);
        assert!(decode_reply(8, &r).is_err());
    }
}
