//! Nereid backend: a local native worker on watch, or the opt-in desktop SSH bridge.
//! Pairing and input grants are always owned by the shared Pict session engine.
#[cfg(feature = "dev-ssh")]
use anyhow::Context;
use anyhow::{Result, ensure};
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(feature = "dev-ssh")]
use std::{
    process::Child,
    sync::{Arc, Mutex, mpsc},
};

static LEASE: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "dev-ssh")]
pub(super) fn command(mode: &str) -> Result<Command> {
    let target = std::env::var("PICT_NEREID_SSH").context("PICT_NEREID_SSH is required")?;
    ensure!(
        !target.starts_with('-') && !target.contains(char::is_whitespace),
        "invalid SSH target"
    );
    let mut c = Command::new("ssh");
    c.args([
        "-T",
        "-o",
        if mode == "venus" {
            "Compression=no"
        } else {
            "Compression=yes"
        },
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ServerAliveInterval=3",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "StrictHostKeyChecking=yes",
    ]);
    if let Ok(alias) = std::env::var("PICT_NEREID_HOST_KEY") {
        c.args(["-o", &format!("HostKeyAlias={alias}")]);
    }
    c.arg("--")
        .arg(target)
        .arg("python3")
        .arg("/userdata/pict-demo/watch-bridge.py")
        .arg(mode);
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(c)
}
#[cfg(not(feature = "dev-ssh"))]
pub(super) fn command(mode: &str) -> Result<Command> {
    ensure!(mode == "venus", "Only native Venus worker is supported");
    let path = std::env::var_os("PICT_VENUS_WORKER")
        .unwrap_or_else(|| "/usr/libexec/pict/nereid-venus".into());
    let mut c = Command::new(path);
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(c)
}
#[cfg(not(feature = "dev-ssh"))]
fn control(args: &[u32]) -> Result<()> {
    crate::output::configure(
        args[0] != 0,
        args[1],
        args[2],
        args[3],
        args.get(4).copied().unwrap_or(35000),
    )
}
#[cfg(feature = "dev-ssh")]
fn control(args: &[u32]) -> Result<()> {
    let mut command = command("configure")?;
    for v in args {
        command.arg(v.to_string());
    }
    let result = command.output()?;
    ensure!(result.status.success(), "Nereid configuration failed");
    Ok(())
}
pub struct Lease;
impl Lease {
    pub fn create(width: u32, height: u32) -> Result<Self> {
        ensure!(
            LEASE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "Nereid supports one desktop session"
        );
        let lease = Self;
        resize(width, height, 100)?;
        Ok(lease)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(e) = control(&[0, 0, 0, 100]) {
            eprintln!("Nereid disable: {e:#}");
        }
        LEASE.store(false, Ordering::Release);
    }
}
pub fn resize(width: u32, height: u32, scale: u32) -> Result<()> {
    ensure!(
        (64..=1920).contains(&width)
            && (64..=1920).contains(&height)
            && width as u64 * height as u64 <= 1920 * 1080
            && matches!(scale, 100 | 200)
            && width % (scale / 100) == 0
            && height % (scale / 100) == 0,
        "Nereid demo supports integer 100%/200% scale and up to 1080p"
    );
    ensure!(
        !venus_enabled() || (width >= 96 && height <= 1088 && width % 2 == 0 && height % 2 == 0),
        "Venus requires even dimensions, width 96..1920 and height 64..1088"
    );
    control(&[
        1,
        width,
        height,
        scale,
        if venus_enabled() { 35000 } else { 15000 },
    ])
}
#[cfg(feature = "dev-ssh")]
struct Process {
    child: Child,
}
#[cfg(feature = "dev-ssh")]
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(super) fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "set pipe nonblocking"
    );
    Ok(())
}
fn wait(fd: i32, events: i16, stop: &AtomicBool, deadline: Instant) -> Result<()> {
    ensure!(!stop.load(Ordering::Acquire), "bridge stopped");
    ensure!(Instant::now() < deadline, "Nereid worker timed out");
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut p, 1, 50) };
    if r < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub(super) fn write(
    writer: &mut std::process::ChildStdin,
    bytes: &[u8],
    stop: &AtomicBool,
) -> Result<()> {
    let mut bytes = bytes;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !bytes.is_empty() {
        wait(writer.as_raw_fd(), libc::POLLOUT, stop, deadline)?;
        match writer.write(bytes) {
            Ok(0) => anyhow::bail!("Nereid worker input closed"),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(super) fn read(
    reader: &mut std::process::ChildStdout,
    mut bytes: &mut [u8],
    stop: &AtomicBool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !bytes.is_empty() {
        wait(reader.as_raw_fd(), libc::POLLIN, stop, deadline)?;
        match reader.read(bytes) {
            Ok(0) => anyhow::bail!("Nereid worker output closed"),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
#[cfg(feature = "dev-ssh")]
pub struct Input {
    sender: Option<mpsc::SyncSender<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(feature = "dev-ssh")]
impl Input {
    pub fn new() -> Result<Self> {
        let mut process = Process {
            child: command("input")?.spawn()?,
        };
        let mut input = process.child.stdin.take().unwrap();
        let mut output = process.child.stdout.take().unwrap();
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut ready = [0; 2];
        read(&mut output, &mut ready, &stop)?;
        ensure!(&ready == b"OK", "Nereid input unavailable");
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(128);
        let error = Arc::new(Mutex::new(None));
        let errors = error.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let _process = process;
            while !stopped.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(bytes) => {
                        if let Err(e) = write(&mut input, &bytes, &stopped) {
                            *errors.lock().unwrap() = Some(e.to_string());
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            // EOF makes the helper release every held key/button before device removal.
            drop(input);
            std::thread::sleep(Duration::from_millis(100));
        });
        Ok(Self {
            sender: Some(sender),
            stop,
            error,
            thread: Some(thread),
        })
    }
    pub fn send(&self, value: serde_json::Value) -> Result<()> {
        if let Some(error) = self.error.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.sender
            .as_ref()
            .context("input closed")?
            .try_send(bytes)
            .context("input queue full/disconnected")
    }
}
#[cfg(feature = "dev-ssh")]
impl Drop for Input {
    fn drop(&mut self) {
        self.sender.take();
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn venus_enabled() -> bool {
    true
}
