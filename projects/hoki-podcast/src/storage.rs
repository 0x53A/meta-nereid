use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct PendingFile(PathBuf);

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn save_download(reader: &mut impl Read, path: &Path) -> io::Result<()> {
    replace_file(reader, path, true)
}

/// Replace frequent progress snapshots atomically without forcing a disk flush
/// on every playback tick. This does not guarantee power-loss durability.
pub fn replace_bytes(mut bytes: &[u8], path: &Path) -> io::Result<()> {
    replace_file(&mut bytes, path, false)
}

fn replace_file(reader: &mut impl Read, path: &Path, sync: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Exclusive creation also handles simultaneous downloads and stale files
    // left by a previous process without truncating either one.
    let (mut file, pending) = loop {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".hoki-write-{}-{serial}.part", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => break (file, PendingFile(temp)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    io::copy(reader, &mut file)?;
    if sync {
        file.sync_all()?;
    }
    drop(file);
    fs::rename(&pending.0, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct InterruptedBody(bool);
    impl Read for InterruptedBody {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if output.is_empty() {
                return Ok(0);
            }
            if self.0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "connection lost",
                ));
            }
            self.0 = true;
            output[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn failed_replacement_preserves_existing_audio_and_success_replaces_it() {
        let dir =
            std::env::temp_dir().join(format!("hoki-download-replace-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("episode.mp3");
        for sync in [false, true] {
            fs::write(&path, b"old complete audio").unwrap();
            assert!(replace_file(&mut InterruptedBody(false), &path, sync).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"old complete audio");
            replace_file(&mut &b"new complete audio"[..], &path, sync).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new complete audio");
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rename_failure_cleans_up_pending_file() {
        let dir = std::env::temp_dir().join(format!("hoki-download-rename-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("existing-directory");
        fs::create_dir(&path).unwrap();
        for sync in [false, true] {
            assert!(replace_file(&mut &b"complete audio"[..], &path, sync).is_err());
            assert!(path.is_dir());
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn destination_is_absent_until_body_is_complete() {
        struct ObservedBody<'a> {
            path: &'a Path,
            remaining: usize,
        }
        impl Read for ObservedBody<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.path.exists() {
                    return Err(io::Error::other(
                        "destination became visible during transfer",
                    ));
                }
                if self.remaining == 0 || output.is_empty() {
                    return Ok(0);
                }
                self.remaining -= 1;
                output[0] = b'a';
                Ok(1)
            }
        }
        let dir =
            std::env::temp_dir().join(format!("hoki-download-visibility-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("episode.mp3");
        let result = save_download(
            &mut ObservedBody {
                path: &path,
                remaining: 32,
            },
            &path,
        );
        let bytes = fs::read(&path);
        fs::remove_dir_all(dir).unwrap();
        result.unwrap();
        assert_eq!(bytes.unwrap(), vec![b'a'; 32]);
    }

    #[test]
    fn interrupted_body_does_not_publish_partial_audio() {
        let dir =
            std::env::temp_dir().join(format!("hoki-download-failure-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("episode.mp3");
        let result = save_download(&mut InterruptedBody(false), &path);
        let exists = path.exists();
        let remaining = fs::read_dir(&dir).unwrap().count();
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
        assert!(
            !exists,
            "partial audio was published as a completed download"
        );
        assert_eq!(remaining, 0, "temporary file was not cleaned up");
    }
}
