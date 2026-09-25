mod discovery;
#[path = "../../shared/transfer.rs"]
mod transfer;

use bluer::{
    gatt::{remote::CharacteristicWriteRequest, WriteOp},
    l2cap, AdapterEvent, AddressType, Device, DiscoveryFilter, DiscoveryTransport,
};
use clap::{Parser, ValueEnum};
use futures::{pin_mut, Stream, StreamExt};
use std::sync::LazyLock;
use std::time::Duration;
use std::{future::Future, io};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};
use uuid::Uuid;

static SERVICE_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"ble-ssh.asteroidwatch.dev"));
static RX_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"rx"));
static TX_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"tx"));
static ACK_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"ack"));

#[derive(Clone, Copy, ValueEnum)]
enum Transport {
    /// Classic Bluetooth L2CAP (faster, stream transport)
    Classic,
    /// BLE GATT notifications with application acknowledgments
    Ble,
    /// Try classic BT first, fall back to BLE
    Auto,
}

#[derive(Parser)]
#[command(
    name = "ble-ssh-client",
    about = "SSH over Bluetooth to AsteroidOS watch"
)]
struct Cli {
    /// Local TCP port to listen on
    #[arg(short, long, default_value_t = 2222)]
    port: u16,

    /// Filter watch by advertised name substring (default: discover by service UUID)
    #[arg(short, long)]
    name: Option<String>,

    /// Connect by MAC address (skip scan)
    #[arg(short, long)]
    addr: Option<bluer::Address>,

    /// Scan timeout in seconds
    #[arg(short, long, default_value_t = 15)]
    timeout: u64,

    /// Transport to use
    #[arg(long, value_enum, default_value_t = Transport::Auto)]
    transport: Transport,
}

#[tokio::main]
async fn main() -> bluer::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    log::info!(
        "Using adapter {} ({})",
        adapter.name(),
        adapter.address().await?
    );

    // Find the watch
    let device = if let Some(addr) = cli.addr {
        adapter.device(addr)?
    } else {
        find_watch(&adapter, cli.name.as_deref(), cli.timeout).await?
    };

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, cli.port)).await?;
    loop {
        log::info!("Waiting on 127.0.0.1:{}", cli.port);
        let (mut tcp, _) = listener.accept().await?;
        // Open Bluetooth only after TCP accept: sshd must not time out while
        // we wait for the user. A session failure leaves the listener alive.
        let result = match cli.transport {
            Transport::Ble => ble_session(&device, &mut tcp).await,
            Transport::Classic => match connect_l2cap(&device).await {
                Ok(stream) => l2cap_session(stream, &mut tcp).await,
                Err(error) => Err(error),
            },
            Transport::Auto => match connect_l2cap(&device).await {
                Ok(stream) => l2cap_session(stream, &mut tcp).await,
                Err(error) => {
                    // No TCP bytes have been consumed, so the same client
                    // can continue transparently through the BLE fallback.
                    log::warn!("L2CAP failed: {error}, falling back to BLE...");
                    ble_session(&device, &mut tcp).await
                }
            },
        };
        if let Err(error) = result {
            log::warn!("Session ended: {error}");
        }
    }
}

async fn connect_l2cap(device: &Device) -> bluer::Result<l2cap::Stream> {
    let addr = l2cap::SocketAddr::new(device.address(), AddressType::BrEdr, 0x1001);
    timeout(Duration::from_secs(15), async {
        let stream = l2cap::Stream::connect(addr).await?;
        // bluer can observe cached writability from before connect(). SO_ERROR
        // alone then says nothing about completion. Unlike getpeername(),
        // L2CAP_CONNINFO requires BT_CONNECTED. Keep this wait inside the
        // overall connection deadline, before performing any stream I/O.
        wait_l2cap_connected(|| stream.as_ref().conn_info().map(|_| ())).await?;
        // Verify that the remote SSH server is reachable before consuming
        // any local TCP bytes. Some Bluetooth connection failures surface
        // only on the first I/O; Auto must still be able to fall back then.
        let mut greeting = [0; 1];
        if stream.peek(&mut greeting).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "L2CAP closed before the SSH greeting",
            ));
        }
        Ok::<_, io::Error>(stream)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "L2CAP connection timed out"))?
    .map_err(Into::into)
}

