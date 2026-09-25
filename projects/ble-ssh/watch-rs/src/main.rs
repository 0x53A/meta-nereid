mod config;
mod incoming;
mod owner;
mod subscriptions;
#[path = "../../shared/transfer.rs"]
mod transfer;

use bluer::{
    adv::Advertisement,
    gatt::local::{
        characteristic_control, Application, Characteristic, CharacteristicControlEvent,
        CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicWrite,
        CharacteristicWriteMethod, Service,
    },
    l2cap, AddressType,
};
use futures::{pin_mut, FutureExt, Stream, StreamExt};
use std::collections::BTreeSet;
use std::sync::LazyLock;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::{
    net::TcpStream,
    signal::unix::{signal, SignalKind},
    sync::{mpsc, oneshot, Mutex},
};
use uuid::Uuid;

static SERVICE_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"ble-ssh.asteroidwatch.dev"));
static RX_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"rx"));
static ACK_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"ack"));
static TX_UUID: LazyLock<Uuid> = LazyLock::new(|| Uuid::new_v5(&SERVICE_UUID, b"tx"));

// Retire session work before reopening admission. Keeping the application
// registered preserves its attribute handles across clean EOF and classic failures.
async fn clear_session<C: Stream + Unpin>(
    ingress: &Mutex<incoming::Gate>,
    writes: &mut mpsc::Receiver<incoming::Write>,
    expected_ack: &Mutex<Option<(bluer::Address, u64)>>,
    acknowledgements: &Mutex<mpsc::Receiver<u64>>,
    control: &mut C,
    ble: bool,
) -> std::io::Result<()> {
    let mut gate = ingress.lock().await;
    let mut expected = expected_ack.lock().await;
    let mut acks = acknowledgements.lock().await;
    *expected = None;
    while let Ok(request) = writes.try_recv() {
        let _ = request
            .complete
            .send(Err(bluer::gatt::local::ReqError::InProgress));
    }
    while acks.try_recv().is_ok() {}
    if ble {
        loop {
            match control.next().now_or_never() {
                Some(Some(event)) => drop(event),
                Some(None) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "GATT control ended",
                    ))
                }
                None => break,
            }
        }
        if writes.is_closed() || acks.is_closed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "GATT request channel closed",
            ));
        }
    }
    *gate = incoming::Gate::Idle;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> bluer::Result<()> {
    env_logger::init();

    let config = config::Config::from_env()?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::select! {
        result = supervise(&config) => result,
        _ = sigterm.recv() => Ok(()),
        _ = sigint.recv() => Ok(()),
    }
}

