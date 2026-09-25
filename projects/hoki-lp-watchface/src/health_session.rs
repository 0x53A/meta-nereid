//! A control directory is single-use, including after an interrupted capture.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub fn claim(dir: &Path, identity: &str) -> io::Result<()> {
    // Claim before examining old controls so concurrent recorders cannot both
    // pass the checks. Never remove this marker, even when startup fails.
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("session"))?;
    marker.write_all(identity.as_bytes())?;
    marker.sync_all()?;
    sync_ancestors(dir)?;
    for name in ["flush-request", "flush-done", "flush-done.tmp", "stop"] {
        match std::fs::symlink_metadata(dir.join(name)) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("stale control {name}; use a new capture directory"),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn sync_ancestors(dir: &Path) -> io::Result<()> {
    let absolute = dir.canonicalize()?;
    for ancestor in absolute.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn directory() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoki-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }
    #[test]
    fn rejects_reuse_after_success() {
        let dir = directory();
        claim(&dir, "first").unwrap();
        assert_eq!(
            claim(&dir, "second").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("session")).unwrap(),
            "first"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_each_legacy_control_and_retains_claim() {
        for name in ["flush-request", "flush-done", "flush-done.tmp", "stop"] {
            let dir = directory();
            std::fs::write(dir.join(name), "1").unwrap();
            assert_eq!(
                claim(&dir, "first").unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert!(dir.join("session").exists());
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), "1");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
