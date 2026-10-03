use crate::model::State;
use std::{
    io::{self, Write},
    path::Path,
};
pub fn load(path: &Path) -> io::Result<State> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let state: State = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            let mut ids = std::collections::BTreeSet::new();
            if state.version != 1
                || state.alarms.len() > 32
                || state.timers.len() > 32
                || state.stopwatch.laps.len() > 100
                || state
                    .alarms
                    .iter()
                    .any(|a| a.hour >= 24 || a.minute >= 60 || a.days > 127 || !ids.insert(a.id))
                || state.timers.iter().any(|t| {
                    t.total <= 0
                        || t.total > 604800000
                        || t.remaining < 0
                        || t.remaining > t.total
                        || !ids.insert(t.id)
                })
                || ids.iter().any(|id| *id == 0 || *id >= state.next_id)
            {
                return Err(io::Error::other(
                    "invalid clock state; original file preserved",
                ));
            }
            Ok(state)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}
pub fn save(path: &Path, state: &State) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("state path needs a directory"))?;
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer(&mut file, state).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temp, path)?;
    std::fs::File::open(parent)?.sync_all()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_state_is_not_replaced_with_empty_state() {
        let path = std::env::temp_dir()
            .join(format!("clock-store-{}", std::process::id()))
            .join("state.json");
        save(&path, &State::default()).unwrap();
        assert_eq!(load(&path).unwrap().version, 1);
        std::fs::write(&path, b"invalid").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