// ConnMan owns radio power. Re-register after power cycles, adapter replacement,
// or BlueZ restart; dropping serve() also closes active proxy sessions.
async fn supervise(config: &config::Config) -> bluer::Result<()> {
    loop {
        // Watch the well-known name before any BlueZ initialization; even a
        // loss/reacquire between two adapter reads invalidates registrations.
        let result = async {
            let mut monitor = owner::BluezOwnerMonitor::new()
                .await
                .map_err(std::io::Error::other)?;
            tokio::select! {
                result = run_when_powered(config) => result,
                reason = monitor.changed() => {
                    log::info!("Re-registering Bluetooth tunnel: {reason}");
                    Ok(())
                }
            }
        }
        .await;
        if let Err(error) = result {
            log::warn!("Bluetooth tunnel unavailable: {error}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn run_when_powered(config: &config::Config) -> bluer::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let events = adapter.events().await?;
    pin_mut!(events);
    if !adapter.is_powered().await? {
        return Ok(());
    }
    tokio::select! {
        result = serve(&adapter, config) => result,
        result = async {
            loop {
                tokio::select! {
                    event = events.next() => match event {
                        None | Some(bluer::AdapterEvent::PropertyChanged(
                            bluer::AdapterProperty::Powered(false))) => return Ok(()),
                        _ => continue,
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
                }
                if !adapter.is_powered().await? {
                    log::info!("Bluetooth powered off; releasing tunnel");
                    return Ok(());
                }
            }
        } => result,
    }
}

async fn serve(adapter: &bluer::Adapter, config: &config::Config) -> bluer::Result<()> {
    log::info!(
        "Bluetooth adapter {} ({})",
        adapter.name(),
        adapter.address().await?
    );

    // --- BLE GATT setup ---
    let mut service_uuids = BTreeSet::new();
    service_uuids.insert(*SERVICE_UUID);
    let adv = Advertisement {
        advertisement_type: bluer::adv::Type::Peripheral,
        service_uuids,
        local_name: Some(config.name.clone()),
        ..Default::default()
    };
    let (ack_tx, ack_rx) = mpsc::channel(1);
    let ack_rx = Arc::new(Mutex::new(ack_rx));
    let expected_ack = Arc::new(Mutex::new(None::<(bluer::Address, u64)>));
    let ack_state = expected_ack.clone();
    let packet_id = Arc::new(AtomicU64::new(1));

    let (mut tx_control, tx_handle) = characteristic_control();
    let (write_tx, mut write_rx) = mpsc::channel(1);
    let ingress = Arc::new(Mutex::new(incoming::Gate::Idle));
    let ingress_callback = ingress.clone();

    let app = Application {
        services: vec![Service {
            uuid: *SERVICE_UUID,
            // Eight fixed attributes, serialized in char0/char1/char2 order by
            // our dbus-crossroads patch. BlueZ auto-inserts TX's CCC at base+5.
            handle: std::num::NonZeroU16::new(config.gatt_handle),
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: *ACK_UUID,
                    handle: std::num::NonZeroU16::new(config.gatt_handle + 2),
                    write: Some(CharacteristicWrite {
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                            let sender = ack_tx.clone();
                            let expected = ack_state.clone();
                            Box::pin(async move {
                                if value.len() != transfer::FRAME_HEADER || request.offset != 0 {
                                    return Err(bluer::gatt::local::ReqError::InvalidValueLength);
                                }
                                let id = u64::from_le_bytes(value[..].try_into().unwrap());
                                if *expected.lock().await != Some((request.device_address, id)) {
                                    return Err(bluer::gatt::local::ReqError::NotPermitted);
                                }
                                sender
                                    .try_send(id)
                                    .map_err(|_| bluer::gatt::local::ReqError::InProgress)?;
                                Ok(())
                            })
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: *TX_UUID,
                    handle: std::num::NonZeroU16::new(config.gatt_handle + 4),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: tx_handle,
                    ..Default::default()
                },
                Characteristic {
                    uuid: *RX_UUID,
                    handle: std::num::NonZeroU16::new(config.gatt_handle + 7),
                    write: Some(CharacteristicWrite {
                        write: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                            let sender = write_tx.clone();
                            let ingress = ingress_callback.clone();
                            Box::pin(async move {
                                use bluer::gatt::local::ReqError;
                                // BlueZ 5.84 omits `type` for ordinary ATT writes;
                                // the patched bluer preserves this as None. The host
                                // explicitly requests a response, so do not infer
                                // ATT response semantics from this optional field.
                                if request.offset != 0
                                    || request.prepare_authorize
                                    || request.op_type == Some(bluer::gatt::WriteOp::Reliable)
                                {
                                    return Err(ReqError::NotSupported);
                                }
                                if value.len() > (request.mtu as usize).saturating_sub(3).min(512) {
                                    return Err(ReqError::InvalidValueLength);
                                }
                                let (id, data) = transfer::decode_frame(&value)
                                    .map_err(|_| ReqError::InvalidValueLength)?;
                                let (complete, response) = oneshot::channel();
                                {
                                    let mut gate = ingress.lock().await;
                                    gate.enqueue(
                                        &sender,
                                        incoming::Write {
                                            peer: request.device_address,
                                            mtu: request.mtu.into(),
                                            id,
                                            data: data.to_vec(),
                                            complete,
                                        },
                                    )?;
                                }
                                response.await.unwrap_or(Err(ReqError::Failed))
                            })
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };

    let _app_handle = if config.ble {
        let handle = adapter.serve_gatt_application(app).await?;
        log::info!(
            "BLE: GATT registered at 0x{:04x}–0x{:04x}, advertising as {}",
            config.gatt_handle,
            config.gatt_handle + 7,
            config.name
        );
        Some(handle)
    } else {
        None
    };

    // --- L2CAP BR/EDR setup (classic BT) ---
    let l2cap_listener = if config.classic {
        let l2cap_psm: u16 = 0x1001;
        let l2cap_socket =
            l2cap::Socket::<l2cap::Stream>::new_stream().map_err(|e| bluer::Error {
                kind: bluer::ErrorKind::Failed,
                message: format!("L2CAP socket: {e}"),
            })?;
        l2cap_socket
            .bind(l2cap::SocketAddr::new(
                bluer::Address::any(),
                AddressType::BrEdr,
                l2cap_psm,
            ))
            .map_err(|e| bluer::Error {
                kind: bluer::ErrorKind::Failed,
                message: format!("L2CAP bind: {e}"),
            })?;
        let l2cap_listener = l2cap_socket.listen(1).map_err(|e| bluer::Error {
            kind: bluer::ErrorKind::Failed,
            message: format!("L2CAP listen: {e}"),
        })?;
        log::info!("L2CAP: Listening on PSM 0x{l2cap_psm:04x} (BR/EDR)");

        Some(l2cap_listener)
    } else {
        None
    };

    let _adv_handle = if config.ble {
        Some(adapter.advertise(adv).await?)
    } else {
        None
    };

    struct Start {
        peer: bluer::Address,
        mtu: usize,
    }

    loop {
        log::info!("Waiting for client (BLE or L2CAP)...");
        let mut pending = subscriptions::Pending::<
            bluer::Address,
            bluer::gatt::CharacteristicWriter,
            Start,
        >::new();
        let setup_deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
        tokio::pin!(setup_deadline);

        enum Transport {
            Ble(bluer::gatt::CharacteristicWriter, Start),
            Classic(l2cap::Stream),
        }
        let transport = loop {
            if pending
                .notifier()
                .is_some_and(|writer| writer.is_closed().unwrap_or(true))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "BLE setup notification channel closed; resetting CCC state",
                )
                .into());
            }
            if let Some((writer, start)) = pending.take_ready() {
                *ingress.lock().await = incoming::Gate::Active(start.peer);
                break Transport::Ble(writer, start);
            }
            tokio::select! {
                event = tx_control.next(), if config.ble => {
                    let Some(event) = event else {
                        return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "GATT control ended").into());
                    };
                    let CharacteristicControlEvent::Notify(writer) = event else { continue; };
                    if !writer.is_closed().unwrap_or(true) {
                        let peer = writer.device_address();
                        if ingress.lock().await.reserve(peer) && pending.offer_notifier(peer, writer).is_ok() {
                            setup_deadline.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(30));
                        } // rejected writer is dropped; other peer remains intact
                    }
                }
                request = write_rx.recv(), if config.ble => {
                    let Some(request) = request else {
                        return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "GATT request channel closed").into());
                    };
                    if request.id != 0 || !request.data.is_empty() {
                        let _ = request.complete.send(Err(bluer::gatt::local::ReqError::NotPermitted));
                    } else {
                        if !ingress.lock().await.reserve(request.peer) {
                            let _ = request.complete.send(Err(bluer::gatt::local::ReqError::InProgress));
                            continue;
                        }
                        let start = Start { peer: request.peer, mtu: request.mtu };
                        match pending.offer_start(request.peer, start) {
                            Ok(()) => {
                                // Acknowledge admission before the host subscribes.
                                // Otherwise a subscription rejected during classic
                                // teardown can strand a later accepted START.
                                if request.complete.send(Ok(())).is_err() {
                                    if pending.notifier().is_some() {
                                        return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted,
                                            "Subscribed BLE setup canceled; resetting CCC state").into());
                                    }
                                    pending.clear();
                                    clear_session(&ingress, &mut write_rx, &expected_ack, &ack_rx, &mut tx_control, config.ble).await?;
                                } else {
                                    setup_deadline.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(30));
                                }
                            },
                            Err(_) => { let _ = request.complete.send(Err(bluer::gatt::local::ReqError::InProgress)); }
                        }
                    }
                }
                _ = async {
                    match pending.notifier() {
                        Some(writer) => { let _ = writer.closed().await; },
                        None => std::future::pending().await,
                    }
                } => {
                    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted,
                        "BLE setup notification channel closed; resetting CCC state").into());
                }
                result = async {
                    match &l2cap_listener {
                        Some(listener) => listener.accept().await,
                        None => std::future::pending().await,
                    }
                }, if !pending.occupied() => {
                    let (stream, _) = result?;
                    *ingress.lock().await = incoming::Gate::Classic;
                    break Transport::Classic(stream);
                }
                _ = &mut setup_deadline, if pending.occupied() => {
                    // A retained bonded CCC can suppress AcquireNotify entirely,
                    // so even START without a writer must reset on timeout.
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,
                        "BLE setup timed out; resetting CCC state").into());
                }
            }
        };

        let ble_peer = match &transport {
            Transport::Ble(_, start) => Some(start.peer),
            Transport::Classic(_) => None,
        };
        let active = async {
            let mut tcp =
                TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.ssh_port)).await?;
            match transport {
                Transport::Classic(mut stream) => {
                    let (mut tcp_read, mut tcp_write) = tcp.split();
                    let (mut bt_read, mut bt_write) = stream.split();
                    let copying = transfer::session(
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
                    );
                    tokio::select! {
                        result = copying => result,
                        result = incoming::reject_busy(&mut write_rx), if config.ble => result,
                    }
                }
                Transport::Ble(writer, start) => {
                    let payload = transfer::payload_size(start.mtu)?
                        .min(writer.mtu().saturating_sub(transfer::FRAME_HEADER));
                    let peer = start.peer;
                    let writer = Arc::new(writer);
                    let closed = writer.closed();
                    let (tcp_read, mut tcp_write) = tcp.split();
                    let send = |packet: Vec<u8>| {
                        let writer = writer.clone();
                        let ack_rx = ack_rx.clone();
                        let expected = expected_ack.clone();
                        let id = packet_id.fetch_add(1, Ordering::Relaxed);
                        async move {
                            *expected.lock().await = Some((peer, id));
                            writer.send(&transfer::encode_frame(id, &packet)).await?;
                            // The peer-specific socket targets only this central.
                            // The application ACK, not the socket send, proves delivery.
                            let mut receiver = ack_rx.lock().await;
                            while let Some(ack) = receiver.recv().await {
                                if ack == id {
                                    *expected.lock().await = None;
                                    return Ok(());
                                }
                            }
                            Err(std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "BLE ACK channel closed",
                            ))
                        }
                    };
                    let outgoing = async {
                        transfer::confirmed_copy(tcp_read, payload, &send).await?;
                        // Header-only frame is EOF and is acknowledged like data.
                        tokio::time::timeout(transfer::DELIVERY_TIMEOUT, send(Vec::new()))
                            .await
                            .map_err(|_| {
                                std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "BLE EOF ACK timed out",
                                )
                            })?
                    };
                    let incoming = incoming::forward(peer, &mut write_rx, &mut tcp_write);
                    tokio::select! {
                        // Prefer completed framed EOF over simultaneous FD close.
                        biased;
                        result = transfer::session(outgoing, incoming) => result,
                        _ = closed => Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionAborted,
                            "BLE notification channel closed",
                        )),
                    }
                }
            }
        };
        // Drain and close only NEW peer writers while a session is active.
        // AcquireNotify gives each central its own FD, so closing a rejected
        // writer never invalidates the admitted writer or its close signal.
        let reject_subscribers = async {
            while let Some(event) = tx_control.next().await {
                drop(event);
            }
        };
        let result = tokio::select! {
            result = active => result,
            _ = reject_subscribers, if config.ble => {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "GATT control ended").into());
            },
        };
        if result.as_ref().is_err_and(|error| error.kind() == std::io::ErrorKind::TimedOut) {
            if let Some(peer) = ble_peer {
                // A suspended central application can retain AcquireNotify after
                // our delivery deadline. Close this peer's link before admitting
                // another session, releasing its stale subscription remotely.
                log::warn!("Disconnecting stalled BLE peer {peer}");
                let disconnect = async { adapter.device(peer)?.disconnect().await };
                match tokio::time::timeout(std::time::Duration::from_secs(5), disconnect).await {
                    Ok(Ok(())) => (),
                    Ok(Err(error)) => log::warn!("Stalled peer disconnect failed: {error}"),
                    Err(_) => log::warn!("Stalled peer disconnect timed out"),
                }
            }
        }
        clear_session(
            &ingress,
            &mut write_rx,
            &expected_ack,
            &ack_rx,
            &mut tx_control,
            config.ble,
        )
        .await?;
        // Session failure is not registration failure. The watch's BlueZ CCC
        // fix reacquires notification writers after socket loss. Keep the GATT
        // database and advertising alive so peers with stale-cache bugs do not
        // need to rediscover our service after every interrupted session.
        match result {
            Ok(()) => log::info!("Session ended"),
            Err(error) => log::warn!("Session ended: {error}"),
        }
    }
}

