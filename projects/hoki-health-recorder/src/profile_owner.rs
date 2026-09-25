//! Durable reservation, held in addition to a permanent process flock.
//! Caller must hold that flock for every operation. Drop never releases ownership.
use crate::{valid_session_id, Result};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) struct Reservation {
    directory: PathBuf,
    identity: Value,
}
impl Reservation {
    /// Reopen a saved claim without creating or changing ownership.
    /// Caller must hold the profile flock, including throughout subsequent reads.
    pub(crate) fn existing(
        directory: &Path,
        session: &Path,
        boot: &str,
        owner: &str,
    ) -> Result<Self> {
        if !valid_session_id(boot)
            || !valid_session_id(owner)
            || !directory.is_absolute()
            || !session.is_absolute()
        {
            return Err("invalid existing profile identity".into());
        }
        let m = fs::symlink_metadata(directory)?;
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err("unsafe profile reservation directory".into());
        }
        let session = fs::canonicalize(session)?;
        let reservation = Self {
            directory: directory.into(),
            identity: json!({"version":1,"boot_id":boot,"owner":owner,
                "session_directory":session.to_str().ok_or("non-UTF8 session path")?}),
        };
        reservation.verify()?;
        Ok(reservation)
    }
    pub(crate) fn claim(directory: &Path, session: &Path, boot: &str, owner: &str) -> Result<Self> {
        if !valid_session_id(boot)
            || !valid_session_id(owner)
            || !directory.is_absolute()
            || !session.is_absolute()
        {
            return Err("invalid profile reservation identity".into());
        }
        match DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {
                File::open(directory.parent().ok_or("missing reservation parent")?)?.sync_all()?
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let m = fs::symlink_metadata(directory)?;
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err("unsafe profile reservation directory".into());
        }
        File::open(directory.parent().ok_or("missing reservation parent")?)?.sync_all()?;
        let session = fs::canonicalize(session)?;
        let session = session
            .to_str()
            .ok_or("non-UTF8 reservation session path")?;
        let reservation = Self {
            directory: directory.into(),
            identity: json!({"version":1,"boot_id":boot,"owner":owner,"session_directory":session}),
        };
        match reservation.read()? {
            Some(saved) if saved != reservation.identity => {
                return Err("another configuration session requires recovery".into())
            }
            Some(_) => {}
            None => {
                let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
                    .trim()
                    .to_string();
                if !valid_session_id(&id) {
                    return Err("invalid reservation UUID".into());
                }
                let pending = directory.join(format!("owner-{id}.pending"));
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&pending)?;
                serde_json::to_writer(&mut file, &reservation.identity)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                // Serialized by the caller's permanent flock. Never replace a different owner.
                fs::rename(pending, directory.join("owner.json"))?;
            }
        }
        // Also sync on reacquisition: a prior process may have died after rename.
        File::open(directory)?.sync_all()?;
        Ok(reservation)
    }
    fn read(&self) -> Result<Option<Value>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(self.directory.join("owner.json"))
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let m = file.metadata()?;
        if !m.is_file()
            || m.uid() != unsafe { libc::geteuid() }
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() > 8192
        {
            return Err("unsafe profile reservation file".into());
        }
        Ok(Some(serde_json::from_reader(file.take(8193))?))
    }
    pub(crate) fn verify(&self) -> Result<()> {
        if self.read()?.as_ref() != Some(&self.identity) {
            return Err("configuration reservation missing or changed".into());
        }
        Ok(())
    }
    /// Only after a validated restored journal or fresh cross-boot baseline
    /// assessment authorizing release is durably published.
    pub(crate) fn release(&self) -> Result<()> {
        match self.read()? {
            Some(saved) if saved != self.identity => {
                return Err("refusing to release another profile owner".into())
            }
            Some(_) => fs::remove_file(self.directory.join("owner.json"))?,
            None => {}
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const BOOT: &str = "12345678-1234-1234-1234-123456789abc";
    const OTHER: &str = "12345678-1234-1234-1234-123456789abd";
    #[test]
    fn reservation_survives_drop_and_requires_same_session_until_release() {
        let root = std::env::temp_dir().join(format!(
            "hoki-reservation-{}",
            fs::read_to_string("/proc/sys/kernel/random/uuid")
                .unwrap()
                .trim()
        ));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        let state = root.join("state");
        let r = Reservation::claim(&state, &root, BOOT, BOOT).unwrap();
        r.verify().unwrap();
        Reservation::existing(&state, &root, BOOT, BOOT)
            .unwrap()
            .verify()
            .unwrap();
        assert!(Reservation::existing(&state, &root, OTHER, BOOT).is_err());
        drop(r);
        assert!(Reservation::claim(&state, &root, BOOT, OTHER).is_err());
        assert!(Reservation::claim(&state, &root, OTHER, BOOT).is_err());
        assert!(Reservation::claim(&state, &state, BOOT, BOOT).is_err());
        let r = Reservation::claim(&state, &root, BOOT, BOOT).unwrap();
        r.release().unwrap();
        r.release().unwrap();
        assert!(Reservation::existing(&state, &root, BOOT, BOOT).is_err());
        assert!(!state.join("owner.json").exists());
        assert!(r.verify().is_err());
        let next = Reservation::claim(&state, &root, BOOT, OTHER).unwrap();
        assert!(r.release().is_err());
        next.verify().unwrap();
        next.release().unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(state.join("owner.json"))
            .unwrap()
            .write_all(b"partial")
            .unwrap();
        assert!(Reservation::claim(&state, &root, BOOT, BOOT).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
