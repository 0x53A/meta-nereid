//! Narrow native-helper boundary; persistent opaque handles, never PIN storage.
use crate::{
    keymaster,
    secure_container::{SecureContainer, SecureContainerConfig},
    Error, Outcome,
};
use rand_core::{OsRng, RngCore};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
use zeroize::Zeroizing;

type StoredCredential = (u32, Zeroizing<Vec<u8>>);

pub trait Backend: Send + Sync + 'static {
    fn enrolled(&self) -> Result<bool, Error>;
    fn authenticate(&self, pin: &[u8]) -> Result<Outcome, Error>;
    fn enroll(&self, _pin: &[u8]) -> Result<Outcome, Error> {
        Err("Enrollment unavailable".into())
    }
    fn change_pin(&self, _current: &[u8], _new: &[u8]) -> Result<Outcome, Error> {
        Err("PIN change unavailable".into())
    }
    fn clear_pin(&self, _current: &[u8]) -> Result<Outcome, Error> {
        Err("PIN clearing unavailable".into())
    }
    fn lock(&self) -> Result<(), Error> {
        Ok(())
    }
}
pub struct Native {
    directory: PathBuf,
    helper: PathBuf,
    storage: Option<SecureContainer>,
    operation: Mutex<bool>,
}
impl Native {
    pub fn new(directory: PathBuf, helper: PathBuf) -> Result<Self, Error> {
        let st = fs::symlink_metadata(&directory)?;
        if !st.is_dir() || st.uid() != 0 || st.mode() & 0o777 != 0o700 {
            return Err("Unsafe auth directory".into());
        }
        let size = read_private(&directory.join("secure-storage.conf"), 32)?;
        let storage = if let Some(size) = size {
            let image_bytes: u64 = std::str::from_utf8(&size)?.trim().parse()?;
            let versions = read_private(&directory.join("keymaster.conf"), 63)?
                .ok_or("Keymaster version configuration is required")?;
            keymaster::validate_versions(&versions)?;
            if Path::new("/dev/mapper/nereid-secure").try_exists()? {
                return Err("Existing secure mapping requires recovery".into());
            }
            let mount_point = PathBuf::from("/mnt/secure");
            match fs::DirBuilder::new().mode(0o700).create(&mount_point) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
            let storage = SecureContainer::new(SecureContainerConfig {
                state_directory: directory.clone(),
                mount_point,
                image_bytes,
                ..SecureContainerConfig::default()
            })?;
            storage.preflight()?;
            Some(storage)
        } else {
            for name in [
                "volume-key",
                "wrapping.pending",
                "secure.luks",
                "secure.luks.state",
            ] {
                if entry_exists(&directory.join(name))? {
                    return Err(
                        "Secure storage configuration is missing; no screen-only fallback".into(),
                    );
                }
            }
            None
        };
        Ok(Self {
            directory,
            helper,
            storage,
            operation: Mutex::new(false),
        })
    }
    fn read_handle(&self) -> Result<Option<StoredCredential>, Error> {
        if entry_exists(&self.directory.join("enrollment.pending"))?
            || entry_exists(&self.directory.join("wrapping.pending"))?
            || entry_exists(&self.directory.join("management.pending"))?
            || entry_exists(&self.directory.join("credential.next"))?
        {
            return Err("Enrollment needs recovery".into());
        }
        let path = self.directory.join("credential");
        let mut f = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                for name in ["volume-key", "secure.luks", "secure.luks.state"] {
                    if entry_exists(&self.directory.join(name))? {
                        return Err("Storage exists without a credential; recovery required".into());
                    }
                }
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let st = f.metadata()?;
        if !st.is_file()
            || st.uid() != 0
            || st.mode() & 0o777 != 0o600
            || !(9..=1032).contains(&st.len())
        {
            return Err("Unsafe credential state".into());
        }
        let mut bytes = Zeroizing::new(vec![]);
        f.read_to_end(&mut bytes)?;
        if &bytes[..4] != b"NGC1" {
            return Err("Invalid credential format".into());
        }
        let uid = u32::from_le_bytes(bytes[4..8].try_into()?);
        Ok(Some((uid, Zeroizing::new(bytes[8..].to_vec()))))
    }
    fn sync_dir(&self) -> Result<(), Error> {
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    fn save_exclusive(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.directory.join(name))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        self.sync_dir()
    }
    fn call(
        &self,
        op: u32,
        uid: u32,
        handle: &[u8],
        pin: &[u8],
    ) -> Result<(i32, Zeroizing<Vec<u8>>), Error> {
        let mut request = Zeroizing::new(Vec::with_capacity(20 + handle.len() + pin.len()));
        request.extend_from_slice(b"NGK1");
        request.extend_from_slice(&op.to_le_bytes());
        request.extend_from_slice(&uid.to_le_bytes());
        request.extend_from_slice(&(handle.len() as u16).to_le_bytes());
        request.extend_from_slice(&(pin.len() as u16).to_le_bytes());
        request.extend_from_slice(&0u32.to_le_bytes());
        request.extend_from_slice(handle);
        request.extend_from_slice(pin);
        let response = run_helper(&self.helper, &request, 1040)?;
        if response.len() < 16 || &response[..4] != b"NGR1" {
            return Err("Native authentication unavailable".into());
        }
        if u32::from_le_bytes(response[4..8].try_into()?) != op {
            return Err("Helper operation mismatch".into());
        }
        let status = i32::from_le_bytes(response[8..12].try_into()?);
        let length = u32::from_le_bytes(response[12..16].try_into()?) as usize;
        if response.len() != 16 + length || (op == 2 && length != 0) || (status != 0 && length != 0)
        {
            return Err("Invalid helper reply".into());
        }
        Ok((status, Zeroizing::new(response[16..].to_vec())))
    }
}
impl Native {
    fn manage_credential(&self, current: &[u8], new: Option<&[u8]>) -> Result<Outcome, Error> {
        let opened = self
            .operation
            .lock()
            .map_err(|_| "Authentication operation poisoned")?;
        if !crate::protocol::valid_pin(current)
            || new.is_some_and(|p| !crate::protocol::valid_pin(p))
        {
            return Err("Invalid management PIN".into());
        }
        let (uid, handle) = self.read_handle()?.ok_or("No enrolled PIN")?;
        if new.is_none() {
            if *opened {
                return Ok(Outcome::StorageProtected);
            }
            for name in [
                "volume-key",
                "secure.luks",
                "secure.luks.state",
                "wrapping.pending",
            ] {
                if entry_exists(&self.directory.join(name))? {
                    return Ok(Outcome::StorageProtected);
                }
            }
        }
        let op = if new.is_some() { 5u32 } else { 6u32 };
        let new_pin = new.unwrap_or_default();
        let mut request = Zeroizing::new(Vec::with_capacity(
            20 + handle.len() + current.len() + new_pin.len(),
        ));
        request.extend_from_slice(b"NGK3");
        request.extend_from_slice(&op.to_le_bytes());
        request.extend_from_slice(&uid.to_le_bytes());
        request.extend_from_slice(&(handle.len() as u16).to_le_bytes());
        request.extend_from_slice(&(current.len() as u16).to_le_bytes());
        request.extend_from_slice(&(new_pin.len() as u32).to_le_bytes());
        request.extend_from_slice(&handle);
        request.extend_from_slice(current);
        request.extend_from_slice(new_pin);
        self.save_exclusive("management.pending", &op.to_le_bytes())?;
        let response = run_helper(&self.helper, &request, 1040)?;
        let (status, next_handle) = decode_management_reply(&response, op)?;
        if status == 1 {
            // Native guarantees generic verification rejection before mutation.
            fs::remove_file(self.directory.join("management.pending"))?;
            self.sync_dir()?;
            return Ok(Outcome::Rejected);
        }
        if op == 5 {
            let mut record = Zeroizing::new(b"NGC1".to_vec());
            record.extend_from_slice(&uid.to_le_bytes());
            record.extend_from_slice(next_handle);
            self.save_exclusive("credential.next", &record)?;
            fs::rename(
                self.directory.join("credential.next"),
                self.directory.join("credential"),
            )?;
            self.sync_dir()?;
        } else {
            fs::remove_file(self.directory.join("credential"))?;
            self.sync_dir()?;
        }
        fs::remove_file(self.directory.join("management.pending"))?;
        self.sync_dir()?;
        Ok(if op == 5 {
            Outcome::PinChanged
        } else {
            Outcome::PinCleared
        })
    }