// The caller's connection timeout bounds retries; a dropped future owns no task.
async fn wait_l2cap_connected(mut check: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match check() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn l2cap_session(
    mut stream: l2cap::Stream,
    tcp: &mut tokio::net::TcpStream,
) -> bluer::Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp.split();
    let (mut bt_read, mut bt_write) = stream.split();
    transfer::session(
        async {
            tokio::io::copy(&mut tcp_read, &mut bt_write)
                .await
                .map(|_| ())
        },
        async {
            tokio::io::copy(&mut bt_read, &mut tcp_write)
                .await
                .map(|_| ())
        },
    )
    .await?;
    Ok(())
}

async fn ble_session(device: &Device, tcp: &mut tokio::net::TcpStream) -> bluer::Result<()> {
    let connection = discovery::Discovery::new().map_err(io::Error::other)?;
    let (disconnect_tx, mut disconnects) = connection
        .monitor_disconnect(device)
        .await
        .map_err(io::Error::other)?;
    let explicit_le = connection.connect_le(device, *SERVICE_UUID).await?;
    if !explicit_le {
        log::warn!("BlueZ explicit LE APIs unavailable; using generic Connect. For reliable dual-mode connections enable host bluetoothd experimental APIs.");
        connect_device(device).await.map_err(|error| bluer::Error {
            kind: error.kind,
            message: format!("{error_message}; host BlueZ cannot explicitly select LE: enable bluetoothd experimental APIs for dual-mode watches", error_message = error.message),
        })?;
    }
    discovery::establish_baseline(&mut disconnects, async {
        if !device.is_connected().await? {
            return Ok(false);
        }
        // Explicit establishment already awaited live GATT; revalidate it after
        // clearing old teardown events. Generic fallback still resolves below.
        if explicit_le {
            return connection
                .services(device, *SERVICE_UUID)
                .await
                .map(|ids| !ids.is_empty())
                .map_err(|error| io::Error::other(error).into());
        }
        Ok(true)
    })
    .await?;
    let disconnected = async {
        tokio::select! {
            _ = disconnects.recv() => (),
            _ = connection.closed() => (),
        }
        Err(bluer::Error {
            kind: bluer::ErrorKind::NotFound,
            message: "watch connection or GATT service became unavailable".into(),
        })
    };
    let forwarding = async {
        let request = CharacteristicWriteRequest {
            op_type: WriteOp::Request,
            ..Default::default()
        };
        // BlueZ may still expose old handles briefly after daemon recovery.
        // Retry only definite stale-object/handle errors before START succeeds;
        // after reservation we never replay setup or consume TCP speculatively.
        let (tx_char, rx_char, ack_char, payload) = retry_stale_setup(|| async {
            let (tx, rx, ack) = find_characteristics(device).await?;
            connection.monitor_gatt_service(device, rx.service_id(), disconnect_tx.clone())
                .await.map_err(io::Error::other)?;
            // Close the subscribe/snapshot race before reserving the daemon.
            if !connection.services(device, *SERVICE_UUID).await.map_err(io::Error::other)?
                .contains(&rx.service_id()) {
                return Err(bluer::Error { kind: bluer::ErrorKind::NotFound,
                    message: "GATT service disappeared before START".into() });
            }
            // bluer already subtracts conservative ATT overhead.
            let payload = rx.mtu().await?
                .saturating_sub(transfer::FRAME_HEADER)
                .min(512 - transfer::FRAME_HEADER);
            if payload == 0 {
                return Err(bluer::Error {
                    kind: bluer::ErrorKind::Failed,
                    message: "BLE MTU cannot fit the transport frame header".into(),
                });
            }
            log::info!("BLE reserving START: service={:04x} RX={:04x}, data payload={payload}", rx.service_id(), rx.id());
            reserve_ble(|| async {
                rx.write_ext(&transfer::encode_frame(0, b""), &request).await
            }).await.map_err(|error| {
                log::warn!("BLE START reservation failed on service={:04x} RX={:04x}: {error}", rx.service_id(), rx.id());
                error
            })?;
            log::info!("BLE START reservation accepted");
            Ok((tx, rx, ack, payload))
        }).await.map_err(|error| {
            if explicit_le { error } else { bluer::Error {
                kind: error.kind,
                message: format!("{}; enable host bluetoothd experimental APIs to select LE independently of classic Bluetooth", error.message),
            } }
        })?;
        // AcquireNotify buffers complete frames in a socket before its reply.
        // bluer::notify instead registers its Value consumer after StartNotify,
        // which could lose the greeting now that START already reserved us.
        let reader = tx_char.notify_io().await.map_err(|error| bluer::Error {
            kind: error.kind,
            message: format!(
                "BLE AcquireNotify failed on service={:04x} TX={:04x}: {}",
                tx_char.service_id(),
                tx_char.id(),
                error.message
            ),
        })?;
        log::info!(
            "BLE notification socket acquired: service={:04x} TX={:04x}, MTU={}",
            tx_char.service_id(),
            tx_char.id(),
            reader.mtu()
        );
        // AcquireNotify can reply before the daemon consumes its new writer.
        // Its first SSH frame proves admission is active before we read TCP.
        let first = first_notification(reader.recv()).await?;
        let notifications = futures::stream::once(async move { first }).chain(
            futures::stream::unfold(reader, |reader| async move {
                match reader.recv().await {
                    Ok(frame) if !frame.is_empty() => Some((frame, reader)),
                    Ok(_) => None,
                    Err(error) => {
                        log::warn!("BLE notification receive failed: {error}");
                        None
                    }
                }
            }),
        );
        pin_mut!(notifications);
        let (mut tcp_read, mut tcp_write) = tcp.split();
        let outgoing = async {
            send_packets(&mut tcp_read, payload, |frame| {
                let rx_char = &rx_char;
                let request = &request;
                async move {
                    rx_char
                        .write_ext(&frame, request)
                        .await
                        .map_err(io::Error::other)
                }
            })
            .await
        };
        let ack_char = &ack_char;
        let incoming = receive_packets(&mut notifications, &mut tcp_write, |id| async move {
            ack_char
                // Commands must not queue behind a slow RX WriteRequest:
                // that request may itself need SSH output to keep flowing.
                .write_ext(
                    &id.to_le_bytes(),
                    &CharacteristicWriteRequest {
                        op_type: WriteOp::Command,
                        ..Default::default()
                    },
                )
                .await
                .map_err(io::Error::other)
        });
        transfer::session(outgoing, incoming).await?;
        Ok(())
    };
    tokio::select! {
        result = forwarding => result,
        result = disconnected => result,
    }
}

