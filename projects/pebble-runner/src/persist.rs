//! Pebble's per-UUID, 256-byte key/value storage. Each update is atomic on disk.
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Mutex};

pub const MISSING: i32 = -9;
pub const INVALID: i32 = -4;
pub const RANGE: i32 = -8;
const IO_ERROR: i32 = -6;

struct Store {
    path: PathBuf,
    values: BTreeMap<u32, Vec<u8>>,
    readable: bool,
}
impl Store {
    fn open(path: PathBuf) -> Self {
        let loaded = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BTreeMap<u32, Vec<u8>>>(&bytes)
                .ok()
                .filter(|v| v.values().all(|b| !b.is_empty() && b.len() <= 256)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(BTreeMap::new()),
            Err(_) => None,
        };
        Self {
            path,
            readable: loaded.is_some(),
            values: loaded.unwrap_or_default(),
        }
    }
    fn save(&mut self, values: BTreeMap<u32, Vec<u8>>) -> Result<(), i32> {
        if !self.readable {
            return Err(IO_ERROR);
        }
        let write = || -> std::io::Result<()> {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            let dir = self.path.parent().unwrap();
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            let temp = self
                .path
                .with_extension(format!("{}.tmp", std::process::id()));
            let bytes = serde_json::to_vec(&values)?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(temp, &self.path)?;
            std::fs::File::open(dir)?.sync_all()?;
            Ok(())
        };
        write().map_err(|e| {
            eprintln!("[persist] write failed: {e}");
            IO_ERROR
        })?;
        self.values = values;
        Ok(())
    }
    fn write(&mut self, key: u32, bytes: &[u8]) -> i32 {
        if bytes.is_empty() || bytes.len() > 256 {
            return RANGE;
        }
        if self.readable && self.values.get(&key).is_some_and(|old| old == bytes) {
            return bytes.len() as i32;
        }
        let mut values = self.values.clone();
        values.insert(key, bytes.to_vec());
        self.save(values).map_or_else(|e| e, |_| bytes.len() as i32)
    }
}
static STORE: Mutex<Option<Store>> = Mutex::new(None);
pub fn select(uuid: &str) {
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/home/ceres".into()))
                .join(".local/share")
        });
    let store = Store::open(
        root.join("pebble-runner/persist")
            .join(format!("{uuid}.json")),
    );
    if !store.readable {
        eprintln!("[persist] cannot read store; refusing to overwrite it");
    }
    *STORE.lock().unwrap() = Some(store);
}
pub fn reset() {
    *STORE.lock().unwrap() = None;
}
pub fn read(key: u32) -> Option<Vec<u8>> {
    STORE.lock().unwrap().as_ref()?.values.get(&key).cloned()
}
pub fn write(key: u32, bytes: &[u8]) -> i32 {
    STORE
        .lock()
        .unwrap()
        .as_mut()
        .map_or(IO_ERROR, |s| s.write(key, bytes))
}
pub fn delete(key: u32) -> i32 {
    let mut guard = STORE.lock().unwrap();
    let Some(s) = guard.as_mut() else {
        return IO_ERROR;
    };
    if !s.values.contains_key(&key) {
        return MISSING;
    }
    let mut values = s.values.clone();
    values.remove(&key);
    s.save(values).map_or_else(|e| e, |_| 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn survives_reopen_isolates_apps_and_preserves_corrupt_store() {
        let dir = std::env::temp_dir().join(format!("pebble-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("app-a.json");
        let mut a = Store::open(path.clone());
        assert_eq!(a.write(42, b"blue\0"), 5);
        assert_eq!(a.write(2, &(-123_i32).to_le_bytes()), 4);
        assert_eq!(a.write(3, &[1; 257]), RANGE);
        assert_eq!(Store::open(path.clone()).values[&42], b"blue\0");
        assert!(Store::open(dir.join("app-b.json")).values.is_empty());
        std::fs::write(&path, b"corrupt").unwrap();
        assert_eq!(Store::open(path.clone()).write(42, b"red\0"), IO_ERROR);
        assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
pub fn select_test(path: PathBuf) {
    *STORE.lock().unwrap() = Some(Store::open(path));
}
