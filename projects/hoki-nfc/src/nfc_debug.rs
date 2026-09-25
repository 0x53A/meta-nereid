//! Explicitly enabled, private logs of host-visible NFC traffic.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct DebugLog(Arc<Log>);
struct Log {
    path: PathBuf,
    start: Instant,
    writer: Mutex<Writer>,
}
struct Writer {
    file: BufWriter<File>,
    error: Option<String>,
}
impl DebugLog {
    pub fn create() -> io::Result<Self> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no state directory"))?;
        Self::create_in(&base.join("hoki-nfc"))
    }
    fn create_in(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = directory.join(format!("debug-{timestamp}-{}.log", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let log = Self(Arc::new(Log {
            path,
            start: Instant::now(),
            writer: Mutex::new(Writer {
                file: BufWriter::new(file),
                error: None,
            }),
        }));
        log.text("DEBUG ENABLED: host-visible netlink and NFC command/response bytes; timestamps are elapsed milliseconds");
        Ok(log)
    }
    pub fn path(&self) -> &Path {
        &self.0.path
    }
    pub fn text(&self, text: &str) {
        self.record(text, None);
    }
    pub fn bytes(&self, direction: &str, data: &[u8]) {
        self.record(direction, Some(data));
    }
    fn record(&self, label: &str, data: Option<&[u8]>) {
        let mut state = self.0.writer.lock().unwrap_or_else(|e| e.into_inner());
        if state.error.is_some() {
            return;
        }
        let result = (|| -> io::Result<()> {
            write!(
                state.file,
                "{:010} {label}",
                self.0.start.elapsed().as_millis()
            )?;
            if let Some(data) = data {
                write!(state.file, " len={} hex=", data.len())?;
                for byte in data {
                    write!(state.file, "{byte:02x}")?;
                }
            }
            writeln!(state.file)?;
            state.file.flush()
        })();
        if let Err(e) = result {
            state.error = Some(e.to_string());
            eprintln!("NFC debug log write failed: {e}");
        }
    }
    pub fn error(&self) -> Option<String> {
        self.0
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_complete_log() {
        let dir = std::env::temp_dir().join(format!(
            "hoki-nfc-debug-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let log = DebugLog::create_in(&dir).unwrap();
        log.bytes("NFC TX", &[0, 1, 0xff]);
        log.clone().bytes("NFC RX", &[0x90, 0]);
        log.text("error=test");
        assert!(log.error().is_none());
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(text.contains("NFC TX len=3 hex=0001ff"));
        assert!(text.contains("NFC RX len=2 hex=9000"));
        assert!(text.contains("error=test"));
        assert_eq!(
            fs::metadata(log.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = log.path().to_owned();
        drop(log);
        fs::remove_file(file).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