fn stale_setup(error: &bluer::Error) -> bool {
    error.kind == bluer::ErrorKind::NotFound
        || (error.kind == bluer::ErrorKind::Failed
            && error.message == "Operation failed with ATT error: 0x01")
}

async fn retry_stale_setup<T, F, Fut>(mut setup: F) -> bluer::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bluer::Result<T>>,
{
    timeout(Duration::from_secs(10), async {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match setup().await {
                Ok(value) => return Ok(value),
                Err(error) if stale_setup(&error) => {
                    log::warn!(
                        "GATT setup attempt {attempt} used stale handles; rediscovering: {error}"
                    );
                    sleep(Duration::from_millis(200)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "BLE characteristic rediscovery timed out",
        )
    })?
}

async fn first_notification(
    receive: impl Future<Output = io::Result<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    let frame = timeout(transfer::DELIVERY_TIMEOUT, receive)
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "BLE greeting timed out before session admission",
            )
        })??;
    if frame.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BLE closed before session admission",
        ));
    }
    Ok(frame)
}

fn start_busy(error: &bluer::Error) -> bool {
    error.kind == bluer::ErrorKind::InProgress
        || (error.kind == bluer::ErrorKind::Failed
            && error.message == "Operation failed with ATT error: 0xfe")
}

async fn reserve_ble<F, Fut>(mut start: F) -> bluer::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bluer::Result<()>>,
{
    let mut busy_deadline = None;
    loop {
        // The busy budget limits retries, not an in-flight START RPC. BlueZ
        // can take longer while refreshing its GATT cache after recovery.
        if busy_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "BLE reservation remained busy for two seconds",
            )
            .into());
        }
        let result = timeout(transfer::DELIVERY_TIMEOUT, start())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "BLE START RPC timed out"))?;
        match result {
            Ok(()) => return Ok(()),
            Err(error) if start_busy(&error) => {
                let deadline = *busy_deadline
                    .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(2));
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
                )
                .await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{duplex, AsyncReadExt};

    #[tokio::test(start_paused = true)]
    async fn stale_setup_rediscovers_without_retrying_terminal_errors() {
        let mut attempts = 0;
        let result = retry_stale_setup(|| {
            attempts += 1;
            let result = match attempts {
                1 => Err(bluer::Error {
                    kind: bluer::ErrorKind::NotFound,
                    message: "removed".into(),
                }),
                2 => Err(bluer::Error {
                    kind: bluer::ErrorKind::Failed,
                    message: "Operation failed with ATT error: 0x01".into(),
                }),
                _ => Ok(7),
            };
            async move { result }
        })
        .await
        .unwrap();
        assert_eq!((result, attempts), (7, 3));
        for error in [
            bluer::Error {
                kind: bluer::ErrorKind::Failed,
                message: "Operation failed with ATT error: 0x0e".into(),
            },
            bluer::Error {
                kind: bluer::ErrorKind::InProgress,
                message: "busy".into(),
            },
        ] {
            let mut attempts = 0;
            let result: bluer::Result<()> = retry_stale_setup(|| {
                attempts += 1;
                let error = error.clone();
                async move { Err(error) }
            })
            .await;
            assert!(result.is_err());
            assert_eq!(attempts, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stale_setup_has_ten_second_deadline() {
        let start = tokio::time::Instant::now();
        let result: bluer::Result<()> = retry_stale_setup(|| async {
            Err(bluer::Error {
                kind: bluer::ErrorKind::NotFound,
                message: "removed".into(),
            })
        })
        .await;
        assert!(result.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn greeting_barrier_rejects_closed_or_stalled_notification_socket() {
        assert_eq!(
            first_notification(async { Ok(Vec::new()) })
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            first_notification(std::future::pending())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test]
    async fn buffered_greeting_is_forwarded_and_acknowledged_exactly_once() {
        let first = first_notification(async { Ok(transfer::encode_frame(42, b"SSH-greeting")) })
            .await
            .unwrap();
        let mut packets = futures::stream::iter([first, transfer::encode_frame(43, b"")]);
        let mut output = Vec::new();
        let mut acks = Vec::new();
        receive_packets(&mut packets, &mut output, |id| {
            acks.push(id);
            async { Ok(()) }
        })
        .await
        .unwrap();
        assert_eq!(output, b"SSH-greeting");
        assert_eq!(acks, [42, 43]);
    }

    #[tokio::test(start_paused = true)]
    async fn start_reservation_retries_only_busy_until_slot_available() {
        let mut attempts = 0;
        reserve_ble(|| {
            attempts += 1;
            let result = if attempts < 3 {
                Err(bluer::Error {
                    kind: bluer::ErrorKind::Failed,
                    message: "Operation failed with ATT error: 0xfe".into(),
                })
            } else {
                Ok(())
            };
            async move { result }
        })
        .await
        .unwrap();
        assert_eq!(attempts, 3);
        let mut attempts = 0;
        let result = reserve_ble(|| {
            attempts += 1;
            async {
                Err(bluer::Error {
                    kind: bluer::ErrorKind::Failed,
                    message: "Operation failed with ATT error: 0x0e".into(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn busy_reservation_has_two_second_deadline() {
        let start = tokio::time::Instant::now();
        let result = reserve_ble(|| async {
            Err(bluer::Error {
                kind: bluer::ErrorKind::InProgress,
                message: "busy".into(),
            })
        })
        .await;
        assert!(result.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn slow_successful_start_is_not_canceled_by_busy_retry_budget() {
        for initially_busy in [false, true] {
            let mut attempts = 0;
            reserve_ble(|| {
                attempts += 1;
                let busy = initially_busy && attempts == 1;
                async move {
                    if busy {
                        Err(bluer::Error {
                            kind: bluer::ErrorKind::InProgress,
                            message: "busy".into(),
                        })
                    } else {
                        sleep(Duration::from_secs(3)).await;
                        Ok(())
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(attempts, if initially_busy { 2 } else { 1 });
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_start_rpc_retains_its_own_deadline() {
        let began = tokio::time::Instant::now();
        assert!(reserve_ble(|| std::future::pending()).await.is_err());
        assert_eq!(began.elapsed(), transfer::DELIVERY_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn classic_waits_for_connected_state_before_io() {
        let start = tokio::time::Instant::now();
        let mut attempts = 0;
        wait_l2cap_connected(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::ErrorKind::NotConnected.into())
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        assert_eq!(attempts, 3);
        assert_eq!(start.elapsed(), Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn classic_readiness_respects_connection_deadline() {
        let mut attempts = 0;
        let result = timeout(
            Duration::from_millis(60),
            wait_l2cap_connected(|| {
                attempts += 1;
                Err(io::ErrorKind::NotConnected.into())
            }),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn classic_readiness_propagates_fatal_error_without_retry() {
        let mut attempts = 0;
        let error = wait_l2cap_connected(|| {
            attempts += 1;
            Err(io::ErrorKind::ConnectionRefused.into())
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        assert_eq!(attempts, 1);
    }

    #[test]
    fn default_discovery_uses_service_uuid_regardless_of_advertised_name() {
        let cli = Cli::try_parse_from(["ble-ssh-client"]).unwrap();
        assert!(cli.name.is_none());
        for name in ["AsteroidOS-SSH", "hoki", "", "Custom Watch"] {
            assert!(matches_watch(cli.name.as_deref(), name, true));
            assert!(!matches_watch(cli.name.as_deref(), name, false));
        }
    }

    #[test]
    fn explicit_name_filter_remains_case_insensitive_and_exclusive() {
        assert!(matches_watch(Some("hoki"), "Hoki Watch", false));
        assert!(!matches_watch(Some("hoki"), "AsteroidOS-SSH", true));
        assert!(!matches_watch(Some("hoki"), "", true));
    }

    #[tokio::test]
    async fn rx_frames_preserve_data_and_send_sequenced_eof() {
        let bytes = b"abcdefghijklmnop";
        let mut packets = Vec::new();
        send_packets(&mut &bytes[..], 5, |packet| {
            packets.push(packet);
            async { Ok(()) }
        })
        .await
        .unwrap();
        let mut received = Vec::new();
        for (index, frame) in packets.iter().enumerate() {
            let (id, payload) = transfer::decode_frame(frame).unwrap();
            assert_eq!(id, index as u64 + 1);
            assert!(payload.len() <= 5);
            assert_eq!(payload.is_empty(), index == packets.len() - 1);
            received.extend_from_slice(payload);
        }
        assert_eq!(received, bytes);
    }

    #[tokio::test(start_paused = true)]
    async fn rx_stalled_write_stops_without_sending_another_frame() {
        let mut count = 0;
        let result = send_packets(&mut &b"abcdef"[..], 3, |_| {
            count += 1;
            std::future::pending()
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn acknowledgements_wait_for_tcp_writes_and_include_eof() {
        let mut packets = futures::stream::iter(vec![
            transfer::encode_frame(41, b"abc"),
            transfer::encode_frame(42, b""),
        ]);
        let (mut writer, mut reader) = duplex(1);
        let ids = Arc::new(Mutex::new(Vec::new()));
        let observed = ids.clone();
        let receiving = receive_packets(&mut packets, &mut writer, move |id| {
            observed.lock().unwrap().push(id);
            async { Ok(()) }
        });
        tokio::pin!(receiving);
        assert!(timeout(Duration::from_millis(20), &mut receiving)
            .await
            .is_err());
        assert!(ids.lock().unwrap().is_empty());
        let consuming = async {
            let mut data = [0; 3];
            reader.read_exact(&mut data).await.unwrap();
            assert_eq!(&data, b"abc");
        };
        let (result, ()) = timeout(Duration::from_secs(1), async {
            tokio::join!(receiving, consuming)
        })
        .await
        .unwrap();
        result.unwrap();
        assert_eq!(*ids.lock().unwrap(), vec![41, 42]);
    }

    #[tokio::test]
    async fn malformed_frame_is_never_acknowledged() {
        let mut packets = futures::stream::iter(vec![vec![0; 7]]);
        let result = receive_packets(&mut packets, &mut tokio::io::sink(), |_| {
            panic!("malformed packet acknowledged");
            #[allow(unreachable_code)]
            async {
                Ok(())
            }
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_zero_duplicate_skipped_and_wrapped_tx_ids_before_delivery() {
        for ids in [vec![0], vec![41, 41], vec![41, 43], vec![u64::MAX, 0]] {
            let mut packets = futures::stream::iter(
                ids.iter()
                    .map(|id| transfer::encode_frame(*id, b"x"))
                    .collect::<Vec<_>>(),
            );
            let mut output = Vec::new();
            let mut acknowledged = Vec::new();
            let error = receive_packets(&mut packets, &mut output, |id| {
                acknowledged.push(id);
                async { Ok(()) }
            })
            .await
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(acknowledged, ids[..ids.len() - 1]);
            assert_eq!(output.len(), ids.len() - 1);
        }
    }

    #[tokio::test]
    async fn failed_tcp_write_is_never_acknowledged() {
        let mut packets = futures::stream::iter(vec![transfer::encode_frame(1, b"data")]);
        let (mut writer, reader) = duplex(1);
        drop(reader);
        let result = receive_packets(&mut packets, &mut writer, |_| {
            panic!("undelivered packet acknowledged");
            #[allow(unreachable_code)]
            async {
                Ok(())
            }
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
}

async fn send_packets<R, F, Fut>(reader: &mut R, payload: usize, mut send: F) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    if payload == 0 || payload > 512 - transfer::FRAME_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid BLE frame payload budget",
        ));
    }
    let mut buffer = vec![0; payload];
    let mut id = 1u64;
    loop {
        let count = reader.read(&mut buffer).await?;
        timeout(
            transfer::DELIVERY_TIMEOUT,
            send(transfer::encode_frame(id, &buffer[..count])),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "BLE RX write timed out"))??;
        if count == 0 {
            return Ok(());
        }
        id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("BLE frame sequence exhausted"))?;
    }
}

async fn receive_packets<S, W, F, Fut>(
    packets: &mut S,
    output: &mut W,
    mut ack: F,
) -> io::Result<()>
where
    S: Stream<Item = Vec<u8>> + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    let mut previous_id: Option<u64> = None;
    while let Some(packet) = packets.next().await {
        let (id, payload) = transfer::decode_frame(&packet)?;
        // The daemon's counter spans sessions, so the first ID is arbitrary
        // but nonzero. Later frames must be strictly contiguous, including EOF.
        if id == 0 || previous_id.is_some_and(|previous| previous.checked_add(1) != Some(id)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected BLE notification sequence",
            ));
        }
        output.write_all(payload).await?;
        // Application ACK after the TCP write bounds the daemon's output
        // while the local SSH client is slow.
        ack(id).await?;
        previous_id = Some(id);
        if payload.is_empty() {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "BLE notification stream ended without EOF",
    ))
}

fn matches_watch(name_filter: Option<&str>, name: &str, has_service: bool) -> bool {
    match name_filter {
        Some(filter) => !name.is_empty() && name.to_lowercase().contains(&filter.to_lowercase()),
        None => has_service,
    }
}

async fn find_watch(
    adapter: &bluer::Adapter,
    name_filter: Option<&str>,
    timeout_secs: u64,
) -> bluer::Result<Device> {
    log::info!("Scanning for AsteroidOS-SSH watch (timeout {timeout_secs}s)...");

    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Auto,
            ..Default::default()
        })
        .await?;

    // Names/service UUIDs may arrive after the initial DeviceAdded event.
    let discover = adapter.discover_devices_with_changes().await?;
    pin_mut!(discover);

    let deadline = timeout(Duration::from_secs(timeout_secs), async {
        while let Some(evt) = discover.next().await {
            if let AdapterEvent::DeviceAdded(addr) = evt {
                let device = match adapter.device(addr) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                let uuids = device.uuids().await.ok().flatten().unwrap_or_default();
                let name = device.name().await.ok().flatten().unwrap_or_default();

                if matches_watch(name_filter, &name, uuids.contains(&*SERVICE_UUID)) {
                    log::info!("Found watch: {addr} ({name})");
                    return Ok(device);
                }
            }
        }
        Err(bluer::Error {
            kind: bluer::ErrorKind::NotFound,
            message: "discovery stream ended".into(),
        })
    });

    match deadline.await {
        Ok(result) => result,
        Err(_) => {
            eprintln!("Scan timed out after {timeout_secs}s. No watch found.");
            std::process::exit(1);
        }
    }
}

async fn connect_device(device: &Device) -> bluer::Result<()> {
    if device.is_connected().await? {
        log::info!("Already connected to {}", device.address());
        return Ok(());
    }

    for attempt in 1..=3 {
        log::info!(
            "Connecting to {} (attempt {}/3)...",
            device.address(),
            attempt
        );
        match device.connect().await {
            Ok(()) => {
                log::info!("Connected");
                return Ok(());
            }
            Err(e) if attempt < 3 => {
                log::warn!("Connect failed: {e}, retrying...");
                sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

async fn find_characteristics(
    device: &Device,
) -> bluer::Result<(
    bluer::gatt::remote::Characteristic,
    bluer::gatt::remote::Characteristic,
    bluer::gatt::remote::Characteristic,
)> {
    let discovery = discovery::Discovery::new().map_err(io::Error::other)?;
    discovery::wait(|| async {
        for id in discovery
            .services(device, *SERVICE_UUID)
            .await
            .map_err(io::Error::other)?
        {
            let service = device.service(id).await?;
            let uuid = service.uuid().await?;
            if uuid != *SERVICE_UUID {
                continue;
            }

            let mut tx_char = None;
            let mut rx_char = None;
            let mut ack_char = None;

            for c in service.characteristics().await? {
                let cuuid = c.uuid().await?;
                if cuuid == *TX_UUID {
                    tx_char = Some(c);
                } else if cuuid == *RX_UUID {
                    rx_char = Some(c);
                } else if cuuid == *ACK_UUID {
                    ack_char = Some(c);
                }
            }

            if let (Some(tx), Some(rx), Some(ack)) = (tx_char, rx_char, ack_char) {
                if !tx.flags().await?.notify
                    || !rx.flags().await?.write
                    || !ack.flags().await?.write_without_response
                {
                    return Err(bluer::Error {
                        kind: bluer::ErrorKind::NotSupported,
                        message: "watch daemon lacks framed notification/request/ACK support; deploy the matching ble-ssh-watch build".into(),
                    });
                }
                log::info!("Found SSH GATT objects: service={:04x}, TX={:04x}, RX={:04x}, ACK={:04x}", service.id(), tx.id(), rx.id(), ack.id());
                return Ok(Some((tx, rx, ack)));
            }
        }

        Ok(None)
    })
    .await
}