    fn authenticate_storage(
        &self,
        storage: &SecureContainer,
        opened: &mut bool,
        uid: u32,
        handle: &[u8],
        pin: &[u8],
    ) -> Result<Outcome, Error> {
        if *opened {
            return Err("Secure storage is already open".into());
        }
        let secret = if let Some(bytes) = read_private(&self.directory.join("volume-key"), 8192)? {
            let wrapped = keymaster::WrappedKey::parse(&bytes)?;
            match keymaster::unwrap(&self.helper, uid, handle, pin, &wrapped) {
                Ok(secret) => secret,
                Err(e) if e.is::<keymaster::AuthenticationRejected>() => {
                    return Ok(Outcome::Rejected)
                }
                Err(e) => return Err(e),
            }
        } else {
            // This explicit configuration opt-in permits one initial storage
            // setup. An interrupted attempt never silently generates a new key.
            if entry_exists(&self.directory.join("secure.luks"))?
                || entry_exists(&self.directory.join("secure.luks.state"))?
            {
                return Err("Existing container has no wrapping key; recovery required".into());
            }
            self.save_exclusive("wrapping.pending", b"NKW1")?;
            let provisioned = match keymaster::provision(&self.helper, uid, handle, pin) {
                Ok(value) => value,
                Err(e) if e.is::<keymaster::AuthenticationRejected>() => {
                    // The native status guarantees rejection before generation.
                    fs::remove_file(self.directory.join("wrapping.pending"))?;
                    self.sync_dir()?;
                    return Ok(Outcome::Rejected);
                }
                Err(e) => return Err(e),
            };
            self.save_exclusive("volume-key", provisioned.wrapped.bytes())?;
            storage.provision(&provisioned.secret)?;
            fs::remove_file(self.directory.join("wrapping.pending"))?;
            self.sync_dir()?;
            provisioned.secret
        };
        storage.open(&secret)?;
        *opened = true;
        Ok(Outcome::Unlocked)
    }
}

