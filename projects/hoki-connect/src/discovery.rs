//! User-triggered, bounded LAN discovery. Advertisements are never pairing trust.
use super::*;
use std::{
    collections::BTreeMap,
    net::{TcpListener, UdpSocket},
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    thread,
};

enum Decision {
    Consent(bool),
    Enroll,
}

#[derive(Clone, Serialize)]
pub(super) struct Candidate {
    pub peer_id: String,
    pub name: String,
    pub address: SocketAddr,
    pub verification_key: String,
    pub token: String,
}
#[derive(Default)]
struct State {
    scanning: bool,
    enrolling: bool,
    busy_token: String,
    candidates: BTreeMap<String, Candidate>,
    expires: Option<Instant>,
    error: String,
    enrolled: String,
    approvals: BTreeMap<String, mpsc::SyncSender<Decision>>,
    generation: u64,
    claims: BTreeMap<String, String>,
    transports: BTreeMap<String, SslStream<TcpStream>>,
}
#[derive(Default, Clone)]
pub(super) struct Discovery(Arc<Mutex<State>>);

fn candidate(data: &[u8], sender: SocketAddr) -> Result<Candidate> {
    ensure!(data.len() <= 8192, "Advertisement too large");
    // Only the packet's source address is used, never an advertised host.
    ensure!(
        !sender.ip().is_unspecified() && !sender.ip().is_multicast(),
        "Invalid source"
    );
    let p = decode(data)?;
    ensure!(
        p.kind == "kdeconnect.identity" && p.body["protocolVersion"] == 8,
        "Unsupported identity"
    );
    let id = p.body["deviceId"].as_str().context("Missing device ID")?;
    let port = p.body["tcpPort"].as_u64().context("Missing port")?;
    ensure!((1714..=1764).contains(&port), "Invalid port");
    let address = SocketAddr::new(sender.ip(), port as u16);
    let cfg = Config {
        peer: address,
        peer_id: id.into(),
        fingerprint: "0".repeat(64),
    };
    peers::validate_config(&cfg)?;
    let name = p.body["deviceName"]
        .as_str()
        .unwrap_or("Companion")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    Ok(Candidate {
        peer_id: id.into(),
        name,
        address,
        verification_key: String::new(),
        token: String::new(),
    })
}

