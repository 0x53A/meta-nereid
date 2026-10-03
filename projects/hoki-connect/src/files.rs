//! Read-only SFTP, with each listener owned by one paired KDE TLS session.
use super::*;
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use std::{net::TcpListener, process::Stdio, sync::Arc, thread};
use tokio::{io::AsyncWriteExt, sync::oneshot, task::JoinHandle};

const USER: &str = "kdeconnect";

pub(super) struct Lease {
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
    pub response: Value,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn subsystem_path() -> Result<PathBuf> {
    let path = std::env::var_os("HOKI_CONNECT_SFTP_SERVER")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/libexec/sftp-server".into());
    ensure!(
        path.is_absolute() && path.is_file(),
        "Read-only SFTP subsystem is not installed"
    );
    Ok(path)
}

impl Lease {
    pub fn start(connection: &TcpStream) -> Result<Self> {
        // File access runs with the app account's permissions, never accidental root access.
        ensure!(
            unsafe { libc::geteuid() } != 0,
            "Run Connect as the watch user, not root"
        );
        let subsystem = subsystem_path()?;
        let local_ip = connection.local_addr()?.ip();
        let peer_ip = connection.peer_addr()?.ip();
        let listener = (1739..=1764)
            .find_map(|port| TcpListener::bind((local_ip, port)).ok())
            .context("No free KDE Connect file-transfer port")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let mut random = [0; 32];
        openssl::rand::rand_bytes(&mut random)?;
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let key = PKey::generate_ed25519()?;
        let pem = String::from_utf8(key.private_key_to_pem_pkcs8()?)?;
        let config = Arc::new(russh::server::Config {
            keys: vec![russh::keys::decode_secret_key(&pem, None)?],
            methods: (&[russh::MethodKind::Password][..]).into(),
            max_auth_attempts: 3,
            inactivity_timeout: Some(Duration::from_secs(120)),
            auth_rejection_time: Duration::from_secs(1),
            auth_rejection_time_initial: Some(Duration::ZERO),
            window_size: 256 * 1024,
            channel_buffer_size: 8,
            ..Default::default()
        });
        let response = json!({"user":USER,"password":password,"port":port,"path":"/",
            "multiPaths":["/"],"pathNames":["Watch files (read-only)"]});
        let password = Arc::new(password);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (stop, stopped) = oneshot::channel();
        let worker = thread::Builder::new().name("connect-sftp".into()).spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(_) => return,
                };
                let mut stopped = stopped;
                let slots = Arc::new(tokio::sync::Semaphore::new(4));
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                        incoming = listener.accept() => {
                            let Ok((socket, address)) = incoming else { break; };
                            if address.ip() != peer_ip { continue; }
                            let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                            let config = config.clone();
                            let handler = SftpSession {
                                password: password.clone(), subsystem: subsystem.clone(),
                                channel: None, used: false, task: None,
                            };
                            tasks.spawn(async move {
                                let _slot = slot;
                                if let Ok(Ok(session)) = tokio::time::timeout(Duration::from_secs(10),
                                    russh::server::run_stream(config, socket, handler)).await {
                                    let _ = session.await;
                                }
                            });
                        }
                    }
                }
                tasks.abort_all();
            });
            // Runtime drop cancels SSH sessions and their SFTP subprocess tasks.
        })?;
        Ok(Self {
            stop: Some(stop),
            worker: Some(worker),
            response,
        })
    }
}