impl Backend for Native {
    fn change_pin(&self, current: &[u8], new: &[u8]) -> Result<Outcome, Error> {
        self.manage_credential(current, Some(new))
    }
    fn clear_pin(&self, current: &[u8]) -> Result<Outcome, Error> {
        self.manage_credential(current, None)
    }
    fn enrolled(&self) -> Result<bool, Error> {
        Ok(self.read_handle()?.is_some())
    }
    fn lock(&self) -> Result<(), Error> {
        let mut opened = self
            .operation
            .lock()
            .map_err(|_| "Authentication operation poisoned")?;
        if let Some(storage) = &self.storage {
            if *opened {
                storage.close()?;
                *opened = false;
            } else if Path::new("/dev/mapper/nereid-secure").try_exists()? {
                return Err("Uncertain secure mapping requires recovery".into());
            }
        }
        Ok(())
    }
    fn authenticate(&self, pin: &[u8]) -> Result<Outcome, Error> {
        let mut opened = self
            .operation
            .lock()
            .map_err(|_| "Authentication operation poisoned")?;
        if let Some((uid, handle)) = self.read_handle()? {
            if let Some(storage) = &self.storage {
                return self.authenticate_storage(storage, &mut opened, uid, &handle, pin);
            }
            let (status, _) = self.call(2, uid, &handle, pin)?;
            return match status {
                0 => Ok(Outcome::Unlocked),
                // Generic verification failure, NOT proof of a particular cause.
                -30 => Ok(Outcome::Rejected),
                _ => Err("Unclassified Gatekeeper status; stopped".into()),
            };
        }
        Err("No enrolled PIN".into())
    }
    fn enroll(&self, pin: &[u8]) -> Result<Outcome, Error> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| "Authentication operation poisoned")?;
        if self.read_handle()?.is_some() {
            return Err("PIN already enrolled".into());
        }
        let uid = 0x7000_0000 | (OsRng.next_u32() & 0x0fff_ffff);
        self.save_exclusive("enrollment.pending", &uid.to_le_bytes())?;
        let (status, handle) = self.call(1, uid, &[], pin)?;
        if status != 0 || handle.is_empty() {
            return Err("Enrollment failed; recovery required".into());
        }
        let mut record = Zeroizing::new(b"NGC1".to_vec());
        record.extend_from_slice(&uid.to_le_bytes());
        record.extend_from_slice(&handle);
        self.save_exclusive("credential", &record)?;
        fs::remove_file(self.directory.join("enrollment.pending"))?;
        self.sync_dir()?;
        Ok(Outcome::Enrolled)
    }
}