impl Discovery {
    pub fn take_transport(&self, id: &str) -> Option<SslStream<TcpStream>> {
        self.0.lock().unwrap().transports.remove(id)
    }
    pub fn snapshot(&self) -> Value {
        let state = self.0.lock().unwrap();
        let candidates: Vec<_> = state
            .candidates
            .values()
            .filter(|c| {
                !c.verification_key.is_empty() || state.expires.is_some_and(|t| Instant::now() < t)
            })
            .cloned()
            .collect();
        json!({"scanning":state.scanning,"enrolling":state.enrolling,
            "candidates":candidates,"error":state.error,"enrolled_peer":state.enrolled})
    }
    pub fn start(
        &self,
        excluded: Vec<String>,
        base: &Path,
        identity: &(String, X509, PKey<Private>),
    ) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(!state.enrolling, "Connecting to a device");
        if state.scanning {
            return Ok(());
        }
        ensure!(
            state.claims.is_empty(),
            "Finish the current pairing request first"
        );
        let socket = UdpSocket::bind("0.0.0.0:1716")
            .context("Cannot listen for devices on UDP port 1716")?;
        socket.set_nonblocking(true)?;
        socket.set_broadcast(true)?;
        let listener = (1716..=1764)
            .find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok())
            .context("No free KDE Connect TCP port")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let identity = identity.clone();
        let base = base.to_owned();
        let generation = state.generation.wrapping_add(1);
        let transports = std::mem::take(&mut state.transports);
        *state = State {
            generation,
            transports,
            scanning: true,
            expires: Some(Instant::now() + Duration::from_secs(90)),
            ..State::default()
        };
        let shared = self.0.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut buf = [0; 8193];
            let mut announced = false;
            let active = Arc::new(AtomicUsize::new(0));
            while Instant::now() < deadline {
                if !announced {
                    let mut hello = identity_packet(&identity.0);
                    hello.body["tcpPort"] = json!(port);
                    if let Ok(mut bytes) = serde_json::to_vec(&hello) {
                        bytes.push(b'\n');
                        let _ = socket.send_to(&bytes, "255.255.255.255:1716");
                    }
                    announced = true;
                }
                if let Ok((tcp, _)) = listener.accept() {
                    // Bound concurrent handshakes, without exhausting the search window.
                    if active.load(Ordering::Relaxed) < 16 {
                        active.fetch_add(1, Ordering::Relaxed);
                        let active = active.clone();
                        let shared = shared.clone();
                        let identity = identity.clone();
                        let excluded = excluded.clone();
                        let base = base.clone();
                        thread::spawn(move || {
                            if let Err(e) = incoming(
                                tcp, &base, &identity, &excluded, &shared, deadline, generation,
                            ) {
                                log("discovery-connection-ended", e.to_string());
                            }
                            active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
                match socket.recv_from(&mut buf) {
                    Ok((n, sender)) => {
                        if let Ok(c) = candidate(&buf[..n], sender) {
                            let mut state = shared.lock().unwrap();
                            let configured = peers::configs(&base)
                                .map(|peers| peers.iter().any(|(cfg, _)| cfg.peer_id == c.peer_id))
                                .unwrap_or(true);
                            if state.candidates.len() < 16
                                && !excluded.contains(&c.peer_id)
                                && !configured
                            {
                                // Preserve consent while refreshing the reconnect endpoint.
                                state
                                    .candidates
                                    .entry(c.peer_id.clone())
                                    .and_modify(|old| {
                                        if old.token.is_empty()
                                            || old.address.ip() == c.address.ip()
                                        {
                                            old.address = c.address;
                                        }
                                    })
                                    .or_insert(c);
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) => {
                        shared.lock().unwrap().error = e.to_string();
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
            shared.lock().unwrap().scanning = false;
        });
        Ok(())
    }
    pub fn approve(&self, token: &str, accept: bool) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(!state.enrolling, "Pairing decision is already pending");
        state
            .approvals
            .get(token)
            .context("Pairing request expired")?
            .try_send(Decision::Consent(accept))
            .context("Pairing approval already pending")?;
        state.enrolling = true;
        state.busy_token = token.into();
        Ok(())
    }
    pub fn enroll(
        &self,
        base: &Path,
        id: &str,
        identity: &(String, X509, PKey<Private>),
    ) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(!state.enrolling, "Already connecting");
        ensure!(
            state.expires.is_some_and(|t| Instant::now() < t),
            "Search expired; find devices again"
        );
        let candidate = state
            .candidates
            .get(id)
            .context("Unknown discovery candidate")?
            .clone();
        if !candidate.token.is_empty() {
            state
                .approvals
                .get(&candidate.token)
                .context("Connection is still opening")?
                .try_send(Decision::Enroll)
                .context("Connection is busy")?;
            state.enrolling = true;
            state.busy_token = candidate.token.clone();
            return Ok(());
        }
        ensure!(
            !state.claims.contains_key(id),
            "Connection is still opening"
        );
        state.claims.insert(id.into(), "outgoing".into());
        state.enrolling = true;
        state.busy_token = candidate.peer_id.clone();
        state.error.clear();
        state.enrolled.clear();
        let shared = self.0.clone();
        let base = base.to_owned();
        let (watch_id, cert, key) = identity.clone();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let cfg = Config {
                    peer: candidate.address,
                    peer_id: candidate.peer_id.clone(),
                    fingerprint: String::new(),
                };
                let (tls, peer) = handshake(&cfg, &watch_id, &cert, &key, None)?;
                let pin = fingerprint(
                    &tls.ssl()
                        .peer_certificate()
                        .context("Missing certificate")?,
                )?;
                // Persist an unpaired pin only after explicit local selection. Pairing
                // still requires a second tap and verification/approval on the phone.
                let cfg = Config {
                    fingerprint: pin,
                    ..cfg
                };
                {
                    let mut state = shared.lock().unwrap();
                    let destination = peers::save_config(&base, &cfg, false)?;
                    private_write(&destination.join("peer.json"), &serde_json::to_vec(&peer)?)?;
                    state.transports.insert(candidate.peer_id.clone(), tls);
                }
                ensure!(
                    peers::request(&base, "reload")? == "ok",
                    "Could not load device"
                );
                let reply = peers::request(&base, &format!("select:{}", candidate.peer_id))?;
                ensure!(reply == "queued", "Could not select new device");
                Ok(())
            })();
            let mut state = shared.lock().unwrap();
            state.claims.remove(&candidate.peer_id);
            if state.busy_token == candidate.peer_id {
                state.enrolling = false;
                state.busy_token.clear();
            }
            match result {
                Ok(()) => state.enrolled = candidate.peer_id,
                Err(e) => state.error = e.to_string(),
            }
        });
        Ok(())
    }
}

