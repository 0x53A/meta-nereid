//! Exercise two real TLS sessions against isolated synthetic KDE Connect peers.
use openssl::{
    asn1::Asn1Time,
    hash::MessageDigest,
    pkey::{PKey, Private},
    rsa::Rsa,
    ssl::{Ssl, SslContextBuilder, SslMethod, SslVerifyMode},
    x509::{X509NameBuilder, X509},
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Sandbox {
    base: PathBuf,
    child: Option<Child>,
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}
// Cargo's target runner covers the test harness, but not its child processes.
fn connect_command() -> Command {
    if let Some(runner) = std::env::var_os("HOKI_CONNECT_TEST_QEMU") {
        let mut command = Command::new(runner);
        command.arg(env!("CARGO_BIN_EXE_hoki-connect"));
        command
    } else {
        Command::new(env!("CARGO_BIN_EXE_hoki-connect"))
    }
}
fn cli(base: &Path, args: &[&str]) -> std::process::Output {
    connect_command()
        .env("HOKI_CONNECT_STATE", base)
        .args(args)
        .output()
        .unwrap()
}
fn ok_cli(base: &Path, args: &[&str]) {
    let output = cli(base, args);
    assert!(
        output.status.success(),
        "child exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
fn request(base: &Path, command: &str) -> Value {
    let mut s = UnixStream::connect(base.join("control.sock")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    s.write_all(command.as_bytes()).unwrap();
    s.shutdown(Shutdown::Write).unwrap();
    let mut data = String::new();
    s.read_to_string(&mut data).unwrap();
    serde_json::from_str(&data).unwrap_or(Value::String(data))
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "Timed out waiting for state"
        );
        thread::sleep(Duration::from_millis(30));
    }
}
fn receive(stream: &mut impl Read) -> Value {
    let mut packet = Vec::new();
    loop {
        let mut b = [0];
        stream.read_exact(&mut b).unwrap();
        if b[0] == b'\n' {
            break;
        }
        packet.push(b[0]);
    }
    serde_json::from_slice(&packet).unwrap()
}
fn send(stream: &mut impl Write, kind: &str, body: Value) {
    writeln!(stream, "{}", json!({"id":1,"type":kind,"body":body})).unwrap();
    stream.flush().unwrap();
}
struct Peer {
    listener: TcpListener,
    cert: X509,
    key: PKey<Private>,
    id: &'static str,
}
impl Peer {
    fn new(id: &'static str) -> Self {
        let listener = (1714..=1764)
            .find_map(|p| TcpListener::bind(("127.0.0.1", p)).ok())
            .unwrap();
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", id).unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        let serial = openssl::bn::BigNum::from_u32(1)
            .unwrap()
            .to_asn1_integer()
            .unwrap();
        cert.set_serial_number(&serial).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(Asn1Time::days_from_now(0).unwrap().as_ref())
            .unwrap();
        cert.set_not_after(Asn1Time::days_from_now(1).unwrap().as_ref())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        Self {
            listener,
            cert: cert.build(),
            key,
            id,
        }
    }
    fn pin(&self) -> String {
        self.cert
            .digest(MessageDigest::sha256())
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn address(&self) -> String {
        self.listener.local_addr().unwrap().to_string()
    }
    fn run(
        self,
        commands: mpsc::Receiver<&'static str>,
        events: mpsc::Sender<Value>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            self.listener.set_nonblocking(true).unwrap();
            loop {
                let start = Instant::now();
                let (mut tcp, _) = loop {
                    if let Ok(connection) = self.listener.accept() {
                        break connection;
                    }
                    if commands.try_recv() == Ok("stop") {
                        return;
                    }
                    assert!(
                        start.elapsed() < Duration::from_secs(20),
                        "No watch connection"
                    );
                    thread::sleep(Duration::from_millis(20));
                };
                tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                let hello = receive(&mut tcp);
                assert_eq!(hello["body"]["targetDeviceId"], self.id);
                let mut context = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
                context.set_certificate(&self.cert).unwrap();
                context.set_private_key(&self.key).unwrap();
                context.set_verify(SslVerifyMode::NONE);
                let mut tls =
                    openssl::ssl::SslStream::new(Ssl::new(&context.build()).unwrap(), tcp).unwrap();
                tls.connect().unwrap();
                let watch = receive(&mut tls);
                send(
                    &mut tls,
                    "kdeconnect.identity",
                    json!({"deviceId":self.id,"deviceName":self.id,"protocolVersion":8}),
                );
                events
                    .send(json!({"event":"connected","watch":watch["body"]["deviceId"]}))
                    .unwrap();
                tls.get_ref()
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut pending = Vec::new();
                'connection: loop {
                    match commands.try_recv() {
                        Ok("stop") => return,
                        Ok("disconnect") => break 'connection,
                        Ok("battery") => send(
                            &mut tls,
                            "kdeconnect.battery",
                            json!({"currentCharge":67,"isCharging":true,"thresholdEvent":0}),
                        ),
                        Ok("share-empty") => send(
                            &mut tls,
                            "kdeconnect.share.request",
                            json!({"filename":"empty.txt"}),
                        ),
                        Ok("browse") => send(
                            &mut tls,
                            "kdeconnect.sftp.request",
                            json!({"startBrowsing":true}),
                        ),
                        Ok("pair-request") => {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                            send(
                                &mut tls,
                                "kdeconnect.pair",
                                json!({"pair":true,"timestamp":timestamp}),
                            );
                        }
                        _ => {}
                    }
                    let mut byte = [0];
                    match tls.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) if byte[0] != b'\n' => pending.push(byte[0]),
                        Ok(_) => {
                            let p: Value = serde_json::from_slice(&pending).unwrap();
                            pending.clear();
                            if p["type"] == "kdeconnect.pair"
                                && p["body"]["pair"] == true
                                && p["body"].get("timestamp").is_some()
                            {
                                send(&mut tls, "kdeconnect.pair", json!({"pair":true}));
                            }
                            if p["type"] == "kdeconnect.mpris.request"
                                && p["body"]["requestPlayerList"] == true
                            {
                                send(
                                    &mut tls,
                                    "kdeconnect.mpris",
                                    json!({"playerList":["SharedPlayer"]}),
                                );
                            }
                            if p["type"] == "kdeconnect.mpris.request"
                                && p["body"]["requestNowPlaying"] == true
                            {
                                send(
                                    &mut tls,
                                    "kdeconnect.mpris",
                                    json!({"player":"SharedPlayer","title":self.id,"volume":30,"canPlay":true}),
                                );
                            }
                            events.send(p).unwrap();
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            }
        })
    }
}
fn event(rx: &mpsc::Receiver<Value>, kind: &str) -> Value {
    let start = Instant::now();
    loop {
        let p = rx
            .recv_timeout(Duration::from_secs(10).saturating_sub(start.elapsed()))
            .unwrap();
        if p["type"] == kind || p["event"] == kind {
            return p;
        }
    }
}
#[test]
fn simultaneous_connections_preserve_trust_and_route_actions_independently() {
    let base = std::env::temp_dir().join(format!("hoki-multi-tls-{}", std::process::id()));
    fs::create_dir(&base).unwrap();
    let mut sandbox = Sandbox { base, child: None };
    let home = sandbox.base.join("home");
    let battery = sandbox.base.join("battery-source");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&battery).unwrap();
    fs::write(battery.join("capacity"), "83\n").unwrap();
    fs::write(battery.join("status"), "Discharging\n").unwrap();
    let laptop = Peer::new("laptop");
    let phone = Peer::new("phone");
    let laptop_pin = laptop.pin();
    ok_cli(
        &sandbox.base,
        &["init", &laptop.address(), "laptop", &laptop_pin],
    );
    let original_identity = fs::read(sandbox.base.join("identity.json")).unwrap();
    // Legacy paired.json stays in place, without any re-pairing or identity rewrite.
    fs::write(
        sandbox.base.join("paired.json"),
        json!({"peer_id":"laptop","fingerprint":laptop_pin}).to_string(),
    )
    .unwrap();
    let (ltx, lrx) = mpsc::channel();
    let (lex, ler) = mpsc::channel();
    let lh = laptop.run(lrx, lex);
    sandbox.child = Some(
        connect_command()
            .arg("serve")
            .env("HOKI_CONNECT_DOWNLOAD_HOME", &home)
            .env("HOKI_CONNECT_BATTERY_DIR", &battery)
            .env("HOKI_CONNECT_STATE", &sandbox.base)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| sandbox.base.join("control.sock").exists());
    let laptop_watch = event(&ler, "connected")["watch"].clone();
    wait_for(|| request(&sandbox.base, "snapshot")["status"]["paired"] == true);
    let reported = event(&ler, "kdeconnect.battery");
    assert_eq!(reported["body"]["currentCharge"], 83);
    assert_eq!(reported["body"]["isCharging"], false);
    ltx.send("battery").unwrap();
    wait_for(|| request(&sandbox.base, "snapshot")["battery"]["currentCharge"] == 67);
    ltx.send("share-empty").unwrap();
    wait_for(|| request(&sandbox.base, "snapshot")["download"]["state"] == "received");
    assert_eq!(
        fs::metadata(home.join("Download/empty.txt")).unwrap().len(),
        0
    );
    // Add while the laptop is connected; no daemon restart or dropped connection.
    let phone_address = phone.address();
    let phone_pin = phone.pin();
    let (ptx, prx) = mpsc::channel();
    let (pex, per) = mpsc::channel();
    let ph = phone.run(prx, pex);
    ok_cli(&sandbox.base, &["add", &phone_address, "phone", &phone_pin]);
    assert_eq!(event(&per, "connected")["watch"], laptop_watch);
    wait_for(|| {
        request(&sandbox.base, "snapshot")["peers"]
            .as_array()
            .is_some_and(|p| p.len() == 2 && p.iter().all(|p| p["status"]["state"] == "connected"))
    });
    assert_eq!(
        request(&sandbox.base, "snapshot")["selected_peer"],
        "laptop"
    );
    ok_cli(&sandbox.base, &["pair", "phone"]);
    event(&per, "kdeconnect.pair");
    wait_for(|| request(&sandbox.base, "snapshot")["peers"][1]["trusted"] == true);
    assert_eq!(
        original_identity,
        fs::read(sandbox.base.join("identity.json")).unwrap()
    );
    // A duplicate configuration must not replace the phone's trust anchor.
    assert!(!cli(
        &sandbox.base,
        &["add", &phone_address, "phone", &"00".repeat(32)]
    )
    .status
    .success());
    ok_cli(&sandbox.base, &["select", "phone"]);
    wait_for(|| request(&sandbox.base, "snapshot")["media"]["title"] == "phone");
    // Configured peers can initiate re-pairing, with explicit, request-bound consent.
    ptx.send("pair-request").unwrap();
    wait_for(|| {
        request(&sandbox.base, "snapshot")["status"]["pairing_token"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    });
    let state = request(&sandbox.base, "snapshot");
    let first_token = state["status"]["pairing_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        state["status"]["verification_key"].as_str().unwrap().len(),
        8
    );
    assert_eq!(state["trusted"], false);
    let reject =
        json!({"peer_id":"phone","command":format!("reject-pair:{first_token}")}).to_string();
    assert_eq!(request(&sandbox.base, &reject), "queued");
    assert_eq!(event(&per, "kdeconnect.pair")["body"]["pair"], false);
    ptx.send("pair-request").unwrap();
    wait_for(|| {
        request(&sandbox.base, "snapshot")["status"]["pairing_token"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s != first_token)
    });
    let second_token = request(&sandbox.base, "snapshot")["status"]["pairing_token"]
        .as_str()
        .unwrap()
        .to_owned();
    // A delayed tap from the previous request must not approve this one.
    let stale =
        json!({"peer_id":"phone","command":format!("accept-pair:{first_token}")}).to_string();
    assert_eq!(request(&sandbox.base, &stale), "queued");
    thread::sleep(Duration::from_millis(150));
    assert_eq!(request(&sandbox.base, "snapshot")["trusted"], false);
    let accept =
        json!({"peer_id":"phone","command":format!("accept-pair:{second_token}")}).to_string();
    assert_eq!(request(&sandbox.base, &accept), "queued");
    assert_eq!(event(&per, "kdeconnect.pair")["body"]["pair"], true);
    wait_for(|| request(&sandbox.base, "snapshot")["trusted"] == true);
    ok_cli(&sandbox.base, &["ping", "laptop"]);
    event(&ler, "kdeconnect.ping");
    assert!(!per.try_iter().any(|p| p["type"] == "kdeconnect.ping"));
    assert_eq!(
        request(
            &sandbox.base,
            r#"{"peer_id":"laptop","command":"volume-set:{\"player\":\"SharedPlayer\",\"volume\":70}"}"#
        ),
        "queued"
    );
    let start = Instant::now();
    loop {
        let p = event(&ler, "kdeconnect.mpris.request");
        if p["body"]["setVolume"] == 70 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    assert!(!per.try_iter().any(|p| p["body"]["setVolume"] == 70));
    // One connection can drop and reconnect while the other remains usable.
    ptx.send("browse").unwrap();
    let browse = event(&per, "kdeconnect.sftp");
    assert_eq!(browse["body"]["path"], "/");
    assert_eq!(browse["body"]["password"].as_str().unwrap().len(), 64);
    let first_files = format!("127.0.0.1:{}", browse["body"]["port"]);
    assert!(std::net::TcpStream::connect(&first_files).is_ok());
    ptx.send("disconnect").unwrap();
    wait_for(|| request(&sandbox.base, "snapshot")["status"]["state"] == "disconnected");
    assert!(std::net::TcpStream::connect(&first_files).is_err());
    assert_eq!(
        request(&sandbox.base, r#"{"peer_id":"phone","command":"ping"}"#),
        "offline"
    );
    ok_cli(&sandbox.base, &["ping", "laptop"]);
    event(&ler, "kdeconnect.ping");
    event(&per, "connected");
    wait_for(|| request(&sandbox.base, "snapshot")["status"]["paired"] == true);
    ptx.send("browse").unwrap();
    let browse_again = event(&per, "kdeconnect.sftp");
    assert_ne!(browse["body"]["password"], browse_again["body"]["password"]);
    let second_files = format!("127.0.0.1:{}", browse_again["body"]["port"]);
    ok_cli(&sandbox.base, &["unpair", "phone"]);
    event(&per, "kdeconnect.pair");
    wait_for(|| !sandbox.base.join("peers/phone/paired.json").exists());
    assert!(std::net::TcpStream::connect(&second_files).is_err());
    assert!(sandbox.base.join("paired.json").exists());
    assert_eq!(
        request(&sandbox.base, "snapshot")["peers"][0]["trusted"],
        true
    );
    // Restart reloads both configurations and the chosen device, retaining laptop trust.
    let child = sandbox.child.as_mut().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    sandbox.child = Some(
        connect_command()
            .arg("serve")
            .env("HOKI_CONNECT_DOWNLOAD_HOME", &home)
            .env("HOKI_CONNECT_BATTERY_DIR", &battery)
            .env("HOKI_CONNECT_STATE", &sandbox.base)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    event(&ler, "connected");
    event(&per, "connected");
    wait_for(|| request(&sandbox.base, "snapshot")["peers"][0]["status"]["paired"] == true);
    assert_eq!(request(&sandbox.base, "snapshot")["selected_peer"], "phone");
    assert_eq!(request(&sandbox.base, "snapshot")["trusted"], false);
    ltx.send("stop").unwrap();
    ptx.send("stop").unwrap();
    lh.join().unwrap();
    ph.join().unwrap();
}