fn decode_management_reply(response: &[u8], operation: u32) -> Result<(u32, &[u8]), Error> {
    if response.len() < 16
        || &response[..4] != b"NGR3"
        || u32::from_le_bytes(response[4..8].try_into()?) != operation
    {
        return Err("Invalid management reply".into());
    }
    let status = u32::from_le_bytes(response[8..12].try_into()?);
    let length = u32::from_le_bytes(response[12..16].try_into()?) as usize;
    if length != response.len() - 16
        || status > 1
        || (status == 1 && length != 0)
        || (status == 0
            && ((operation == 5 && !(1..=1024).contains(&length))
                || (operation == 6 && length != 0)))
        || !matches!(operation, 5 | 6)
    {
        return Err("Invalid management result".into());
    }
    Ok((status, &response[16..]))
}

/// Shared private helper boundary; the Python supervisor owns the one bounded
/// execution and listener-first cleanup. A valid payload alone is insufficient.
pub(crate) fn run_helper(
    helper: &Path,
    request: &[u8],
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut child = Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let write = child
        .stdin
        .take()
        .ok_or("Missing helper input")?
        .write_all(request);
    if write.is_err() {
        let _ = child.wait();
        return Err("Helper input failed".into());
    }
    let mut response = Zeroizing::new(vec![]);
    let read = child
        .stdout
        .take()
        .ok_or("Missing helper output")?
        .take((limit + 1) as u64)
        .read_to_end(&mut response);
    let exit = child.wait()?;
    if read.is_err() || !exit.success() || response.len() > limit {
        return Err("Native authentication unavailable".into());
    }
    Ok(response)
}

fn read_private(path: &Path, limit: usize) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let st = file.metadata()?;
    if !st.is_file()
        || st.uid() != 0
        || st.mode() & 0o7777 != 0o600
        || st.nlink() != 1
        || st.len() > limit as u64
    {
        return Err("Unsafe private authentication state".into());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("Oversized private authentication state".into());
    }
    Ok(Some(bytes))
}

// Presence of a recovery entry counts even if it is a dangling symlink. Only
// an absent directory entry can establish clean, unconfigured state.
fn entry_exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_management_is_never_treated_as_unenrolled() {
        let directory = std::env::temp_dir().join(format!(
            "nereid-management-{}-{}",
            std::process::id(),
            OsRng.next_u64()
        ));
        fs::create_dir(&directory).unwrap();
        let backend = Native {
            directory: directory.clone(),
            helper: PathBuf::from("/nonexistent-helper"),
            storage: None,
            operation: Mutex::new(false),
        };
        assert!(backend.read_handle().unwrap().is_none());
        for name in ["management.pending", "credential.next"] {
            let path = directory.join(name);
            std::os::unix::fs::symlink("missing-target", &path).unwrap();
            assert!(backend.read_handle().is_err());
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn management_reply_requires_exact_operation_status_and_payload() {
        fn reply(op: u32, status: u32, data: &[u8]) -> Vec<u8> {
            let mut out = b"NGR3".to_vec();
            out.extend(op.to_le_bytes());
            out.extend(status.to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
            out
        }
        assert_eq!(
            decode_management_reply(&reply(5, 0, b"handle"), 5).unwrap(),
            (0, b"handle".as_slice())
        );
        assert_eq!(
            decode_management_reply(&reply(6, 0, b""), 6).unwrap(),
            (0, b"".as_slice())
        );
        assert!(decode_management_reply(&reply(5, 1, b""), 5).is_ok());
        for bad in [
            reply(6, 0, b"handle"),
            reply(5, 0, b""),
            reply(5, 1, b"handle"),
            reply(5, 2, b""),
            reply(5, 0, &[0; 1025]),
        ] {
            assert!(decode_management_reply(&bad, 5).is_err());
        }
        let mut bad = reply(5, 0, b"handle");
        bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_management_reply(&bad, 5).is_err());
        for len in 0..22 {
            assert!(decode_management_reply(&reply(5, 0, b"handle")[..len], 5).is_err());
        }
    }
    #[test]
    fn dangling_recovery_entry_is_not_absent() {
        let path = std::env::temp_dir().join(format!(
            "nereid-auth-marker-{}-{}",
            std::process::id(),
            OsRng.next_u64()
        ));
        std::os::unix::fs::symlink("nonexistent-marker-target", &path).unwrap();
        assert!(entry_exists(&path).unwrap());
        fs::remove_file(&path).unwrap();
        assert!(!entry_exists(&path).unwrap());
    }
}