#[cfg(test)]
mod session_cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn clears_old_work_before_admitting_next_session() {
        let peer = bluer::Address::new([1, 2, 3, 4, 5, 6]);
        let gate = Mutex::new(incoming::Gate::Active(peer));
        let (write_tx, mut write_rx) = mpsc::channel(1);
        let (complete, result) = oneshot::channel();
        assert!(write_tx
            .try_send(incoming::Write {
                peer,
                mtu: 64,
                id: 2,
                data: vec![42],
                complete,
            })
            .is_ok());
        let expected = Mutex::new(Some((peer, 9)));
        let (ack_tx, ack_rx) = mpsc::channel(1);
        ack_tx.try_send(9).unwrap();
        let acks = Mutex::new(ack_rx);
        let (control_tx, mut control_rx) = futures::channel::mpsc::unbounded();
        let sentinel = Arc::new(());
        control_tx.unbounded_send(sentinel.clone()).unwrap();

        clear_session(
            &gate,
            &mut write_rx,
            &expected,
            &acks,
            &mut control_rx,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(
            result.await.unwrap(),
            Err(bluer::gatt::local::ReqError::InProgress)
        ));
        assert_eq!(*expected.lock().await, None);
        assert!(acks.lock().await.try_recv().is_err());
        assert_eq!(
            Arc::strong_count(&sentinel),
            1,
            "queued subscriber must be dropped"
        );
        assert_eq!(*gate.lock().await, incoming::Gate::Idle);
        let (complete, _result) = oneshot::channel();
        assert!(gate
            .lock()
            .await
            .enqueue(
                &write_tx,
                incoming::Write {
                    peer,
                    mtu: 64,
                    id: 0,
                    data: Vec::new(),
                    complete,
                }
            )
            .is_ok());
        assert_eq!(write_rx.recv().await.unwrap().id, 0);
    }

    #[tokio::test]
    async fn closed_control_remains_a_registration_failure() {
        let gate = Mutex::new(incoming::Gate::Classic);
        let (_write_tx, mut write_rx) = mpsc::channel(1);
        let (_ack_tx, ack_rx) = mpsc::channel(1);
        let error = clear_session(
            &gate,
            &mut write_rx,
            &Mutex::new(None),
            &Mutex::new(ack_rx),
            &mut futures::stream::empty::<()>(),
            true,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(*gate.lock().await, incoming::Gate::Classic);
    }
}

