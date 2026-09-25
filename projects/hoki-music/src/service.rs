use crate::{
    decode::{Block, Decoder},
    model::*,
    pulse::Output,
};
use anyhow::{bail, Context, Result};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

type Shared = Arc<Mutex<State>>;
enum Message {
    Command(Request),
    Scanned(Result<Vec<Track>, String>, bool),
}
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })))
        .join("hoki-music/control.sock")
}
pub fn request(request: &Request) -> Result<State> {
    let mut socket = UnixStream::connect(socket_path()).context("Music service is not running")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut socket, request)?;
    socket.write_all(b"\n")?;
    let mut bytes = Vec::new();
    BufReader::new(socket)
        .take(16 * 1024 * 1024 + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Music service response too large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn ensure_running() -> Result<()> {
    if request(&Request::State).is_ok() {
        return Ok(());
    }
    // Deployment uses systemd, while native development can start a detached instance.
    if std::env::var_os("HOKI_MUSIC_DIRECT").is_some() {
        std::process::Command::new(std::env::current_exe()?)
            .arg("--daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    } else {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "hoki-music.service"])
            .status()?;
        if !status.success() {
            bail!("Cannot start music service");
        }
    }
    for _ in 0..30 {
        if request(&Request::State).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("Music service did not start")
}
pub fn run() -> Result<()> {
    let path = socket_path();
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join("daemon.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("Music service already running");
    }
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let shared = Arc::new(Mutex::new(restored_state()));
    let (tx, rx) = mpsc::sync_channel(64);
    let state = shared.clone();
    let scan_tx = tx.clone();
    std::thread::Builder::new()
        .name("music-playback".into())
        .spawn(move || engine(state, rx, scan_tx))?;
    for socket in listener.incoming() {
        let Ok(mut socket) = socket else { continue };
        let result = (|| -> Result<()> {
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            socket.set_write_timeout(Some(Duration::from_secs(2)))?;
            let mut bytes = Vec::new();
            BufReader::new(&mut socket)
                .take(4097)
                .read_until(b'\n', &mut bytes)?;
            if bytes.len() > 4096 {
                bail!("Request too large");
            }
            let command: Request = serde_json::from_slice(&bytes)?;
            let library = matches!(command, Request::Library);
            if !matches!(command, Request::State | Request::Library) {
                tx.try_send(Message::Command(command))
                    .map_err(|_| anyhow::anyhow!("Music command queue is full"))?;
            }
            let s = shared.lock().unwrap();
            let mut snapshot = State {
                library_revision: s.library_revision,
                library: Vec::new(),
                queue: Vec::new(),
                current: None,
                playing: s.playing,
                busy: s.busy,
                scanning: s.scanning,
                position: s.position,
                volume: s.volume,
                notice: s.notice.clone(),
            };
            if library {
                snapshot.library = s.library.clone();
            }
            if let Some(track) = s.current.and_then(|i| s.queue.get(i)) {
                snapshot.queue.push(track.clone());
                snapshot.current = Some(0);
            }
            drop(s);
            serde_json::to_writer(&mut socket, &snapshot)?;
            socket.write_all(b"\n")?;
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("Music IPC: {e}");
        }
    }
    Ok(())
}
fn persist(shared: &Shared) {
    let s = shared.lock().unwrap();
    if let Err(e) = save_state(&s) {
        eprintln!("Music state save failed: {e}");
    }
}
fn failure(shared: &Shared, e: impl std::fmt::Display) {
    let mut s = shared.lock().unwrap();
    s.playing = false;
    s.busy = false;
    s.notice = e.to_string();
}
struct Playback {
    decoder: Decoder,
    output: Option<Output>,
    pending: Option<Block>,
    offset: usize,
    eof: bool,
    drain: Option<libpulse_binding::operation::Operation<dyn FnMut(bool)>>,
    last_write: Instant,
    waiting: bool,
}
impl Playback {
    fn open(track: &Track, position: f64) -> Result<Self> {
        let mut decoder = Decoder::open(track, &config()?)?;
        if position > 0.0 {
            decoder.seek(position)?;
        }
        Ok(Self {
            decoder,
            output: None,
            pending: None,
            offset: 0,
            eof: false,
            drain: None,
            last_write: Instant::now(),
            waiting: false,
        })
    }
    fn step(&mut self, volume: u8) -> Result<bool> {
        self.waiting = false;
        if self.eof {
            self.waiting = true;
            let Some(out) = &mut self.output else {
                return Ok(true);
            };
            out.pump()?;
            if self.drain.is_none() {
                self.drain = Some(out.start_drain());
                self.last_write = Instant::now();
            }
            if self.drain.as_ref().unwrap().get_state()
                != libpulse_binding::operation::State::Running
            {
                return Ok(true);
            }
            if self.last_write.elapsed() > Duration::from_secs(10) {
                bail!("Audio output stalled at end of track");
            }
            return Ok(false);
        }
        if self.pending.is_none() {
            self.pending = self.decoder.next()?;
            self.offset = 0;
            if self.pending.is_none() {
                self.eof = true;
                return Ok(false);
            }
        }
        let block = self.pending.as_ref().unwrap();
        if self
            .output
            .as_ref()
            .is_none_or(|o| o.rate != block.rate || o.channels != block.channels)
        {
            self.output = Some(Output::new(block.rate, block.channels)?);
            self.last_write = Instant::now();
        }
        let count = self
            .output
            .as_mut()
            .unwrap()
            .write(&block.samples[self.offset..], volume)?;
        self.waiting = count == 0;
        if count > 0 {
            self.last_write = Instant::now();
        } else if self.last_write.elapsed() > Duration::from_secs(10) {
            bail!("Audio output stalled");
        }
        self.offset += count;
        if self.offset == block.samples.len() {
            self.pending = None;
        }
        Ok(false)
    }
    fn position(&self) -> f64 {
        let pending = self
            .pending
            .as_ref()
            .map(|b| (b.samples.len() - self.offset) as f64 / b.channels as f64 / b.rate as f64)
            .unwrap_or(0.0);
        (self.decoder.position - pending - self.output.as_ref().map(|o| o.latency()).unwrap_or(0.0))
            .max(0.0)
    }
}
fn load(shared: &Shared) -> Result<Option<Playback>> {
    let (track, position) = {
        let mut s = shared.lock().unwrap();
        s.busy = true;
        (s.current.and_then(|i| s.queue.get(i)).cloned(), s.position)
    };
    let result = track.map(|t| Playback::open(&t, position)).transpose();
    let mut s = shared.lock().unwrap();
    s.busy = false;
    if let Ok(Some(p)) = &result {
        if p.decoder.duration > 0.0 {
            if let Some(i) = s.current {
                s.queue[i].duration = p.decoder.duration;
            }
        }
    }
    result
}
fn engine(shared: Shared, rx: mpsc::Receiver<Message>, tx: mpsc::SyncSender<Message>) {
    let mut playback: Option<Playback> = None;
    loop {
        let playing = shared.lock().unwrap().playing;
        let message = if playing {
            match rx.recv_timeout(Duration::from_millis(
                if playback.as_ref().is_some_and(|p| p.waiting) {
                    20
                } else {
                    0
                },
            )) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        };
        if let Some(message) = message {
            let result = (|| -> Result<()> {
                match message {
                    Message::Scanned(result, remote) => {
                        let mut s = shared.lock().unwrap();
                        s.scanning = false;
                        match result {
                            Ok(tracks) => {
                                s.library.retain(|t| {
                                    matches!(t.source, Source::Navidrome { .. }) != remote
                                });
                                s.library.extend(tracks);
                                s.library.sort_by(|a, b| {
                                    (&a.artist, &a.album, &a.title)
                                        .cmp(&(&b.artist, &b.album, &b.title))
                                });
                                s.library_revision = s.library_revision.wrapping_add(1);
                                s.notice = "Library refreshed".into();
                            }
                            Err(e) => s.notice = e,
                        }
                    }
                    Message::Command(Request::Refresh { remote }) => {
                        let mut s = shared.lock().unwrap();
                        if s.scanning {
                            return Ok(());
                        }
                        s.scanning = true;
                        drop(s);
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            let result = (|| -> Result<Vec<Track>> {
                                let c = config()?;
                                if remote {
                                    crate::network::server_library(
                                        c.navidrome
                                            .as_ref()
                                            .context("Add Navidrome to config.json first")?,
                                    )
                                } else {
                                    crate::decode::local_library(&c)
                                }
                            })()
                            .map_err(|e| e.to_string());
                            let _ = tx.send(Message::Scanned(result, remote));
                        });
                    }
                    Message::Command(Request::Play { source }) => {
                        {
                            let mut s = shared.lock().unwrap();
                            let index = s
                                .library
                                .iter()
                                .position(|t| t.source == source)
                                .context("Track no longer exists; refresh the library")?;
                            s.queue = s.library.clone();
                            s.current = Some(index);
                            s.position = 0.0;
                            s.playing = true;
                            s.notice.clear();
                        }
                        playback = None;
                        playback = load(&shared)?;
                    }
                    Message::Command(Request::Toggle) => {
                        let playing = {
                            let mut s = shared.lock().unwrap();
                            if s.current.is_none() {
                                return Ok(());
                            }
                            s.playing = !s.playing;
                            s.playing
                        };
                        if let Some(p) = playback.as_mut() {
                            if let Some(o) = p.output.as_mut() {
                                o.pause(!playing)?;
                            }
                            p.last_write = Instant::now();
                        } else if playing {
                            playback = load(&shared)?;
                        }
                    }
                    Message::Command(Request::Stop) => {
                        playback = None;
                        let mut s = shared.lock().unwrap();
                        s.playing = false;
                        s.position = 0.0;
                    }
                    Message::Command(Request::Next) => {
                        playback = None;
                        advance(&shared, false);
                        if shared.lock().unwrap().playing {
                            playback = load(&shared)?;
                        }
                    }
                    Message::Command(Request::Previous) => {
                        playback = None;
                        advance(&shared, true);
                        if shared.lock().unwrap().playing {
                            playback = load(&shared)?;
                        }
                    }
                    Message::Command(Request::Seek { seconds }) => {
                        if !seconds.is_finite() || seconds < 0.0 {
                            bail!("Invalid seek position");
                        }
                        let s = shared.lock().unwrap();
                        let duration = s
                            .current
                            .and_then(|i| s.queue.get(i))
                            .map(|t| t.duration)
                            .unwrap_or(0.0);
                        let playing = s.playing;
                        drop(s);
                        let seconds = if duration > 0.0 {
                            seconds.min((duration - 0.1).max(0.0))
                        } else {
                            seconds
                        };
                        // Reopen after seek: no stale samples remain buffered in PulseAudio.
                        playback = None;
                        shared.lock().unwrap().position = seconds;
                        if playing {
                            playback = load(&shared)?;
                        }
                    }
                    Message::Command(Request::Volume { value }) => {
                        shared.lock().unwrap().volume = value.min(100)
                    }
                    Message::Command(Request::Dismiss) => shared.lock().unwrap().notice.clear(),
                    Message::Command(Request::State | Request::Library) => (),
                }
                Ok(())
            })();
            if let Err(e) = result {
                playback = None;
                failure(&shared, e);
            }
            persist(&shared);
        }
        if !shared.lock().unwrap().playing {
            continue;
        }
        if let Some(p) = playback.as_mut() {
            let volume = shared.lock().unwrap().volume;
            match p.step(volume) {
                Ok(true) => {
                    playback = None;
                    advance(&shared, false);
                    persist(&shared);
                    if shared.lock().unwrap().playing {
                        match load(&shared) {
                            Ok(p) => playback = p,
                            Err(e) => failure(&shared, e),
                        }
                    }
                }
                Ok(false) => shared.lock().unwrap().position = p.position(),
                Err(e) => {
                    playback = None;
                    failure(&shared, e);
                    persist(&shared);
                }
            }
        }
    }
}
fn advance(shared: &Shared, previous: bool) {
    let mut s = shared.lock().unwrap();
    if let Some(i) = s.current {
        if previous {
            s.current = Some(if s.position > 3.0 {
                i
            } else {
                i.saturating_sub(1)
            });
        } else if i + 1 < s.queue.len() {
            s.current = Some(i + 1);
        } else {
            s.playing = false;
        }
    }
    s.position = 0.0;
}
#[cfg(test)]
mod tests {
    use super::*;
    fn track(n: &str) -> Track {
        Track {
            title: n.into(),
            artist: "".into(),
            album: "".into(),
            duration: 10.0,
            source: Source::Local { path: n.into() },
        }
    }
    #[test]
    fn end_of_queue_stops_and_previous_restarts() {
        let s = Arc::new(Mutex::new(State {
            queue: vec![track("a"), track("b")],
            current: Some(0),
            playing: true,
            position: 5.0,
            ..State::default()
        }));
        advance(&s, true);
        assert_eq!(s.lock().unwrap().current, Some(0));
        advance(&s, false);
        assert_eq!(s.lock().unwrap().current, Some(1));
        advance(&s, false);
        assert!(!s.lock().unwrap().playing);
    }
}
