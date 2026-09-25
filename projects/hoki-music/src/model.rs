use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Local { path: PathBuf },
    Navidrome { server: String, id: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub source: Source,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub library_revision: u64,
    pub library: Vec<Track>,
    pub queue: Vec<Track>,
    pub current: Option<usize>,
    pub playing: bool,
    pub busy: bool,
    pub scanning: bool,
    pub position: f64,
    pub volume: u8,
    pub notice: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    State,
    Library,
    Refresh { remote: bool },
    Play { source: Source },
    Toggle,
    Stop,
    Next,
    Previous,
    Seek { seconds: f64 },
    Volume { value: u8 },
    Dismiss,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub music_dirs: Vec<PathBuf>,
    pub navidrome: Option<Server>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Server {
    pub url: String,
    pub username: String,
    pub password: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            music_dirs: vec![home().join("Music")],
            navidrome: None,
        }
    }
}
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/ceres"))
}
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("hoki-music")
}
pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("hoki-music/config.json")
}
pub fn config() -> anyhow::Result<Config> {
    let path = config_path();
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn save_state(state: &State) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("state.tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.sync_all()?;
    std::fs::rename(tmp, dir.join("state.json"))?;
    Ok(())
}
pub fn restored_state() -> State {
    let mut s: State = std::fs::read(data_dir().join("state.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| State {
            volume: 60,
            ..State::default()
        });
    s.playing = false;
    s.busy = false;
    s.scanning = false;
    s.notice.clear();
    s.volume = s.volume.min(100);
    if s.current.is_some_and(|i| i >= s.queue.len()) {
        s.current = None;
    }
    if !s.position.is_finite() || s.position < 0.0 {
        s.position = 0.0;
    }
    s
}