#[cfg(test)]
mod object_manager_order_tests {
    #[test]
    fn managed_objects_are_serialized_in_gatt_registration_order() {
        use dbus::arg::ArgType;
        // Inspect the dictionary's wire order, not a decoded map: BlueZ 5.84
        // allocates CCC attributes while processing this exact order.
        for _ in 0..16 {
            let mut crossroads = dbus_crossroads::Crossroads::new();
            crossroads.insert("/app", &[crossroads.object_manager()], ());
            let expected = [
                "/app/service0",
                "/app/service0/char0",
                "/app/service0/char1",
                "/app/service0/char2",
            ];
            for path in expected.iter().rev() {
                crossroads.insert(*path, &[], ());
            }
            let mut call = dbus::Message::new_method_call(
                "dev.asteroidwatch.Test",
                "/app",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
            )
            .unwrap();
            call.set_serial(1);
            let replies = std::cell::RefCell::new(Vec::new());
            crossroads.handle_message(call, &replies).unwrap();
            let replies = replies.into_inner();
            assert_eq!(replies.len(), 1);
            let mut iter = replies[0].iter_init();
            let mut entries = iter.recurse(ArgType::Array).unwrap();
            let mut paths = Vec::new();
            while entries.arg_type() != ArgType::Invalid {
                let mut entry = entries.recurse(ArgType::DictEntry).unwrap();
                let path: dbus::Path<'_> = entry.read().unwrap();
                paths.push(path.to_string());
                entries.next();
            }
            assert_eq!(paths, expected);
        }
    }
}
