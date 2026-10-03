//! One shared watch identity, with isolated state and workers for each companion.
use super::*;
use std::{collections::BTreeMap, net::Shutdown, os::unix::net::UnixListener, sync::Arc, thread};

const MAX_PEERS: usize = 16;

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(super) fn validate_config(cfg: &Config) -> Result<()> {
    ensure!(
        (1714..=1764).contains(&cfg.peer.port()),
        "Peer port outside KDE Connect range"
    );
    ensure!(
        !cfg.peer_id.is_empty()
            && cfg.peer_id.len() <= 64
            && cfg
                .peer_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Invalid device ID"
    );
    ensure!(
        normalized_pin(&cfg.fingerprint)? == cfg.fingerprint,
        "Fingerprint must be normalized"
    );
    Ok(())
}
pub(super) fn configs(base: &Path) -> Result<Vec<(Config, PathBuf)>> {
    let mut paths = vec![base.join("config.json")];
    let extra = base.join("peers");
    if extra.exists() {
        for entry in fs::read_dir(extra)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                paths.push(entry.path().join("config.json"));
            }
        }
    }
    paths.sort();
    let mut result = Vec::new();
    for path in paths.into_iter().filter(|p| p.exists()) {
        let cfg: Config = serde_json::from_slice(&fs::read(&path)?)?;
        validate_config(&cfg)?;
        let directory = path.parent().unwrap().to_path_buf();
        ensure!(
            directory == base
                || directory.file_name().and_then(|s| s.to_str()) == Some(&cfg.peer_id),
            "Peer directory/identity mismatch"
        );
        ensure!(
            !result
                .iter()
                .any(|(c, _): &(Config, PathBuf)| c.peer_id == cfg.peer_id),
            "Duplicate companion identity"
        );
        result.push((cfg, directory));
    }
    ensure!(
        result.len() <= MAX_PEERS,
        "Too many companions (maximum {MAX_PEERS})"
    );
    Ok(result)
}
fn read_value(base: &Path, file: &str) -> Value {
    fs::read(base.join(file))
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or(Value::Null)
}
fn peer_snapshot(cfg: &Config, base: &Path, full: bool) -> Value {
    let mut result = json!({"peer_id":cfg.peer_id,"address":cfg.peer});
    for (key, file) in [
        ("status", "status.json"),
        ("media", "media.json"),
        ("peer", "peer.json"),
        ("ping", "last-ping.json"),
        ("action", "last-action.json"),
        ("battery", "battery.json"),
        ("download", "download.json"),
    ] {
        if full || matches!(key, "status" | "peer") {
            result[key] = read_value(base, file);
        }
    }
    let trust = read_value(base, "paired.json");
    result["trusted"] =
        json!(trust["peer_id"] == cfg.peer_id && trust["fingerprint"] == cfg.fingerprint);
    if !result["peer"]["name"].is_string() {
        result["peer"] = json!({"name":cfg.peer_id});
    }
    result
}

