use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub fn save_values(path: &Path, updates: &[(&str, String)]) -> io::Result<()> {
    if updates
        .iter()
        .any(|(key, value)| key.contains(['=', '\n', '\r']) || value.contains(['\n', '\r']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration values must fit on one line",
        ));
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut content = String::new();
    for line in existing.lines() {
        if line
            .trim()
            .split_once('=')
            .is_some_and(|(key, _)| updates.iter().any(|(name, _)| *name == key.trim()))
        {
            continue;
        }
        content.push_str(line);
        content.push('\n');
    }
    for (key, value) in updates {
        content.push_str(&format!("{key}={value}\n"));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "config needs parent directory")
    })?;
    std::fs::create_dir_all(parent)?;
    use std::os::unix::fs::OpenOptionsExt;
    let (tmp, mut file) = loop {
        let tmp = parent.join(format!(
            ".hoki-config-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn stale_temporary_file_does_not_block_a_preference_save() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("hoki-pref-stale-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("watchface.conf");
        std::fs::write(&path, "show_seconds=false\nother=value\n").unwrap();
        let stale = dir.join(format!(
            ".hoki-config-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.load(Ordering::Relaxed)
        ));
        std::fs::write(&stale, b"previous interrupted save").unwrap();
        let result = save_values(&path, &[("show_seconds", "true".into())]);
        let contents = std::fs::read_to_string(&path).unwrap();
        let preserved = std::fs::read(&stale).unwrap();
        let remaining = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(dir).unwrap();
        result.unwrap();
        assert_eq!(contents, "other=value\nshow_seconds=true\n");
        assert_eq!(preserved, b"previous interrupted save");
        assert_eq!(remaining, 2);
    }

    #[test]
    fn saving_preferences_preserves_other_keys_and_permissions() {
        let _lock = TEST_LOCK.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hoki-pref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watchface.conf");
        std::fs::write(&path, "show_seconds=false\nfuture=yes\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        save_values(&path, &[("show_seconds", "true".into())]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "future=yes\nshow_seconds=true\n"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // A malformed old config must not be overwritten while reporting success.
        std::fs::write(&path, [0xff]).unwrap();
        assert!(save_values(&path, &[("show_seconds", "false".into())]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff]);
        // A path whose parent is a regular file cannot be saved.
        assert!(save_values(
            &path.join("impossible"),
            &[("show_seconds", "false".into())]
        )
        .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
