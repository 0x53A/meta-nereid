mod media;

use anyhow::{bail, ensure, Context, Result};
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    rsa::Rsa,
    ssl::{Ssl, SslContextBuilder, SslMethod, SslStream, SslVerifyMode, SslVersion},
    x509::{X509NameBuilder, X509},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 65536;
const PAIR_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Serialize, Deserialize)]
struct Config {
    peer: SocketAddr,
    peer_id: String,
    fingerprint: String,
}
#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    cert: String,
    key: String,
}
#[derive(Serialize, Deserialize, Debug)]
struct Packet {
    id: u64,
    #[serde(rename = "type")]
    kind: String,
    body: Value,
}
impl Packet {
    fn new(kind: &str, body: Value) -> Self {
        Self {
            id: now_ms(),
            kind: kind.into(),
            body,
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn log(event: &str, detail: impl Serialize) {
    println!("{}", json!({"event":event,"detail":detail}));
}
fn dir() -> PathBuf {
    std::env::var_os("HOKI_CONNECT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/home/ceres".into()))
                .join(".config/hoki-connect")
        })
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().context("State file needs a parent directory")?;
    let (temp, mut f) = loop {
        let temp = parent.join(format!(
            ".hoki-connect-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| -> Result<()> {
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn fingerprint(cert: &X509) -> Result<String> {
    Ok(cert
        .digest(MessageDigest::sha256())?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn normalized_pin(pin: &str) -> Result<String> {
    let p = pin.replace(':', "").to_ascii_lowercase();
    ensure!(
        p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()),
        "Expected SHA256 certificate fingerprint"
    );
    Ok(p)
}
fn identity() -> Result<(String, X509, PKey<Private>)> {
    let path = dir().join("identity.json");
    if path.exists() {
        let i: Identity = serde_json::from_slice(&fs::read(path)?)?;
        let cert = X509::from_pem(i.cert.as_bytes())?;
        let key = PKey::private_key_from_pem(i.key.as_bytes())?;
        ensure!(cert.public_key()?.public_eq(&key), "Identity key mismatch");
        return Ok((i.id, cert, key));
    }
    let mut random = [0u8; 16];
    openssl::rand::rand_bytes(&mut random)?;
    let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let key = PKey::from_rsa(Rsa::generate(2048)?)?;
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("CN", &id)?;
    name.append_entry_by_text("O", "KDE")?;
    name.append_entry_by_text("OU", "KDE Connect")?;
    let name = name.build();
    let mut cert = X509::builder()?;
    cert.set_version(2)?;
    let mut serial = BigNum::new()?;
    serial.rand(128, MsbOption::MAYBE_ZERO, false)?;
    cert.set_serial_number(serial.to_asn1_integer()?.as_ref())?;
    cert.set_subject_name(&name)?;
    cert.set_issuer_name(&name)?;
    cert.set_pubkey(&key)?;
    let yesterday = ((now_ms() / 1000) as i64 - 86400)
        .try_into()
        .context("Clock outside supported range")?;
    cert.set_not_before(Asn1Time::from_unix(yesterday)?.as_ref())?;
    cert.set_not_after(Asn1Time::days_from_now(3650)?.as_ref())?;
    cert.sign(&key, MessageDigest::sha256())?;
    let cert = cert.build();
    let i = Identity {
        id: id.clone(),
        cert: String::from_utf8(cert.to_pem()?)?,
        key: String::from_utf8(key.private_key_to_pem_pkcs8()?)?,
    };
    private_write(&path, &serde_json::to_vec(&i)?)?;
    Ok((id, cert, key))
}
fn identity_packet(id: &str) -> Packet {
    Packet::new(
        "kdeconnect.identity",
        json!({"deviceId":id,"deviceName":"Hoki","deviceType":"phone",
        "protocolVersion":8,"incomingCapabilities":["kdeconnect.ping","kdeconnect.mpris"],"outgoingCapabilities":["kdeconnect.ping","kdeconnect.mpris.request"]}),
    )
}
fn send(stream: &mut impl Write, p: &Packet) -> Result<()> {
    let mut data = serde_json::to_vec(p)?;
    data.push(b'\n');
    stream.write_all(&data)?;
    stream.flush()?;
    Ok(())
}
fn decode(data: &[u8]) -> Result<Packet> {
    ensure!(data.len() <= LIMIT, "Packet exceeds size limit");
    let p: Packet = serde_json::from_slice(data)?;
    ensure!(p.body.is_object(), "Packet body must be an object");
    Ok(p)
}
// Read one byte at a time only for the first identity, never consuming TLS bytes.
fn read_packet(stream: &mut impl Read, limit: usize) -> Result<Packet> {
    let mut bytes = Vec::new();
    loop {
        let mut b = [0];
        stream.read_exact(&mut b)?;
        if b[0] == b'\n' {
            return decode(&bytes);
        }
        ensure!(bytes.len() < limit, "Identity exceeds size limit");
        bytes.push(b[0]);
    }
}
fn validate_identity(p: &Packet, cfg: &Config) -> Result<()> {
    ensure!(
        p.kind == "kdeconnect.identity"
            && p.body["deviceId"] == cfg.peer_id
            && p.body["protocolVersion"] == 8,
        "Peer identity or protocol mismatch"
    );
    Ok(())
}
fn validate_cert(cert: &X509, cfg: &Config) -> Result<()> {
    ensure!(
        fingerprint(cert)? == cfg.fingerprint,
        "Peer certificate changed; refusing connection"
    );
    let cn = cert
        .subject_name()
        .entries_by_nid(Nid::COMMONNAME)
        .next()
        .context("Peer certificate has no CN")?;
    ensure!(
        cn.data().to_string()? == cfg.peer_id,
        "Peer certificate/device ID mismatch"
    );
    Ok(())
}
fn connect(
    cfg: &Config,
    id: &str,
    cert: &X509,
    key: &PKey<Private>,
) -> Result<SslStream<TcpStream>> {
    let mut tcp = TcpStream::connect_timeout(&cfg.peer, Duration::from_secs(5))?;
    tcp.set_read_timeout(Some(Duration::from_secs(8)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(8)))?;
    let mut hello = identity_packet(id);
    hello.body["targetDeviceId"] = json!(cfg.peer_id);
    hello.body["targetProtocolVersion"] = json!(8);
    send(&mut tcp, &hello)?;
    // KDE Connect reverses the TLS roles: the outgoing TCP side is the TLS server.
    let mut ctx = SslContextBuilder::new(SslMethod::tls_server())?;
    ctx.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    ctx.set_certificate(cert)?;
    ctx.set_private_key(key)?;
    ctx.check_private_key()?;
    let pin = cfg.fingerprint.clone();
    ctx.set_verify_callback(
        SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT,
        move |_, c| {
            // Self-signed peers are authenticated by the explicit certificate pin.
            c.error_depth() == 0
                && c.current_cert()
                    .and_then(|cert| cert.digest(MessageDigest::sha256()).ok())
                    .map(|d| d.iter().map(|b| format!("{b:02x}")).collect::<String>() == pin)
                    .unwrap_or(false)
        },
    );
    let mut tls = SslStream::new(Ssl::new(&ctx.build())?, tcp)?;
    tls.accept()?;
    validate_cert(
        &tls.ssl()
            .peer_certificate()
            .context("Missing peer certificate")?,
        cfg,
    )?;
    send(&mut tls, &identity_packet(id))?;
    let peer = read_packet(&mut tls, 8192)?;
    validate_identity(&peer, cfg)?;
    let name: String = peer.body["deviceName"]
        .as_str()
        .unwrap_or("Laptop")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    private_write(
        &dir().join("peer.json"),
        &serde_json::to_vec(&json!({"name": name}))?,
    )?;
    tls.get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(tls)
}
#[derive(Default)]
struct Pairing {
    paired: bool,
    pending: Option<Instant>,
}
impl Pairing {
    fn acknowledgement(&mut self, body: &Value) -> bool {
        if body.get("pair") == Some(&json!(false)) {
            self.paired = false;
            self.pending = None;
            return false;
        }
        // v8 requests contain a timestamp; only an acknowledgement to our live
        // local request grants trust. Unsolicited incoming requests are rejected.
        if body.get("pair") == Some(&json!(true))
            && body.get("timestamp").is_none()
            && self.pending.is_some_and(|t| t.elapsed() < PAIR_TIMEOUT)
        {
            self.pending = None;
            self.paired = true;
            return true;
        }
        false
    }
}
fn status(state: &str, paired: bool) -> Result<()> {
    private_write(
        &dir().join("status.json"),
        &serde_json::to_vec(&json!({"state":state,"paired":paired}))?,
    )
}
fn session(
    cfg: &Config,
    id: &str,
    cert: &X509,
    key: &PKey<Private>,
    rx: &mpsc::Receiver<String>,
    volume_slot: &Mutex<Option<String>>,
    wake: &mut UnixStream,
) -> Result<()> {
    let mut stream = connect(cfg, id, cert, key)?;
    while rx.try_recv().is_ok() {} // Discard actions queued while disconnected/handshaking.
    *volume_slot.lock().unwrap() = None;
    let trust = dir().join("paired.json");
    let mut pair = Pairing::default();
    if trust.exists() {
        let saved: Value = serde_json::from_slice(&fs::read(&trust)?)?;
        ensure!(
            saved["peer_id"] == cfg.peer_id && saved["fingerprint"] == cfg.fingerprint,
            "Stored trust mismatch"
        );
        pair.paired = true;
    }
    status("connected", pair.paired)?;
    log("connected", json!({"peer":cfg.peer,"paired":pair.paired}));
    let mut media = media::Media::default();
    save_media(&media)?;
    if pair.paired {
        request_media(&mut stream, json!({"requestPlayerList":true}))?;
    }
    let mut pending = Vec::new();
    let mut partial_since = None;
    loop {
        let mut commands: Vec<_> = rx.try_iter().collect();
        if let Some(target) = volume_slot.lock().unwrap().take() {
            commands.push(target);
        }
        for command in commands {
            match command.as_str() {
                "pair" if !pair.paired && pair.pending.is_none() => {
                    let timestamp = now_ms() / 1000;
                    send(
                        &mut stream,
                        &Packet::new(
                            "kdeconnect.pair",
                            json!({"pair":true,"timestamp":timestamp}),
                        ),
                    )?;
                    pair.pending = Some(Instant::now());
                    status("pairing", false)?;
                    log("pairing-requested", "Accept Hoki in laptop KDE Connect");
                }
                "ping" if pair.paired => {
                    send(
                        &mut stream,
                        &Packet::new("kdeconnect.ping", json!({"message":"Hello from Hoki"})),
                    )?;
                    log("ping-sent", "Hello from Hoki");
                    private_write(
                        &dir().join("last-action.json"),
                        &serde_json::to_vec(&json!({"action":"ping","sent_ms":now_ms()}))?,
                    )?;
                }
                "refresh" if pair.paired => {
                    request_media(&mut stream, json!({"requestPlayerList":true}))?;
                    if !media.player.is_empty() {
                        request_media(&mut stream, media.request())?;
                    }
                }
                "next-player" | "previous-player" if pair.paired => {
                    if command == "previous-player" {
                        media.select_previous();
                    } else {
                        media.select_next();
                    }
                    save_media(&media)?;
                    if !media.player.is_empty() {
                        request_media(&mut stream, media.request())?;
                    }
                }
                c if pair.paired
                    && (matches!(
                        c,
                        "play-pause" | "next" | "previous" | "volume-up" | "volume-down"
                    ) || media::volume_adjustment(c).is_some()
                        || media::volume_target(c).is_some()) =>
                {
                    if let Some(body) = media.command(&command) {
                        let volume = body["setVolume"].as_i64();
                        request_media(&mut stream, body)?;
                        media.sent_volume(volume);
                        if let Some(value) = volume {
                            log("volume-sent", json!({"at_ms":now_ms(),"volume":value}));
                        }
                        request_media(&mut stream, media.request())?;
                    }
                }
                "unpair" => {
                    pair.paired = false;
                    pair.pending = None;
                    if trust.exists() {
                        fs::remove_file(&trust)?;
                    }
                    status("connected", false)?;
                    send(
                        &mut stream,
                        &Packet::new("kdeconnect.pair", json!({"pair":false})),
                    )?;
                }
                _ => log("command-ignored", command),
            }
        }
        if pair.pending.is_some_and(|t| t.elapsed() >= PAIR_TIMEOUT) {
            pair.pending = None;
            status("connected", false)?;
            send(
                &mut stream,
                &Packet::new("kdeconnect.pair", json!({"pair":false})),
            )?;
            log(
                "pairing-timeout",
                "No approval; issue pair again when ready",
            );
        }
        ensure!(
            !partial_since.is_some_and(|t: Instant| t.elapsed() > Duration::from_secs(15)),
            "Incomplete packet timed out"
        );
        // Sleep until TLS input or local commands arrive; no idle fast polling.
        if stream.ssl().pending() == 0 {
            let mut fds = [
                libc::pollfd {
                    fd: stream.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // Both descriptors outlive this call; poll only writes the two entries.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 1000) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if fds[1].revents != 0 {
                let mut bytes = [0u8; 128];
                while matches!(wake.read(&mut bytes), Ok(n) if n > 0) {}
                continue;
            }
            if fds[0].revents == 0 {
                continue;
            }
        }
        let mut buf = [0u8; 4096];
        match stream.read(&mut buf) {
            Ok(0) => bail!("Peer disconnected"),
            Ok(n) => {
                for &b in &buf[..n] {
                    if b != b'\n' {
                        ensure!(pending.len() < LIMIT, "Packet exceeds size limit");
                        if pending.is_empty() {
                            partial_since = Some(Instant::now());
                        }
                        pending.push(b);
                        continue;
                    }
                    let p = decode(&pending)?;
                    pending.clear();
                    partial_since = None;
                    if p.kind == "kdeconnect.pair" {
                        if pair.acknowledgement(&p.body) {
                            private_write(
                                &trust,
                                &serde_json::to_vec(
                                    &json!({"peer_id":cfg.peer_id,"fingerprint":cfg.fingerprint}),
                                )?,
                            )?;
                            log("paired", &cfg.peer_id);
                            request_media(&mut stream, json!({"requestPlayerList":true}))?;
                        } else if p.body.get("pair") == Some(&json!(false)) {
                            if trust.exists() {
                                fs::remove_file(&trust)?;
                            }
                            log("unpaired", &cfg.peer_id);
                        } else if p.body.get("timestamp").is_some() {
                            // Never approve incoming requests without local user consent.
                            pair.paired = false;
                            pair.pending = None;
                            if trust.exists() {
                                fs::remove_file(&trust)?;
                            }
                            send(
                                &mut stream,
                                &Packet::new("kdeconnect.pair", json!({"pair":false})),
                            )?;
                            log("pairing-rejected", "Initiate pairing locally on Hoki");
                        }
                        status("connected", pair.paired)?;
                    } else if pair.paired && p.kind == "kdeconnect.mpris" {
                        if let Some(value) = p.body["volume"].as_i64() {
                            log("volume-received", json!({"at_ms":now_ms(),"volume":value}));
                        }
                        let before = serde_json::to_vec(&media)?;
                        if media.update(&p.body) {
                            request_media(&mut stream, media.request())?;
                        }
                        if serde_json::to_vec(&media)? != before {
                            save_media(&media)?;
                        }
                    } else if pair.paired && p.kind == "kdeconnect.ping" {
                        let message = p
                            .body
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Ping");
                        let message: String = message.chars().take(256).collect();
                        private_write(
                            &dir().join("last-ping.json"),
                            &serde_json::to_vec(
                                &json!({"received_ms":now_ms(),"message":message}),
                            )?,
                        )?;
                        log("ping-received", message);
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
}
fn request_media(stream: &mut impl Write, body: Value) -> Result<()> {
    send(stream, &Packet::new("kdeconnect.mpris.request", body))
}
fn save_media(media: &media::Media) -> Result<()> {
    private_write(&dir().join("media.json"), &serde_json::to_vec(media)?)
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let base = dir();
    fs::create_dir_all(&base)?;
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    match args.first().map(String::as_str) {
        Some("init") if args.len()==4 => {
            let peer: SocketAddr=args[1].parse().context("Use IP:port, e.g. 100.1.2.3:1716")?;
            ensure!((1714..=1764).contains(&peer.port()),"Peer port outside KDE Connect range");
            ensure!(!args[2].is_empty() && args[2].len()<=64 && args[2].bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'),"Invalid device ID");
            let cfg=Config {peer,peer_id:args[2].clone(),fingerprint:normalized_pin(&args[3])?};
            ensure!(!base.join("config.json").exists(),"Config already exists; inspect it instead of overwriting trust");
            let (id,_,_)=identity()?;private_write(&base.join("config.json"),&serde_json::to_vec_pretty(&cfg)?)?;log("initialized",id);
        }
        Some("serve") => {
            let cfg: Config=serde_json::from_slice(&fs::read(base.join("config.json"))?)?;
            let (id,cert,key)=identity()?;
            // flock prevents duplicate daemons before cleaning our stale socket.
            let lock=OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(base.join("daemon.lock"))?;
            lock.try_lock().context("Another Hoki Connect daemon is running")?;
            status("disconnected",false)?;
            let sock=base.join("control.sock");if sock.exists(){fs::remove_file(&sock)?;}
            let listener=UnixListener::bind(sock)?;let(tx,rx)=mpsc::sync_channel(16);
            let volume_slot = Arc::new(Mutex::new(None::<String>));
            let socket_volume = volume_slot.clone();
            let (mut wake_read, mut wake_write) = UnixStream::pair()?;
            wake_read.set_nonblocking(true)?;
            wake_write.set_nonblocking(true)?;
            thread::spawn(move || { for incoming in listener.incoming() {
                let mut conn = match incoming {
                    Ok(conn) => conn,
                    Err(error) => {
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            log("control-accept-error", error.to_string());
                            // Persistent accept failures must not spin on a watch.
                            thread::sleep(Duration::from_secs(1));
                        }
                        continue;
                    }
                };
                let _=conn.set_read_timeout(Some(Duration::from_secs(2)));
                let mut data=String::new();if (&mut conn).take(4096).read_to_string(&mut data).is_ok() {
                    let c=data.trim();
                    let _=conn.set_write_timeout(Some(Duration::from_secs(2)));
                    if c == "snapshot" {
                        let mut result = json!({});
                        for (key,file) in [("status","status.json"),("media","media.json"),("peer","peer.json"),("ping","last-ping.json"),("action","last-action.json")] {
                            result[key] = fs::read(dir().join(file)).ok().and_then(|v| serde_json::from_slice::<Value>(&v).ok()).unwrap_or(Value::Null);
                        }
                        let _=conn.write_all(serde_json::to_string(&result).unwrap_or_default().as_bytes());
                    } else if media::volume_target(c).is_some() {
                        *socket_volume.lock().unwrap() = Some(c.into());
                        let _=wake_write.write(&[1]);
                        let _=conn.write_all(b"queued");
                    } else if ["pair","ping","unpair","refresh","next-player","previous-player","play-pause","next","previous","volume-up","volume-down"].contains(&c) || media::volume_adjustment(c).is_some() || media::volume_target(c).is_some() {
                        let reply=if tx.try_send(c.into()).is_ok() { let _=wake_write.write(&[1]); "queued" } else { "busy" };
                        let _=conn.write_all(reply.as_bytes());
                    }

                }
            }});
            loop {
                if let Err(e)=session(&cfg,&id,&cert,&key,&rx,&volume_slot,&mut wake_read) { log("disconnected",e.to_string()); }
                status("disconnected",false)?;
                while rx.try_recv().is_ok() {} // Commands never carry across reconnects.
                thread::sleep(Duration::from_secs(5));
            }
        }
        Some("pair"|"ping"|"unpair"|"refresh"|"next-player"|"previous-player"|"play-pause"|"next"|"previous"|"volume-up"|"volume-down") => {let mut s=UnixStream::connect(base.join("control.sock"))?;s.write_all(args[0].as_bytes())?;}
        Some("status") => {println!("{}",String::from_utf8(fs::read(base.join("status.json"))?)?);}
        _=>bail!("Usage: hoki-connect init IP:PORT DEVICE_ID SHA256_FINGERPRINT | serve | pair | ping | unpair | status"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_state_save_does_not_block_later_publication() {
        let directory = std::env::temp_dir().join(format!("hoki-connect-state-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("status.json");
        let stale = path.with_extension("tmp");
        fs::write(&path, b"old state").unwrap();
        fs::write(&stale, b"interrupted save").unwrap();
        private_write(&path, b"new state").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new state");
        assert_eq!(fs::read(&stale).unwrap(), b"interrupted save");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_state_publication_cleans_only_its_own_temporary() {
        let directory = std::env::temp_dir().join(format!("hoki-connect-state-failure-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("status.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("retained"), b"existing directory").unwrap();
        let stale = path.with_extension("tmp");
        fs::write(&stale, b"other save").unwrap();
        assert!(private_write(&path, b"new state").is_err());
        assert_eq!(fs::read(path.join("retained")).unwrap(), b"existing directory");
        assert_eq!(fs::read(&stale).unwrap(), b"other save");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn peer_pin_and_certificate_identity_are_both_required() {
        let cert = X509::from_pem(include_bytes!("../tests/peer-cert.pem")).unwrap();
        let mut cfg = Config {
            peer: "127.0.0.1:1716".parse().unwrap(),
            peer_id: "test-peer".into(),
            fingerprint: fingerprint(&cert).unwrap(),
        };
        assert!(validate_cert(&cert, &cfg).is_ok());
        cfg.fingerprint = "00".repeat(32);
        assert!(validate_cert(&cert, &cfg).is_err());
        cfg.fingerprint = fingerprint(&cert).unwrap();
        cfg.peer_id = "different-peer".into();
        assert!(validate_cert(&cert, &cfg).is_err());
    }
    #[test]
    fn trust_requires_live_local_request_and_ack() {
        let mut p = Pairing::default();
        assert!(!p.acknowledgement(&json!({"pair":true})));
        assert!(!p.paired);
        p.pending = Some(Instant::now());
        assert!(!p.acknowledgement(&json!({"pair":true,"timestamp":0})));
        assert!(!p.paired);
        assert!(p.acknowledgement(&json!({"pair":true})));
        assert!(p.paired);
        assert!(!p.acknowledgement(&json!({"pair":false})));
        assert!(!p.paired);
        p.pending = Some(Instant::now() - Duration::from_secs(30));
        assert!(!p.acknowledgement(&json!({"pair":true})));
    }
    #[test]
    fn bounds_and_types() {
        assert!(decode(&vec![b' '; LIMIT + 1]).is_err());
        assert!(decode(br#"{"id":1,"type":"kdeconnect.ping","body":[]}"#).is_err());
        assert!(read_packet(&mut &b"0123456789\n"[..], 8).is_err());
        assert!(normalized_pin("not a fingerprint").is_err());
    }
    #[test]
    fn identity_changes_rejected() {
        let cfg = Config {
            peer: "127.0.0.1:1716".parse().unwrap(),
            peer_id: "test".into(),
            fingerprint: "00".repeat(32),
        };
        let mut p = identity_packet("test");
        assert!(validate_identity(&p, &cfg).is_ok());
        p.body["protocolVersion"] = json!(7);
        assert!(validate_identity(&p, &cfg).is_err());
        p.body["protocolVersion"] = json!(8);
        p.body["deviceId"] = json!("other");
        assert!(validate_identity(&p, &cfg).is_err());
    }
}