struct Worker {
    cfg: Config,
    base: PathBuf,
    tx: mpsc::SyncSender<String>,
    volume: Arc<Mutex<Option<String>>>,
    wake: UnixStream,
}
impl Worker {
    fn start(
        cfg: Config,
        base: PathBuf,
        identity: &(String, X509, PKey<Private>),
        discovery: discovery::Discovery,
    ) -> Result<Self> {
        private_dir(&base)?;
        status(&base, "disconnected", false)?;
        let (tx, rx) = mpsc::sync_channel(16);
        let volume = Arc::new(Mutex::new(None));
        let (mut wake_read, wake) = UnixStream::pair()?;
        wake_read.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        let worker = Self {
            cfg: cfg.clone(),
            base: base.clone(),
            tx,
            volume: volume.clone(),
            wake,
        };
        let (id, cert, key) = identity.clone();
        thread::Builder::new()
            .name(format!("peer-{}", cfg.peer_id))
            .spawn(move || loop {
                if let Err(error) = session(
                    &base,
                    &cfg,
                    &id,
                    &cert,
                    &key,
                    &rx,
                    &volume,
                    &mut wake_read,
                    discovery.take_transport(&cfg.peer_id),
                ) {
                    log(
                        "disconnected",
                        json!({"peer_id":cfg.peer_id,"error":error.to_string()}),
                    );
                }
                if let Err(error) = status(&base, "disconnected", false) {
                    log("status-error", error.to_string());
                }
                while rx.try_recv().is_ok() {}
                *volume.lock().unwrap() = None;
                thread::sleep(Duration::from_secs(5));
            })?;
        Ok(worker)
    }
    fn queue(&mut self, command: &str) -> &'static str {
        if !matches!(
            read_value(&self.base, "status.json")["state"].as_str(),
            Some("connected" | "pairing")
        ) {
            return "offline";
        }
        if media::volume_target(command).is_some() {
            *self.volume.lock().unwrap() = Some(command.into());
        } else if self.tx.try_send(command.into()).is_err() {
            return "busy";
        }
        let _ = self.wake.write(&[1]);
        "queued"
    }
}
struct Manager {
    base: PathBuf,
    identity: (String, X509, PKey<Private>),
    workers: BTreeMap<String, Worker>,
    selected: String,
    discovery: discovery::Discovery,
}
impl Manager {
    fn reload(&mut self) -> Result<()> {
        let entries = configs(&self.base)?;
        ensure!(
            self.workers
                .keys()
                .all(|id| entries.iter().any(|(cfg, _)| &cfg.peer_id == id)),
            "Existing companion removed; stop the daemon before removing configuration"
        );
        // Never silently change a running worker's trust anchor.
        for (cfg, _) in &entries {
            if let Some(worker) = self.workers.get(&cfg.peer_id) {
                ensure!(
                    worker.cfg == *cfg,
                    "Existing companion configuration changed; review and restart explicitly"
                );
            }
        }
        for (cfg, base) in entries {
            if !self.workers.contains_key(&cfg.peer_id) {
                let worker = Worker::start(cfg, base, &self.identity, self.discovery.clone())?;
                self.workers.insert(worker.cfg.peer_id.clone(), worker);
            }
        }
        if !self.workers.contains_key(&self.selected) {
            self.selected = self
                .workers
                .values()
                .find(|w| w.base == self.base)
                .or_else(|| self.workers.values().next())
                .map(|w| w.cfg.peer_id.clone())
                .unwrap_or_default();
        }
        Ok(())
    }
    fn snapshot(&self) -> Value {
        let mut result = self
            .workers
            .get(&self.selected)
            .map(|w| peer_snapshot(&w.cfg, &w.base, true))
            .unwrap_or_else(|| json!({}));
        let peers: Vec<_> = self.workers.values().map(|w| {
            let p = peer_snapshot(&w.cfg, &w.base, false);
            json!({"peer_id":p["peer_id"],"address":p["address"],"peer":p["peer"],"status":p["status"],"trusted":p["trusted"]})
        }).collect();
        result["selected_peer"] = json!(self.selected);
        result["peers"] = json!(peers);
        result["discovery"] = self.discovery.snapshot();
        result
    }
    fn select(&mut self, id: &str) -> Result<()> {
        ensure!(self.workers.contains_key(id), "Unknown companion");
        private_write(&self.base.join("selected.json"), &serde_json::to_vec(id)?)?;
        self.selected = id.into();
        Ok(())
    }
    fn command(&mut self, input: &str) -> Result<String> {
        if input == "discover" {
            self.reload()?;
            let mut excluded: Vec<_> = self.workers.keys().cloned().collect();
            excluded.push(self.identity.0.clone());
            self.discovery.start(excluded, &self.base, &self.identity)?;
            return Ok("queued".into());
        }
        if let Some(token) = input.strip_prefix("approve:") {
            self.discovery.approve(token, true)?;
            return Ok("queued".into());
        }
        if let Some(token) = input.strip_prefix("reject:") {
            self.discovery.approve(token, false)?;
            return Ok("queued".into());
        }
        if let Some(id) = input.strip_prefix("enroll:") {
            ensure!(
                !self.workers.contains_key(id),
                "Companion already configured"
            );
            ensure!(self.workers.len() < MAX_PEERS, "Too many companions");
            self.discovery.enroll(&self.base, id, &self.identity)?;
            return Ok("queued".into());
        }
        if input == "snapshot" || input == "devices" || input == "status" {
            // Recover configs saved before a previous enrollment was interrupted.
            self.reload()?;
            return Ok(serde_json::to_string(&self.snapshot())?);
        }
        if input == "reload" {
            self.reload()?;
            return Ok("ok".into());
        }
        if let Some(id) = input.strip_prefix("select:") {
            self.select(id)?;
            return Ok("queued".into());
        }
        // Explicitly addressed UI commands cannot move to another companion
        // if selection changes while an action waits in the UI's queue.
        let envelope: Value = if input.starts_with('{') {
            serde_json::from_str(input)?
        } else {
            Value::Null
        };
        let (id, command) = if envelope.is_null() {
            (self.selected.as_str(), input)
        } else {
            (
                envelope["peer_id"].as_str().context("Missing peer_id")?,
                envelope["command"].as_str().context("Missing command")?,
            )
        };
        if matches!(command, "next-device" | "previous-device") {
            let ids: Vec<_> = self.workers.keys().cloned().collect();
            let index = ids
                .iter()
                .position(|p| p == id)
                .context("Unknown companion")?;
            let next = if command == "next-device" {
                (index + 1) % ids.len()
            } else {
                (index + ids.len() - 1) % ids.len()
            };
            self.select(&ids[next])?;
            return Ok("queued".into());
        }
        ensure!(valid_command(command), "Unknown command");
        let worker = self.workers.get_mut(id).context("Unknown companion")?;
        Ok(worker.queue(command).into())
    }
}
fn valid_command(c: &str) -> bool {
    if let Some((kind, token)) = c.split_once(':') {
        if matches!(kind, "accept-pair" | "reject-pair") {
            return token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit());
        }
    }
    [
        "pair",
        "ping",
        "unpair",
        "refresh",
        "next-player",
        "previous-player",
        "play-pause",
        "next",
        "previous",
        "volume-up",
        "volume-down",
    ]
    .contains(&c)
        || media::volume_adjustment(c).is_some()
        || media::volume_target(c).is_some()
}
pub(super) fn request(base: &Path, command: &str) -> Result<String> {
    let mut socket = UnixStream::connect(base.join("control.sock"))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    socket.write_all(command.as_bytes())?;
    socket.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    socket.take(262144).read_to_string(&mut reply)?;
    ensure!(!reply.starts_with("error:"), "{reply}");
    Ok(reply)
}
fn serve(base: &Path) -> Result<()> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(base.join("daemon.lock"))?;
    lock.try_lock()
        .context("Another Hoki Connect daemon is running")?;
    let config_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(base.join("config.lock"))?;
    config_lock.lock()?;
    let watch_identity = identity(base)?;
    drop(config_lock);
    let mut manager = Manager {
        base: base.into(),
        identity: watch_identity,
        workers: BTreeMap::new(),
        discovery: discovery::Discovery::default(),
        selected: read_value(base, "selected.json")
            .as_str()
            .unwrap_or_default()
            .into(),
    };
    manager.reload()?;
    let socket = base.join("control.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    for incoming in listener.incoming() {
        let mut conn = match incoming {
            Ok(conn) => conn,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::Interrupted {
                    log("control-accept-error", error.to_string());
                    thread::sleep(Duration::from_secs(1));
                }
                continue;
            }
        };
        conn.set_read_timeout(Some(Duration::from_secs(2)))?;
        conn.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut data = String::new();
        if (&mut conn).take(4097).read_to_string(&mut data).is_ok() && data.len() <= 4096 {
            let response = manager
                .command(data.trim())
                .unwrap_or_else(|e| format!("error: {e}"));
            let _ = conn.write_all(response.as_bytes());
        }
    }
    Ok(())
}
pub(super) fn save_config(base: &Path, cfg: &Config, initial: bool) -> Result<PathBuf> {
    // Serialize initial identity/configuration creation across CLI processes.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(base.join("config.lock"))?;
    lock.lock()?;
    validate_config(cfg)?;
    let existing = configs(base)?;
    ensure!(
        !initial || existing.is_empty(),
        "Already configured; use add to keep existing companions"
    );
    ensure!(
        !existing.iter().any(|(c, _)| c.peer_id == cfg.peer_id),
        "Companion already configured; refusing to overwrite trust"
    );
    ensure!(existing.len() < MAX_PEERS, "Too many companions");
    identity(base)?;
    let destination = if existing.is_empty() {
        base.to_path_buf()
    } else {
        base.join("peers").join(&cfg.peer_id)
    };
    if destination != base {
        private_dir(&base.join("peers"))?;
    }
    private_dir(&destination)?;
    private_write(
        &destination.join("config.json"),
        &serde_json::to_vec_pretty(cfg)?,
    )?;
    Ok(destination)
}
pub(super) fn add(base: &Path, args: &[String], initial: bool) -> Result<()> {
    let cfg = Config {
        peer: args[1].parse().context("Use IP:port")?,
        peer_id: args[2].clone(),
        fingerprint: normalized_pin(&args[3])?,
    };
    save_config(base, &cfg, initial)?;
    println!("Configured {}", cfg.peer_id);
    if base.join("control.sock").exists() {
        // Configuration is already durable even if the daemon is unavailable.
        match request(base, "reload") {
            Ok(reply) if reply == "ok" => println!("Companion loaded by running daemon"),
            _ => println!("Configuration saved; start/restart the daemon to load it"),
        }
    }
    Ok(())
}
pub(super) fn main_command(base: &Path, args: &[String]) -> Result<()> {
    private_dir(base)?;
    match args.first().map(String::as_str) {
        Some("init" | "add") if args.len() == 4 => add(base, args, args[0] == "init"),
        Some("serve") if args.len() == 1 => serve(base),
        Some("select") if args.len() == 2 => { println!("{}", request(base, &format!("select:{}", args[1]))?); Ok(()) }
        Some("status" | "devices") if args.len() == 1 => { println!("{}", request(base, "snapshot")?); Ok(()) }
        Some(c) if (valid_command(c) || matches!(c, "next-device" | "previous-device")) && args.len() <= 2 => {
            let command = if let Some(id) = args.get(1) { json!({"peer_id":id,"command":c}).to_string() } else { c.into() };
            let reply = request(base, &command)?;
            ensure!(reply == "queued", "Command not queued: {reply}");
            println!("{reply}"); Ok(())
        }
        _ => bail!("Usage: hoki-connect init|add IP:PORT DEVICE_ID SHA256_FINGERPRINT | serve | devices | select DEVICE_ID | status | pair|ping|unpair|refresh|next-player|previous-player|play-pause|next|previous|volume-up|volume-down|next-device|previous-device [DEVICE_ID]"),
    }
}
