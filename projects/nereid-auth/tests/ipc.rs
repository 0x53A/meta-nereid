//! Private local bus only: no watch, Gatekeeper, real credentials, or system bus.
use futures_util::StreamExt;
use nereid_auth::{backend::Backend, service::AuthService, Client, Error, Outcome, BUS, PATH};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

struct Bus {
    child: Child,
    address: String,
}
impl Bus {
    fn start() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon required");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            child,
            address: address.trim().into(),
        }
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct Fake {
    enrolled: AtomicBool,
    calls: AtomicUsize,
    broken: bool,
}
impl Backend for Fake {
    fn enrolled(&self) -> Result<bool, Error> {
        Ok(self.enrolled.load(Ordering::SeqCst))
    }
    fn authenticate(&self, pin: &[u8]) -> Result<Outcome, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.broken {
            return Err("Synthetic infrastructure failure".into());
        }
        if pin != b"928461" {
            return Ok(Outcome::Rejected);
        }
        assert!(self.enrolled.load(Ordering::SeqCst));
        Ok(Outcome::Unlocked)
    }
    fn enroll(&self, pin: &[u8]) -> Result<Outcome, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(pin, b"928461");
        assert!(!self.enrolled.swap(true, Ordering::SeqCst));
        Ok(Outcome::Enrolled)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_client_enrollment_unlock_and_relock() {
    let bus = Bus::start();
    let fake = Arc::new(Fake {
        enrolled: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        broken: false,
    });
    let _server = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(BUS)
        .unwrap()
        .serve_at(PATH, AuthService::new(fake.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let monitor = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let proxy = zbus::Proxy::new(
        &monitor,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus.Monitoring",
    )
    .await
    .unwrap();
    proxy
        .call::<_, _, ()>(
            "BecomeMonitor",
            &(vec!["type='method_call',interface='io.Nereid.Auth1'"], 0u32),
        )
        .await
        .unwrap();
    let mut stream = zbus::MessageStream::from(&monitor);
    let address = bus.address.clone();
    tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let client = Client::from_connection(connection.clone());
        assert!(!client.state().unwrap().locked);
        assert!(!client.state().unwrap().enrolled);
        let p = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
        p.call::<_, _, ()>("Lock", &()).unwrap();
        assert!(!client.state().unwrap().locked);
        assert_eq!(
            client
                .submit_pin(client.begin_attempt().unwrap(), b"928461")
                .unwrap(),
            Outcome::Unavailable,
            "ordinary verification must not enroll"
        );
        assert!(!client.state().unwrap().enrolled);
        let attempt = client.begin_attempt().unwrap();
        assert_eq!(
            client.enroll_pin(attempt, b"928461").unwrap(),
            Outcome::Enrolled
        );
        assert!(client.state().unwrap().enrolled);
        assert!(!client.state().unwrap().locked);
        assert!(client.begin_attempt().is_err());
        p.call::<_, _, ()>("Lock", &()).unwrap();
        assert!(client.state().unwrap().locked);
        assert_eq!(
            client
                .enroll_pin(client.begin_attempt().unwrap(), b"928461")
                .unwrap(),
            Outcome::Unavailable,
            "setup cannot replace an enrolled PIN"
        );
        let attempt = client.begin_attempt().unwrap();
        assert_eq!(
            client.submit_pin(attempt, b"928461").unwrap(),
            Outcome::Unlocked
        );
        assert!(!client.state().unwrap().locked);
        assert!(client.begin_attempt().is_err());
        let p = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
        p.call::<_, _, ()>("Lock", &()).unwrap();
        assert!(client.state().unwrap().locked);
        assert_eq!(
            client
                .submit_pin(client.begin_attempt().unwrap(), b"111111")
                .unwrap(),
            Outcome::Rejected
        );
        assert!(client.state().unwrap().locked);
        assert!(client.begin_attempt().is_err()); // cooldown, no new TEE call
    })
    .await
    .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    let mut submissions = 0;
    while submissions < 5 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if message
            .header()
            .member()
            .is_some_and(|m| matches!(m.as_str(), "SubmitPin" | "SubmitEnrollment"))
        {
            assert!(!message
                .data()
                .bytes()
                .windows(6)
                .any(|w| w == b"928461" || w == b"111111"));
            let (id, sealed): (Vec<u8>, Vec<u8>) = message.body().deserialize().unwrap();
            assert_eq!(id.len(), 32);
            assert_eq!(sealed.len(), 112);
            submissions += 1;
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_failure_latches_without_retry() {
    let bus = Bus::start();
    let fake = Arc::new(Fake {
        enrolled: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        broken: true,
    });
    let _server = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(BUS)
        .unwrap()
        .serve_at(PATH, AuthService::new(fake.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let address = bus.address.clone();
    tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let client = Client::from_connection(connection);
        assert_eq!(
            client
                .submit_pin(client.begin_attempt().unwrap(), b"928461")
                .unwrap(),
            Outcome::Unavailable
        );
        assert!(client.state().is_err());
        assert!(client.begin_attempt().is_err());
    })
    .await
    .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
// Private dbus-daemon only; no native helper, watch, or real credentials.

struct BlockingUnlockBackend {
    initially_enrolled: bool,
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    events: std::sync::Mutex<Vec<&'static str>>,
    lock_calls: std::sync::atomic::AtomicUsize,
}

impl nereid_auth::backend::Backend for BlockingUnlockBackend {
    fn enrolled(&self) -> Result<bool, nereid_auth::Error> {
        Ok(self.initially_enrolled)
    }

    fn authenticate(&self, _pin: &[u8]) -> Result<nereid_auth::Outcome, nereid_auth::Error> {
        self.events.lock().unwrap().push("authenticate-start");
        self.started
            .lock()
            .unwrap()
            .take()
            .expect("one blocked authentication")
            .send(())
            .map_err(|_| "test synchronization receiver closed")?;
        self.release
            .lock()
            .unwrap()
            .recv()
            .map_err(|_| "test release channel closed")?;
        self.events.lock().unwrap().push("authenticate-finished");
        Ok(nereid_auth::Outcome::Unlocked)
    }

    fn enroll(&self, pin: &[u8]) -> Result<Outcome, Error> {
        self.authenticate(pin)?;
        Ok(Outcome::Enrolled)
    }

    fn lock(&self) -> Result<(), nereid_auth::Error> {
        self.events.lock().unwrap().push("lock");
        self.lock_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_waits_for_inflight_auth_and_suppresses_unlock_reply() {
    lock_during_submission(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_during_enrollment_enforces_new_pin_after_completion() {
    lock_during_submission(true).await;
}

async fn lock_during_submission(enrollment: bool) {
    use futures_util::StreamExt;
    use nereid_auth::{service::AuthService, Client, Outcome, BUS, PATH};
    use std::sync::atomic::Ordering;

    let bus = Bus::start();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let fake = std::sync::Arc::new(BlockingUnlockBackend {
        initially_enrolled: !enrollment,
        started: std::sync::Mutex::new(Some(started_tx)),
        release: std::sync::Mutex::new(release_rx),
        events: std::sync::Mutex::new(Vec::new()),
        lock_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let _server = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(BUS)
        .unwrap()
        .serve_at(PATH, AuthService::new(fake.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();

    // Monitor the property signal that Lock emits after setting closing=true.
    // SubmitPin emits Busy first; a Locked property change can only be the
    // in-flight Lock request because authentication remains blocked.
    let monitor = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let monitor_proxy = zbus::Proxy::new(
        &monitor,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus.Monitoring",
    )
    .await
    .unwrap();
    monitor_proxy
        .call::<_, _, ()>(
            "BecomeMonitor",
            &(
                vec!["type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'"],
                0u32,
            ),
        )
        .await
        .unwrap();
    let mut stream = zbus::MessageStream::from(&monitor);

    // BeginAttempt and SubmitPin share one D-Bus connection so the attempt is
    // still bound to its original sender.
    let address = bus.address.clone();
    let submit_task = tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let client = Client::from_connection(connection);
        let attempt = client.begin_attempt().unwrap();
        if enrollment {
            client.enroll_pin(attempt, b"928461")
        } else {
            client.submit_pin(attempt, b"928461")
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
        .await
        .expect("authentication did not start")
        .expect("backend start signal was dropped");

    let address = bus.address.clone();
    let (lock_sent_tx, lock_sent_rx) = tokio::sync::oneshot::channel();
    let lock_task = tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let proxy = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
        let _ = lock_sent_tx.send(());
        proxy.call::<_, _, ()>("Lock", &()).unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), lock_sent_rx)
        .await
        .expect("Lock client did not start")
        .expect("Lock client start signal was dropped");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut lock_state_published = false;
    while !lock_state_published {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "Lock did not publish its closing state"
        );
        let message = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for Lock property signal")
            .expect("monitor stream ended")
            .expect("monitor returned a bus error");
        if message
            .header()
            .member()
            .is_none_or(|member| member.as_str() != "PropertiesChanged")
        {
            continue;
        }
        let Ok((interface, changed, _invalidated)) = message.body().deserialize::<(
            String,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<String>,
        )>() else {
            continue;
        };
        lock_state_published = interface == BUS && changed.contains_key("Locked");
    }

    assert_eq!(fake.lock_calls.load(Ordering::SeqCst), 0);
    assert!(
        !lock_task.is_finished(),
        "Lock must wait for authentication"
    );
    let address = bus.address.clone();
    let state = tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        Client::from_connection(connection).state().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(state.locked, !enrollment);
    assert!(state.busy);

    release_tx.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), submit_task)
            .await
            .expect("SubmitPin remained blocked")
            .unwrap()
            .unwrap(),
        if enrollment {
            Outcome::Enrolled
        } else {
            Outcome::Unavailable
        },
        "Lock supersedes unlock; explicit enrollment still completes"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), lock_task)
        .await
        .expect("Lock remained blocked after authentication")
        .unwrap();
    assert_eq!(
        fake.events.lock().unwrap().as_slice(),
        &["authenticate-start", "authenticate-finished", "lock"]
    );
    assert_eq!(fake.lock_calls.load(Ordering::SeqCst), 1);
    // Both an existing PIN and a newly enrolled PIN must enforce the lock.
    let address = bus.address.clone();
    let state = tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        Client::from_connection(connection).state().unwrap()
    })
    .await
    .unwrap();
    assert!(state.enrolled);
    assert!(state.locked);
}

struct CleanupFailureBackend {
    authenticate_calls: std::sync::atomic::AtomicUsize,
    lock_calls: std::sync::atomic::AtomicUsize,
}

impl nereid_auth::backend::Backend for CleanupFailureBackend {
    fn enrolled(&self) -> Result<bool, nereid_auth::Error> {
        Ok(true)
    }

    fn authenticate(&self, _pin: &[u8]) -> Result<nereid_auth::Outcome, nereid_auth::Error> {
        self.authenticate_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(nereid_auth::Outcome::Unlocked)
    }

    fn lock(&self) -> Result<(), nereid_auth::Error> {
        self.lock_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("synthetic storage cleanup failure".into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_cleanup_failure_latches_unavailable_while_screen_locked() {
    use nereid_auth::{service::AuthService, Client, Outcome, BUS, PATH};
    use std::sync::atomic::Ordering;

    let bus = Bus::start();
    let fake = std::sync::Arc::new(CleanupFailureBackend {
        authenticate_calls: std::sync::atomic::AtomicUsize::new(0),
        lock_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let _server = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(BUS)
        .unwrap()
        .serve_at(PATH, AuthService::new(fake.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();

    let address = bus.address.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || {
            let connection = zbus::blocking::connection::Builder::address(address.as_str())
                .unwrap()
                .build()
                .unwrap();
            let client = Client::from_connection(connection);
            let attempt = client.begin_attempt().unwrap();
            client.submit_pin(attempt, b"928461").unwrap()
        })
        .await
        .unwrap(),
        Outcome::Unlocked
    );

    let address = bus.address.clone();
    assert!(
        tokio::task::spawn_blocking(move || {
            let connection = zbus::blocking::connection::Builder::address(address.as_str())
                .unwrap()
                .build()
                .unwrap();
            let proxy = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
            proxy.call::<_, _, ()>("Lock", &())
        })
        .await
        .unwrap()
        .is_err(),
        "cleanup failure must fail the Lock method"
    );

    let address = bus.address.clone();
    let (locked, busy, begin_attempt_fails) = tokio::task::spawn_blocking(move || {
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let client = Client::from_connection(connection.clone());
        assert!(client.state().is_err());
        let proxy = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
        (
            proxy.get_property::<bool>("Locked").unwrap(),
            proxy.get_property::<bool>("Busy").unwrap(),
            client.begin_attempt().is_err(),
        )
    })
    .await
    .unwrap();
    assert!(locked, "screen state remains locked after cleanup failure");
    assert!(!busy, "failed cleanup must leave the transaction closed");
    assert!(
        begin_attempt_fails,
        "fault must latch BeginAttempt unavailable"
    );
    assert_eq!(fake.authenticate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fake.lock_calls.load(Ordering::SeqCst), 1);
}

struct ManagementBackend {
    pin: std::sync::Mutex<Option<Vec<u8>>>,
    protected: AtomicBool,
    broken: bool,
    mutations: AtomicUsize,
}
impl Backend for ManagementBackend {
    fn enrolled(&self) -> Result<bool, Error> {
        Ok(self.pin.lock().unwrap().is_some())
    }
    fn authenticate(&self, pin: &[u8]) -> Result<Outcome, Error> {
        Ok(if self.pin.lock().unwrap().as_deref() == Some(pin) {
            Outcome::Unlocked
        } else {
            Outcome::Rejected
        })
    }
    fn change_pin(&self, current: &[u8], new: &[u8]) -> Result<Outcome, Error> {
        if self.broken {
            return Err("synthetic management failure".into());
        }
        let mut pin = self.pin.lock().unwrap();
        if pin.as_deref() != Some(current) {
            return Ok(Outcome::Rejected);
        }
        *pin = Some(new.to_vec());
        self.mutations.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome::PinChanged)
    }
    fn clear_pin(&self, current: &[u8]) -> Result<Outcome, Error> {
        if self.protected.load(Ordering::SeqCst) {
            return Ok(Outcome::StorageProtected);
        }
        let mut pin = self.pin.lock().unwrap();
        if pin.as_deref() != Some(current) {
            return Ok(Outcome::Rejected);
        }
        *pin = None;
        self.mutations.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome::PinCleared)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn management_change_clear_and_error_states() {
    for mode in ["lifecycle", "rejection", "failure"] {
        let bus = Bus::start();
        let backend = Arc::new(ManagementBackend {
            pin: std::sync::Mutex::new(Some(b"928461".to_vec())),
            protected: AtomicBool::new(true),
            broken: mode == "failure",
            mutations: AtomicUsize::new(0),
        });
        let _server = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(BUS)
            .unwrap()
            .serve_at(PATH, AuthService::new(backend.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let monitor = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let monitor_proxy = zbus::Proxy::new(
            &monitor,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus.Monitoring",
        )
        .await
        .unwrap();
        monitor_proxy
            .call::<_, _, ()>(
                "BecomeMonitor",
                &(
                    vec!["type='method_call',interface='io.Nereid.Auth1',member='ManagePin'"],
                    0u32,
                ),
            )
            .await
            .unwrap();
        let mut stream = zbus::MessageStream::from(&monitor);
        let address = bus.address.clone();
        let copy = backend.clone();
        tokio::task::spawn_blocking(move || {
            let connection = zbus::blocking::connection::Builder::address(address.as_str())
                .unwrap()
                .build()
                .unwrap();
            let client = Client::from_connection(connection.clone());
            assert!(client.begin_management().is_err());
            assert_eq!(
                client
                    .submit_pin(client.begin_attempt().unwrap(), b"928461")
                    .unwrap(),
                Outcome::Unlocked
            );
            if mode == "failure" {
                assert_eq!(
                    client
                        .change_pin(client.begin_management().unwrap(), b"928461", b"975310")
                        .unwrap(),
                    Outcome::Unavailable
                );
                assert!(client.state().is_err());
                assert!(client.begin_management().is_err());
                return;
            }
            if mode == "rejection" {
                assert_eq!(
                    client
                        .change_pin(client.begin_management().unwrap(), b"111111", b"975310")
                        .unwrap(),
                    Outcome::Rejected
                );
                assert!(client.begin_management().is_err());
                assert!(!client.state().unwrap().locked);
                return;
            }
            assert_eq!(
                client
                    .clear_pin(client.begin_management().unwrap(), b"928461")
                    .unwrap(),
                Outcome::StorageProtected
            );
            assert!(client.state().unwrap().enrolled);
            assert!(!client.state().unwrap().locked);
            assert_eq!(
                client
                    .change_pin(client.begin_management().unwrap(), b"928461", b"975310")
                    .unwrap(),
                Outcome::PinChanged
            );
            assert!(!client.state().unwrap().locked);
            let proxy = zbus::blocking::Proxy::new(&connection, BUS, PATH, BUS).unwrap();
            proxy.call::<_, _, ()>("Lock", &()).unwrap();
            assert_eq!(
                client
                    .submit_pin(client.begin_attempt().unwrap(), b"975310")
                    .unwrap(),
                Outcome::Unlocked
            );
            copy.protected.store(false, Ordering::SeqCst);
            assert_eq!(
                client
                    .clear_pin(client.begin_management().unwrap(), b"975310")
                    .unwrap(),
                Outcome::PinCleared
            );
            assert!(!client.state().unwrap().enrolled);
            assert!(!client.state().unwrap().locked);
            proxy.call::<_, _, ()>("Lock", &()).unwrap();
            assert!(!client.state().unwrap().locked);
            assert!(client.begin_management().is_err());
        })
        .await
        .unwrap();
        assert_eq!(
            backend.mutations.load(Ordering::SeqCst),
            if mode == "lifecycle" { 2 } else { 0 }
        );
        let mut seen = 0;
        while seen < if mode == "lifecycle" { 3 } else { 1 } {
            let message = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if message
                .header()
                .member()
                .is_some_and(|m| m.as_str() == "ManagePin")
            {
                assert!(!message
                    .data()
                    .bytes()
                    .windows(6)
                    .any(|w| w == b"928461" || w == b"975310" || w == b"111111"));
                let (id, sealed): (Vec<u8>, Vec<u8>) = message.body().deserialize().unwrap();
                assert_eq!(id.len(), 32);
                assert_eq!(sealed.len(), 112);
                seen += 1;
            }
        }
    }
}
