//! Live transaction adapter. Callers must independently supervise restoration;
//! holding this lease prevents competing adapters, not direct vendor clients.
use crate::{
    sleep_transaction::Backend,
    ssc_helper::{private_json, validate_snapshot, Helper},
    valid_session_id, Result,
};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) fn lease(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
    {
        return Err("unsafe sleep profile lease".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file) // Never unlink: the inode is the stable ownership object.
}
fn uuid() -> Result<String> {
    let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    if !valid_session_id(&id) {
        return Err("invalid capture UUID".into());
    }
    Ok(id)
}
pub struct LiveBackend {
    helper: Helper,
    root: PathBuf,
    owner: String,
    _lease: File,
    reservation: crate::profile_owner::Reservation,
}
impl LiveBackend {
    /// Only call after arranging independently supervised restoration. This
    /// constructor cannot prove that external service policy. No public CLI
    /// enables this backend until the lifecycle adapter supplies that contract.
    pub fn new(executable: &Path, root: &Path, owner: &str, supervisor: &str) -> Result<Self> {
        if crate::ssc_helper::supervisor_owner(supervisor)? != owner {
            return Err("sleep supervisor owner mismatch".into());
        }
        let helper = Helper::new(executable, root)?.with_supervisor(supervisor)?;
        if !valid_session_id(owner) {
            return Err("invalid configuration owner".into());
        }
        let lease = lease(Path::new("/run/hoki-sleep-profile.lock"))?;
        let existing = root.join("transaction.json");
        match fs::symlink_metadata(&existing) {
            Ok(_) => {
                let saved = private_json(&existing)?;
                if saved["owner"] != owner || saved["boot_id"] != helper.boot() {
                    return Err("transaction belongs to another owner or boot".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        crate::ssc_quiesce::quiesce(root, owner)?;
        let reservation = crate::profile_owner::Reservation::claim(
            Path::new("/var/lib/hoki-sleep-profile"),
            root,
            helper.boot(),
            owner,
        )?;
        Ok(Self {
            reservation,
            helper,
            root: root.into(),
            owner: owner.into(),
            _lease: lease,
        })
    }
}
impl Backend for LiveBackend {
    fn finish_restoration(&mut self) -> Result<()> {
        let journal = private_json(&self.root.join("transaction.json"))?;
        let transaction =
            crate::sleep_transaction::Transaction::load(journal, self.helper.boot(), &self.owner)?;
        if transaction.journal()["phase"] != "restored" {
            return Err("configuration restoration is not committed".into());
        }
        // Retry the durability barrier before removing the global recovery claim.
        File::open(self.root.join("transaction.json"))?.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        self.reservation.release()
    }

    fn persist(&mut self, journal: &Value) -> Result<()> {
        self.reservation.verify()?;
        if journal["owner"] != self.owner || journal["boot_id"] != self.helper.boot() {
            return Err("transaction checkpoint identity mismatch".into());
        }
        // Unique temporary names permit recovery after an interrupted publication.
        // Unpublished files remain evidence and never replace the committed journal.
        let pending = self.root.join(format!("transaction-{}.pending", uuid()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending)?;
        serde_json::to_writer(&mut file, journal)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(pending, self.root.join("transaction.json"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn read(&mut self, baseline: &Value) -> Result<String> {
        if baseline["boot_id"] != self.helper.boot() {
            return Err("stale read baseline".into());
        }
        let mode = baseline["mode"].as_str().ok_or("missing getter mode")?;
        let source = baseline["source"].as_str().ok_or("missing getter source")?;
        let event = u32::try_from(
            baseline["event_id"]
                .as_u64()
                .ok_or("missing getter event")?,
        )?;
        let directory = self
            .helper
            .capture(&format!("read-{}", uuid()?), Some((mode, source)))?;
        validate_snapshot(
            &private_json(&directory.join("config.json"))?,
            &private_json(&directory.join("status.json"))?,
            self.helper.boot(),
            mode,
            source,
            event,
        )
    }
    fn set(&mut self, op: &Value, restore: bool) -> Result<()> {
        self.reservation.verify()?;
        // Independently check the COMMITTED checkpoint before running any setter.
        // A caller cannot skip the transaction engine's write-ahead protocol.
        let journal = private_json(&self.root.join("transaction.json"))?;
        let transaction =
            crate::sleep_transaction::Transaction::load(journal, self.helper.boot(), &self.owner)?;
        let j = transaction.journal();
        let operations = j["plan"]["operations"]
            .as_array()
            .ok_or("missing operations")?;
        let mut matches = operations
            .iter()
            .enumerate()
            .filter(|(_, candidate)| *candidate == op);
        let (index, _) = matches.next().ok_or("setter not owned by journal")?;
        if matches.next().is_some()
            || j["states"][index]
                != if restore {
                    "restore_intent"
                } else {
                    "enable_intent"
                }
            || j["phase"] != if restore { "restoring" } else { "enabling" }
        {
            return Err("setter lacks committed matching intent".into());
        }
        self.helper.control(
            &format!("{}-{}", if restore { "restore" } else { "enable" }, uuid()?),
            op,
            restore,
        )?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn profile_lease_excludes_other_handles_and_rejects_unsafe_inodes() {
        let root = std::env::temp_dir().join(format!("hoki-profile-{}", uuid().unwrap()));
        fs::create_dir(&root).unwrap();
        let path = root.join("lock");
        let first = lease(&path).unwrap();
        assert!(lease(&path).is_err());
        drop(first);
        drop(lease(&path).unwrap());
        fs::hard_link(&path, root.join("hard")).unwrap();
        assert!(lease(&path).is_err());
        fs::remove_file(root.join("hard")).unwrap();
        symlink(&path, root.join("symlink")).unwrap();
        assert!(lease(&root.join("symlink")).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(lease(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