struct Claim {
    shared: Arc<Mutex<State>>,
    id: String,
    token: String,
    generation: u64,
}
impl Drop for Claim {
    fn drop(&mut self) {
        let mut state = self.shared.lock().unwrap();
        if state.generation == self.generation && state.claims.get(&self.id) == Some(&self.token) {
            state.claims.remove(&self.id);
            state.approvals.remove(&self.token);
            if state
                .candidates
                .get(&self.id)
                .is_some_and(|c| c.token == self.token)
            {
                state.candidates.remove(&self.id);
            }
        }
    }
}

fn incoming(
    mut tcp: TcpStream,
    base: &Path,
    identity: &(String, X509, PKey<Private>),
    excluded: &[String],
    shared: &Arc<Mutex<State>>,
    deadline: Instant,
    generation: u64,
) -> Result<()> {
    tcp.set_read_timeout(Some(Duration::from_secs(5)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
    let hello = read_packet(&mut tcp, 8192)?;
    let id = hello.body["deviceId"]
        .as_str()
        .context("Missing device ID")?
        .to_owned();
    ensure!(!excluded.contains(&id), "Already configured device");
    ensure!(
        !peers::configs(base)?.iter().any(|(c, _)| c.peer_id == id),
        "Already configured device"
    );
    let mut random = [0; 16];
    openssl::rand::rand_bytes(&mut random)?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut state = shared.lock().unwrap();
        ensure!(state.generation == generation, "Search expired");
        ensure!(!state.claims.contains_key(&id), "Device already pending");
        state.claims.insert(id.clone(), token.clone());
    }
    let _claim = Claim {
        shared: shared.clone(),
        id: id.clone(),
        token: token.clone(),
        generation,
    };
    let mut cfg = Config {
        peer: SocketAddr::new(tcp.peer_addr()?.ip(), 1716),
        peer_id: id.clone(),
        fingerprint: "0".repeat(64),
    };
    peers::validate_config(&cfg)?;
    validate_identity(&hello, &cfg)?;
    if let Some(target) = hello.body.get("targetDeviceId") {
        ensure!(target == &identity.0, "Wrong target device");
    }
    if let Some(version) = hello.body.get("targetProtocolVersion") {
        // Android's UDP response encodes this optional hint as a JSON string.
        ensure!(version == 8 || version == "8", "Wrong target protocol");
    }
    let mut context = SslContextBuilder::new(SslMethod::tls_client())?;
    context.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    context.set_certificate(&identity.1)?;
    context.set_private_key(&identity.2)?;
    context.set_verify_callback(SslVerifyMode::PEER, |_, c| c.error_depth() == 0);
    let mut tls = SslStream::new(Ssl::new(&context.build())?, tcp)?;
    tls.connect()?;
    let cert = tls
        .ssl()
        .peer_certificate()
        .context("Missing peer certificate")?;
    cfg.fingerprint = fingerprint(&cert)?;
    validate_cert(&cert, &cfg)?;
    send(&mut tls, &identity_packet(&identity.0))?;
    let secure = read_packet(&mut tls, 8192)?;
    validate_identity(&secure, &cfg)?;
    if let Some(port) = secure.body["tcpPort"]
        .as_u64()
        .or_else(|| hello.body["tcpPort"].as_u64())
    {
        ensure!((1714..=1764).contains(&port), "Invalid peer port");
        cfg.peer.set_port(port as u16);
    }
    ensure!(Instant::now() < deadline, "Discoverable mode ended");
    // UDP advertisements supply the reconnect port. Android normally listens on 1716.
    {
        let state = shared.lock().unwrap();
        if let Some(c) = state
            .candidates
            .get(&id)
            .filter(|c| c.address.ip() == cfg.peer.ip())
        {
            cfg.peer = c.address;
        }
    }
    let name: String = secure.body["deviceName"]
        .as_str()
        .unwrap_or("Companion")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    tls.get_ref()
        .set_read_timeout(Some(Duration::from_millis(250)))?;
    let (tx, rx) = mpsc::sync_channel::<Decision>(1);
    {
        let mut state = shared.lock().unwrap();
        ensure!(state.generation == generation, "Search expired");
        ensure!(
            state.candidates.len() < 16 || state.candidates.contains_key(&id),
            "Too many candidates"
        );
        ensure!(
            !state
                .candidates
                .get(&id)
                .is_some_and(|c| !c.token.is_empty()),
            "Device already pending"
        );
        state.candidates.insert(
            id.clone(),
            Candidate {
                peer_id: id.clone(),
                name: name.clone(),
                address: cfg.peer,
                verification_key: String::new(),
                token: token.clone(),
            },
        );
        state.approvals.insert(token.clone(), tx.clone());
    }
    let result = (|| -> Result<()> {
        let mut pending: Option<Instant> = None;
        let mut data = Vec::new();
        let mut packets = 0;
        while pending.is_some() || Instant::now() < deadline {
            if pending.is_some_and(|t| t.elapsed() >= PAIR_TIMEOUT) {
                bail!("Pairing request expired");
            }
            if let Ok(decision) = rx.try_recv() {
                let accept = match decision {
                    Decision::Consent(accept) => {
                        ensure!(pending.is_some(), "No pairing request");
                        accept
                    }
                    Decision::Enroll => {
                        if pending.is_some() {
                            let mut state = shared.lock().unwrap();
                            if state.busy_token == token {
                                state.enrolling = false;
                                state.busy_token.clear();
                            }
                            continue;
                        }
                        true
                    }
                };
                if !accept {
                    send(
                        &mut tls,
                        &Packet::new("kdeconnect.pair", json!({"pair":false})),
                    )?;
                    bail!("Pairing rejected locally");
                }
                // Local consent is tied to this connection's random token and displayed key.
                {
                    // Hold publication until config, trust and the live transport agree.
                    let mut state = shared.lock().unwrap();
                    ensure!(state.generation == generation, "Search expired");
                    if let Some(c) = state.candidates.get(&id) {
                        cfg.peer = c.address;
                    }
                    let destination = peers::save_config(base, &cfg, false)?;
                    private_write(
                        &destination.join("peer.json"),
                        &serde_json::to_vec(
                            &json!({"name":name,"type":secure.body["deviceType"]}),
                        )?,
                    )?;
                    if pending.is_some() {
                        let trust = destination.join("paired.json");
                        private_write(
                            &trust,
                            &serde_json::to_vec(
                                &json!({"peer_id":id,"fingerprint":cfg.fingerprint}),
                            )?,
                        )?;
                        if let Err(error) = send(
                            &mut tls,
                            &Packet::new("kdeconnect.pair", json!({"pair":true})),
                        ) {
                            fs::remove_file(&trust)?;
                            return Err(error);
                        }
                    }
                    state.transports.insert(id.clone(), tls);
                }
                ensure!(
                    peers::request(base, "reload")? == "ok",
                    "Could not load device"
                );
                ensure!(
                    peers::request(base, &format!("select:{id}"))? == "queued",
                    "Could not select device"
                );
                shared.lock().unwrap().enrolled = id.clone();
                return Ok(());
            }
            let mut byte = [0];
            match tls.read(&mut byte) {
                Ok(0) => bail!("Device disconnected"),
                Ok(_) if byte[0] != b'\n' => {
                    ensure!(data.len() < 8192, "Pairing packet too large");
                    data.push(byte[0]);
                }
                Ok(_) => {
                    let p = decode(&data)?;
                    data.clear();
                    packets += 1;
                    ensure!(packets <= 64, "Too many pre-pairing packets");
                    if p.kind == "kdeconnect.pair" {
                        ensure!(p.body["pair"] == true, "Pairing cancelled");
                        if pending.is_some() {
                            continue;
                        }
                        let timestamp = p.body["timestamp"]
                            .as_u64()
                            .context("Missing pairing timestamp")?;
                        ensure!(
                            (now_ms() / 1000).abs_diff(timestamp) <= 1800,
                            "Device clocks differ"
                        );
                        let key = verification_key(&identity.1, &cert, timestamp)?;
                        pending = Some(Instant::now());
                        let mut state = shared.lock().unwrap();
                        ensure!(state.generation == generation, "Search expired");
                        state.error.clear();
                        state
                            .candidates
                            .get_mut(&id)
                            .filter(|c| c.token == token)
                            .context("Candidate expired")?
                            .verification_key = key;
                        state.approvals.insert(token.clone(), tx.clone());
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
        bail!("Discoverable mode ended")
    })();
    let mut state = shared.lock().unwrap();
    state.approvals.remove(&token);
    if state.candidates.get(&id).is_some_and(|c| c.token == token) {
        state.candidates.remove(&id);
    }
    if state.generation == generation {
        if state.busy_token == token {
            state.enrolling = false;
            state.busy_token.clear();
        }
        if let Err(ref error) = result {
            state.error = error.to_string();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn incoming_pairing_requires_connection_bound_local_consent() {
        for (approve, enrollment) in [
            (Some(false), false),
            (Some(true), false),
            (None, false),
            (Some(true), true),
        ] {
            let accepted = approve == Some(true);
            let base = std::env::temp_dir().join(format!(
                "hoki-discovery-{}-{approve:?}-{enrollment}",
                std::process::id()
            ));
            fs::create_dir_all(&base).unwrap();
            let remote_base = base.join("remote");
            fs::create_dir_all(&remote_base).unwrap();
            let watch = identity(&base).unwrap();
            let remote = identity(&remote_base).unwrap();
            let watch_cert = watch.1.clone();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let discovery = Discovery::default();
            discovery.0.lock().unwrap().expires = Some(Instant::now() + Duration::from_secs(90));
            let shared = discovery.0.clone();
            let incoming_base = base.clone();
            let server = thread::spawn(move || {
                let (tcp, _) = listener.accept().unwrap();
                incoming(
                    tcp,
                    &incoming_base,
                    &watch,
                    &[],
                    &shared,
                    Instant::now() + Duration::from_secs(3),
                    0,
                )
            });
            let mut tcp = TcpStream::connect(addr).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut hello = identity_packet(&remote.0);
            hello.body["targetProtocolVersion"] = json!("8");
            send(&mut tcp, &hello).unwrap();
            let mut context = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
            context.set_certificate(&remote.1).unwrap();
            context.set_private_key(&remote.2).unwrap();
            context.set_verify_callback(
                SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT,
                |_, _| true,
            );
            let mut tls = SslStream::new(Ssl::new(&context.build()).unwrap(), tcp).unwrap();
            tls.accept().unwrap();
            send(&mut tls, &identity_packet(&remote.0)).unwrap();
            assert_eq!(
                read_packet(&mut tls, 8192).unwrap().kind,
                "kdeconnect.identity"
            );
            let timestamp = now_ms() / 1000;
            if !enrollment {
                send(
                    &mut tls,
                    &Packet::new(
                        "kdeconnect.pair",
                        json!({"pair":true,"timestamp":timestamp}),
                    ),
                )
                .unwrap();
            }
            let began = Instant::now();
            let token = loop {
                let state = discovery.0.lock().unwrap();
                if let Some(c) = state
                    .candidates
                    .get(&remote.0)
                    .filter(|c| enrollment || !c.verification_key.is_empty())
                {
                    if !enrollment {
                        assert_eq!(
                            c.verification_key,
                            verification_key(&remote.1, &watch_cert, timestamp).unwrap()
                        );
                    }
                    break c.token.clone();
                }
                drop(state);
                assert!(began.elapsed() < Duration::from_secs(4));
                thread::sleep(Duration::from_millis(10));
            };
            assert!(!base.join("config.json").exists());
            assert!(!base.join("paired.json").exists());
            assert!(discovery.approve("different-connection", true).is_err());
            let control = UnixListener::bind(base.join("control.sock")).unwrap();
            let controller = accepted.then(|| {
                thread::spawn(move || {
                    for reply in ["ok", "queued"] {
                        let (mut conn, _) = control.accept().unwrap();
                        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                        let mut data = String::new();
                        conn.read_to_string(&mut data).unwrap();
                        conn.write_all(reply.as_bytes()).unwrap();
                    }
                })
            });
            if let Some(approve) = approve {
                if accepted {
                    // Approval remains valid after discovery stops accepting new peers.
                    if !enrollment {
                        thread::sleep(Duration::from_millis(3200));
                    }
                    // A late UDP advertisement updates the eventual reconnect port.
                    discovery
                        .0
                        .lock()
                        .unwrap()
                        .candidates
                        .get_mut(&remote.0)
                        .unwrap()
                        .address
                        .set_port(1760);
                }
                if enrollment {
                    discovery
                        .enroll(&base, &remote.0, &identity(&base).unwrap())
                        .unwrap();
                } else {
                    discovery.approve(&token, approve).unwrap();
                    let response = read_packet(&mut tls, 8192).unwrap();
                    assert_eq!(response.body["pair"], approve);
                }
            }
            let result = server.join().unwrap();
            assert_eq!(result.is_ok(), accepted);
            if let Some(controller) = controller {
                controller.join().unwrap();
            }
            assert_eq!(base.join("paired.json").exists(), accepted && !enrollment);
            if accepted {
                if !enrollment {
                    let trust: Value =
                        serde_json::from_slice(&fs::read(base.join("paired.json")).unwrap())
                            .unwrap();
                    assert_eq!(trust["fingerprint"], fingerprint(&remote.1).unwrap());
                    assert_eq!(trust["peer_id"], remote.0);
                }
                let config: Config =
                    serde_json::from_slice(&fs::read(base.join("config.json")).unwrap()).unwrap();
                assert_eq!(config.peer.port(), 1760);
                let mut transport = discovery
                    .take_transport(&remote.0)
                    .expect("handoff must preserve TLS");
                send(&mut transport, &Packet::new("kdeconnect.ping", json!({}))).unwrap();
                assert_eq!(read_packet(&mut tls, 8192).unwrap().kind, "kdeconnect.ping");
            }
            assert!(discovery.approve(&token, true).is_err());
            fs::remove_dir_all(base).unwrap();
        }
    }
    #[test]
    fn advertisements_are_bounded_and_use_sender_address() {
        let mut p = identity_packet("phone_1");
        p.body["tcpPort"] = json!(1716);
        p.body["deviceName"] = json!("Pixel\nphone");
        p.body["host"] = json!("203.0.113.1");
        let sender = "127.0.0.1:50000".parse().unwrap();
        let c = candidate(&serde_json::to_vec(&p).unwrap(), sender).unwrap();
        assert_eq!(c.address.to_string(), "127.0.0.1:1716");
        assert_eq!(c.name, "Pixelphone");
        for (field, value) in [
            ("tcpPort", json!(65537)),
            ("deviceId", json!("../escape")),
            ("protocolVersion", json!(7)),
        ] {
            let mut invalid = identity_packet("phone");
            invalid.body["tcpPort"] = json!(1716);
            invalid.body[field] = value;
            assert!(candidate(&serde_json::to_vec(&invalid).unwrap(), sender).is_err());
        }
        assert!(candidate(&[b' '; 8193], sender).is_err());
    }
}
