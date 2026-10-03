//! Paired Share payloads. Remote filenames never become paths or shell arguments.
use super::*;
use std::{
    ffi::CString,
    fs::File,
    net::Shutdown,
    os::fd::FromRawFd,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

const MAX_SIZE: u64 = 512 * 1024 * 1024;
const RESERVE: u64 = 64 * 1024 * 1024;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

fn filename(name: &str) -> Result<()> {
    ensure!(!name.is_empty() && name.len() <= 180 && name.trim() == name
        && !name.starts_with('.') && !name.ends_with('.')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        "Filename must be a plain visible name, not a path");
    Ok(())
}
struct Job {
    name: String,
    size: u64,
    port: Option<u16>,
}
impl Job {
    fn parse(p: &Packet) -> Result<Self> {
        let name = p.body["filename"]
            .as_str()
            .context("Only file shares are supported")?;
        filename(name)?;
        let size = p.payload_size.unwrap_or(0);
        ensure!(
            p.payload_info.is_none() || p.payload_size.is_some(),
            "Missing payload size"
        );
        ensure!(size <= MAX_SIZE, "File exceeds the 512 MiB limit");
        let port = match &p.payload_info {
            Some(info) => {
                let port = info["port"].as_u64().context("Missing payload port")?;
                ensure!((1739..=1764).contains(&port), "Invalid payload port");
                Some(port as u16)
            }
            None => None,
        };
        ensure!(size == 0 || port.is_some(), "Missing file payload");
        Ok(Self {
            name: name.into(),
            size,
            port,
        })
    }
}

fn syscall(rc: libc::c_int) -> std::io::Result<libc::c_int> {
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}
fn directory(home: &Path) -> Result<File> {
    ensure!(home.is_absolute(), "Download home must be absolute");
    let home = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(home)?;
    // Only the fixed Download child is created/opened, relative to this directory FD.
    let result = unsafe { libc::mkdirat(home.as_raw_fd(), c"Download".as_ptr(), 0o700) };
    if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd = syscall(unsafe {
        libc::openat(
            home.as_raw_fd(),
            c"Download".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    // openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn space(dir: &File, needed: u64) -> Result<()> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    syscall(unsafe { libc::fstatvfs(dir.as_raw_fd(), stats.as_mut_ptr()) })?;
    let stats = unsafe { stats.assume_init() };
    let available = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
    ensure!(
        available >= needed.saturating_add(RESERVE),
        "Not enough free space in Download"
    );
    Ok(())
}
struct Temporary {
    dir: File,
    name: CString,
    file: File,
}
impl Temporary {
    fn new(dir: File) -> Result<Self> {
        let mut random = [0; 16];
        openssl::rand::rand_bytes(&mut random)?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let name = CString::new(format!(".connect-{token}.part"))?;
        let fd = syscall(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        Ok(Self {
            dir,
            name,
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    fn finish(self, original: &str) -> Result<String> {
        filename(original)?;
        self.file.sync_all()?;
        let path = Path::new(original);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(original);
        let extension = path.extension().and_then(|s| s.to_str());
        for n in 0..1000 {
            let name = if n == 0 {
                original.into()
            } else {
                format!(
                    "{stem} ({n}){}",
                    extension.map(|e| format!(".{e}")).unwrap_or_default()
                )
            };
            let dest = CString::new(name.as_str())?;
            // linkat publishes atomically and refuses every existing destination, including links.
            let result = unsafe {
                libc::linkat(
                    self.dir.as_raw_fd(),
                    self.name.as_ptr(),
                    self.dir.as_raw_fd(),
                    dest.as_ptr(),
                    0,
                )
            };
            if result == 0 {
                let dir = self.dir.try_clone()?;
                drop(self);
                dir.sync_all()?;
                return Ok(name);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        bail!("Too many files with the same name")
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.dir.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

#[derive(Default)]
struct State {
    cancelled: bool,
    socket: Option<TcpStream>,
}
pub(super) struct Receiver {
    tx: Option<mpsc::SyncSender<Job>>,
    state: Arc<Mutex<State>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Receiver {
    pub fn start(
        base: &Path,
        cfg: &Config,
        peer: SocketAddr,
        cert: &X509,
        key: &PKey<Private>,
    ) -> Result<Self> {
        let home = std::env::var_os("HOKI_CONNECT_DOWNLOAD_HOME")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| "/home/ceres".into());
        let (tx, rx) = mpsc::sync_channel::<Job>(8);
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let (base, cfg, cert, key) = (base.to_owned(), cfg.clone(), cert.clone(), key.clone());
        let worker = thread::Builder::new().name("connect-download".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                if shared.lock().unwrap().cancelled { break; }
                let result = receive(&home, &job, &cfg, peer, &cert, &key, &shared);
                shared.lock().unwrap().socket = None;
                let value = match result {
                    Ok(name) => json!({"state":"received","filename":name,"bytes":job.size,"at_ms":now_ms()}),
                    Err(error) => json!({"state":"failed","error":error.to_string(),"at_ms":now_ms()}),
                };
                let _ = private_write(&base.join("download.json"), &serde_json::to_vec(&value).unwrap());
                log("download", &value);
            }
        })?;
        Ok(Self {
            tx: Some(tx),
            state,
            worker: Some(worker),
        })
    }
    pub fn submit(&self, packet: &Packet) -> Result<()> {
        let job = Job::parse(packet)?;
        self.tx
            .as_ref()
            .context("File receiver stopped")?
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("File queue is full"))
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock().unwrap();
            state.cancelled = true;
            if let Some(socket) = state.socket.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn receive(
    home: &Path,
    job: &Job,
    cfg: &Config,
    peer: SocketAddr,
    cert: &X509,
    key: &PKey<Private>,
    state: &Mutex<State>,
) -> Result<String> {
    ACTIVE
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            (n < 2).then_some(n + 1)
        })
        .map_err(|_| anyhow::anyhow!("Two file transfers are already active"))?;
    let _slot = Slot;
    let dir = directory(home)?;
    space(&dir, job.size)?;
    let mut temp = Temporary::new(dir)?;
    if let Some(port) = job.port {
        // Use only the authenticated control connection's IP; ignore advertised hosts.
        let tcp =
            TcpStream::connect_timeout(&SocketAddr::new(peer.ip(), port), Duration::from_secs(3))?;
        tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
        {
            let mut state = state.lock().unwrap();
            ensure!(!state.cancelled, "Transfer cancelled");
            state.socket = Some(tcp.try_clone()?);
        }
        let mut context = SslContextBuilder::new(SslMethod::tls_client())?;
        context.set_min_proto_version(Some(SslVersion::TLS1_2))?;
        context.set_certificate(cert)?;
        context.set_private_key(key)?;
        let pin = cfg.fingerprint.clone();
        context.set_verify_callback(SslVerifyMode::PEER, move |_, c| {
            c.error_depth() == 0
                && c.current_cert()
                    .and_then(|c| fingerprint(&c.to_owned()).ok())
                    .is_some_and(|p| p == pin)
        });
        let mut tls = SslStream::new(Ssl::new(&context.build())?, tcp)?;
        tls.connect()?;
        validate_cert(
            &tls.ssl()
                .peer_certificate()
                .context("Missing payload certificate")?,
            cfg,
        )?;
        let started = Instant::now();
        let mut remaining = job.size;
        let mut buffer = [0; 65536];
        while remaining > 0 {
            ensure!(!state.lock().unwrap().cancelled, "Transfer cancelled");
            ensure!(
                started.elapsed() < Duration::from_secs(1800),
                "Transfer exceeded 30 minutes"
            );
            space(&temp.dir, buffer.len() as u64)?;
            let count = remaining.min(buffer.len() as u64) as usize;
            let count = tls.read(&mut buffer[..count])?;
            ensure!(count > 0, "File payload ended early");
            temp.file.write_all(&buffer[..count])?;
            remaining -= count as u64;
        }
        let _ = tls.shutdown();
    }
    // Serialize publication with revocation so cancellation cannot race the final link.
    let state = state.lock().unwrap();
    ensure!(!state.cancelled, "Transfer cancelled");
    temp.finish(&job.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, os::unix::fs::symlink};
    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "hoki-download-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn filenames_are_plain_leaves_and_metadata_is_bounded() {
        for name in [
            "",
            ".",
            "..",
            "../photo.jpg",
            "/photo.jpg",
            "folder/photo.jpg",
            "folder\\photo.jpg",
            "C:photo.jpg",
            ".hidden",
            "photo\n.jpg",
            " photo.jpg",
            "photo.",
            "photo\u{202e}.jpg",
        ] {
            assert!(filename(name).is_err(), "accepted {name:?}");
        }
        for name in ["photo.jpg", "résumé 2026.pdf", "a..b.txt", "%2e%2e.txt"] {
            filename(name).unwrap();
        }
        assert!(filename(&"x".repeat(181)).is_err());
        let mut packet = Packet::new("kdeconnect.share.request", json!({"filename":"photo.jpg"}));
        assert_eq!(Job::parse(&packet).unwrap().size, 0);
        packet.payload_size = Some(10);
        assert!(Job::parse(&packet).is_err());
        packet.payload_info = Some(json!({"port":1739}));
        assert_eq!(Job::parse(&packet).unwrap().port, Some(1739));
        packet.payload_size = Some(MAX_SIZE + 1);
        assert!(Job::parse(&packet).is_err());
        packet.payload_size = Some(10);
        packet.payload_info = Some(json!({"port":22}));
        assert!(Job::parse(&packet).is_err());
    }

    #[test]
    fn publication_preserves_existing_files_and_does_not_follow_links() {
        let home = Home::new();
        fs::write(home.0.join("original"), b"keep").unwrap();
        fs::create_dir(home.0.join("Download")).unwrap();
        symlink(home.0.join("original"), home.0.join("Download/photo.txt")).unwrap();
        let mut temp = Temporary::new(directory(&home.0).unwrap()).unwrap();
        temp.file.write_all(b"received").unwrap();
        assert_eq!(temp.finish("photo.txt").unwrap(), "photo (1).txt");
        assert_eq!(fs::read(home.0.join("original")).unwrap(), b"keep");
        assert_eq!(
            fs::read(home.0.join("Download/photo (1).txt")).unwrap(),
            b"received"
        );
        assert_eq!(
            fs::metadata(home.0.join("Download/photo (1).txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let temp = Temporary::new(directory(&home.0).unwrap()).unwrap();
        drop(temp);
        assert_eq!(fs::read_dir(home.0.join("Download")).unwrap().count(), 2);
        let linked = Home::new();
        symlink(&home.0, linked.0.join("Download")).unwrap();
        assert!(directory(&linked.0).is_err());
    }

    #[test]
    fn authenticated_payload_completion_and_truncation() {
        let home = Home::new();
        let remote = Home::new();
        let (id, cert, key) = identity(&remote.0).unwrap();
        let (_, own_cert, own_key) = identity(&home.0).unwrap();
        // Sequential cases share no network or directory state with deployed devices.
        for (name, declared, trusted, accepted) in [
            ("complete.txt", 7, true, true),
            ("short.txt", 8, true, false),
            ("wrong-certificate.txt", 7, false, false),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let peer = listener.local_addr().unwrap();
            let cfg = Config {
                peer,
                peer_id: id.clone(),
                fingerprint: if trusted {
                    fingerprint(&cert).unwrap()
                } else {
                    "00".repeat(32)
                },
            };
            let (cert, key) = (cert.clone(), key.clone());
            let server = thread::spawn(move || {
                let mut ctx = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
                ctx.set_certificate(&cert).unwrap();
                ctx.set_private_key(&key).unwrap();
                let (tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut tls = SslStream::new(Ssl::new(&ctx.build()).unwrap(), tcp).unwrap();
                if tls.accept().is_err() {
                    return;
                }
                tls.write_all(b"example").unwrap();
                let _ = tls.shutdown();
            });
            let job = Job {
                name: name.into(),
                size: declared,
                port: Some(peer.port()),
            };
            let result = receive(
                &home.0,
                &job,
                &cfg,
                peer,
                &own_cert,
                &own_key,
                &Mutex::new(State::default()),
            );
            server.join().unwrap();
            assert_eq!(result.is_ok(), accepted, "{result:?}");
            assert_eq!(home.0.join("Download").join(name).exists(), accepted);
        }
        assert_eq!(
            fs::read(home.0.join("Download/complete.txt")).unwrap(),
            b"example"
        );
        assert_eq!(fs::read_dir(home.0.join("Download")).unwrap().count(), 1);
        let cfg = Config {
            peer: "127.0.0.1:1739".parse().unwrap(),
            peer_id: id,
            fingerprint: fingerprint(&cert).unwrap(),
        };
        let job = Job {
            name: "cancelled.txt".into(),
            size: 0,
            port: None,
        };
        assert!(receive(
            &home.0,
            &job,
            &cfg,
            cfg.peer,
            &own_cert,
            &own_key,
            &Mutex::new(State {
                cancelled: true,
                socket: None
            })
        )
        .is_err());
        assert!(!home.0.join("Download/cancelled.txt").exists());
    }
}