struct SftpSession {
    password: Arc<String>,
    subsystem: PathBuf,
    channel: Option<Channel<Msg>>,
    used: bool,
    task: Option<JoinHandle<()>>,
}
impl Drop for SftpSession {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl russh::server::Handler for SftpSession {
    type Error = anyhow::Error;
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
        Ok(
            if user == USER
                && password.len() == self.password.len()
                && openssl::memcmp::eq(password.as_bytes(), self.password.as_bytes())
            {
                Auth::Accept
            } else {
                Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                }
            },
        )
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        if !self.used {
            self.used = true;
            self.channel = Some(channel);
            reply.accept().await;
        }
        Ok(())
    }
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
        session.channel_failure(channel)?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        session.channel_failure(channel)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<()> {
        if name != "sftp" || self.channel.as_ref().is_none_or(|c| c.id() != id) {
            session.channel_failure(id)?;
            return Ok(());
        }
        let mut child = tokio::process::Command::new(&self.subsystem)
            .args(["-R", "-d", "/"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child.stdin.take().context("Missing SFTP input")?;
        let mut output = child.stdout.take().context("Missing SFTP output")?;
        let channel = self.channel.take().context("Missing SSH channel")?;
        session.channel_success(id)?;
        let handle = session.handle();
        self.task = Some(tokio::spawn(async move {
            let (mut read, mut write) = tokio::io::split(channel.into_stream());
            let upload = async {
                tokio::io::copy(&mut read, &mut input).await?;
                input.shutdown().await
            };
            let download = async {
                tokio::io::copy(&mut output, &mut write).await?;
                write.shutdown().await
            };
            let _ = tokio::try_join!(upload, download);
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = handle.close(id).await;
        }));
        Ok(())
    }
    async fn channel_close(&mut self, _: ChannelId, _: &mut Session) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

    struct Client;
    impl russh::client::Handler for Client {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            _: &russh::keys::PublicKeyOrCertificate,
        ) -> std::result::Result<bool, Self::Error> {
            // This client connects only to this test's ephemeral loopback server.
            Ok(true)
        }
    }
    fn string(data: &[u8]) -> Vec<u8> {
        [(data.len() as u32).to_be_bytes().as_slice(), data].concat()
    }
    async fn exchange(stream: &mut (impl AsyncRead + AsyncWrite + Unpin), body: &[u8]) -> Vec<u8> {
        stream.write_u32(body.len() as u32).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
        let size = stream.read_u32().await.unwrap();
        assert!(size < 65536);
        let mut response = vec![0; size as usize];
        stream.read_exact(&mut response).await.unwrap();
        response
    }
    fn request(kind: u8, id: u32, fields: &[u8]) -> Vec<u8> {
        [&[kind], id.to_be_bytes().as_slice(), fields].concat()
    }

    #[test]
    fn browse_download_read_only_and_revoke() {
        let fixture = std::env::temp_dir().join(format!("hoki-sftp-{}.bin", std::process::id()));
        let contents: Vec<u8> = (0..262144).map(|i| (i % 251) as u8).collect();
        fs::write(&fixture, &contents).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (connection, _) = listener.accept().unwrap();
        let lease = Lease::start(&connection).unwrap();
        let address = format!("127.0.0.1:{}", lease.response["port"]);
        let password = lease.response["password"].as_str().unwrap().to_owned();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(20), async {
                let mut client =
                    russh::client::connect(Arc::new(Default::default()), &address, Client)
                        .await
                        .unwrap();
                assert!(!client
                    .authenticate_password(USER, "incorrect")
                    .await
                    .unwrap()
                    .success());
                drop(client);
                let mut client =
                    russh::client::connect(Arc::new(Default::default()), &address, Client)
                        .await
                        .unwrap();
                assert!(client
                    .authenticate_password(USER, password)
                    .await
                    .unwrap()
                    .success());
                let channel = client.channel_open_session().await.unwrap();
                channel.request_subsystem(true, "sftp").await.unwrap();
                let mut stream = channel.into_stream();
                let version = exchange(&mut stream, &[1, 0, 0, 0, 3]).await;
                assert_eq!(version[0], 2);
                // Browse the requested filesystem root using standard SFTP v3 operations.
                let opened = exchange(&mut stream, &request(11, 1, &string(b"/"))).await;
                assert_eq!(opened[0], 102);
                let directory = &opened[9..];
                let listing = exchange(&mut stream, &request(12, 2, &string(directory))).await;
                assert_eq!(listing[0], 104);
                let fields = [
                    string(fixture.to_str().unwrap().as_bytes()),
                    1u32.to_be_bytes().to_vec(),
                    0u32.to_be_bytes().to_vec(),
                ]
                .concat();
                let opened = exchange(&mut stream, &request(3, 3, &fields)).await;
                assert_eq!(opened[0], 102);
                let handle = &opened[9..];
                let mut downloaded = Vec::new();
                while downloaded.len() < contents.len() {
                    let fields = [
                        string(handle),
                        (downloaded.len() as u64).to_be_bytes().to_vec(),
                        32768u32.to_be_bytes().to_vec(),
                    ]
                    .concat();
                    let data = exchange(&mut stream, &request(5, 4, &fields)).await;
                    assert_eq!(data[0], 103);
                    downloaded.extend_from_slice(&data[9..]);
                }
                assert_eq!(downloaded, contents);
                // A normal write-open request is refused by OpenSSH's read-only subsystem.
                let fields = [
                    string(fixture.to_str().unwrap().as_bytes()),
                    2u32.to_be_bytes().to_vec(),
                    0u32.to_be_bytes().to_vec(),
                ]
                .concat();
                let denied = exchange(&mut stream, &request(3, 5, &fields)).await;
                assert_eq!(denied[0], 101);
                assert_eq!(u32::from_be_bytes(denied[5..9].try_into().unwrap()), 3);
                drop(lease);
                let mut byte = [0];
                let ended = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte))
                    .await
                    .unwrap();
                assert!(matches!(ended, Ok(0) | Err(_)));
                assert!(tokio::net::TcpStream::connect(&address).await.is_err());
            })
            .await
            .unwrap();
        });
        assert_eq!(fs::read(&fixture).unwrap(), contents);
        fs::remove_file(fixture).unwrap();
        drop(peer);
    }
}
