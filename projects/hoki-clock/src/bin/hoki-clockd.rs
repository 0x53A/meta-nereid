// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
use hoki_clock::{
    BUS, PATH,
    model::{Now, State},
    store,
};
use serde_json::Value;
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[path = "../../../shared/sleep_client.rs"]
mod sleep_client;

struct Shared {
    state: Mutex<State>,
    path: PathBuf,
    wake: OwnedFd,
    delivery_error: Mutex<Option<String>>,
}
struct Clock(Arc<Shared>);
#[zbus::interface(name = "org.hoki.Clock1")]
impl Clock {
    fn command(&self, command: &str) -> zbus::fdo::Result<String> {
        if command.len() > 4096 {
            return Err(zbus::fdo::Error::InvalidArgs("Command too large".into()));
        }
        let command: Value = serde_json::from_str(command)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let mut state = self.0.state.lock().unwrap();
        let now = Now::read();
        let mut next = state.clone();
        next.reconcile(&now);
        next.command(&command, &now)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        if serde_json::to_vec(&next).unwrap() != serde_json::to_vec(&*state).unwrap() {
            store::save(&self.0.path, &next)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            *state = next;
            let one = 1u64;
            unsafe {
                libc::write(self.0.wake.as_raw_fd(), (&one as *const u64).cast(), 8);
            }
        }
        let mut snapshot = state.snapshot(&now);
        snapshot["delivery_error"] = serde_json::json!(*self.0.delivery_error.lock().unwrap());
        Ok(snapshot.to_string())
    }
}
fn fd(clock: libc::clockid_t) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::timerfd_create(clock, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
fn arm(fd: &OwnedFd, value: Option<i64>, absolute: bool) -> io::Result<()> {
    let ms = value.unwrap_or(0).max(0);
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: (ms / 1000) as _,
            tv_nsec: ((ms % 1000) * 1_000_000) as _,
        },
    };
    let flags = if absolute {
        libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET
    } else {
        0
    };
    if unsafe { libc::timerfd_settime(fd.as_raw_fd(), flags, &spec, std::ptr::null_mut()) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
unsafe extern "C" {
    fn hap_open() -> i32;
    fn hap_upload(fd: i32, id: *mut i32, preset: i32, strength: i32, ms: i32) -> i32;
    fn hap_play(fd: i32, id: i32, on: i32) -> i32;
    fn hap_close(fd: i32, id: i32) -> i32;
}
fn vibrate() -> io::Result<()> {
    let fd = unsafe { hap_open() };
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(-fd));
    }
    let mut id = -1;
    let result = unsafe { hap_upload(fd, &mut id, -1, 65, 200) };
    let played = if result >= 0 {
        unsafe { hap_play(fd, id, 1) }
    } else {
        result
    };
    if played >= 0 {
        std::thread::sleep(Duration::from_millis(220));
    }
    let cleaned = unsafe { hap_close(fd, id) };
    if played < 0 {
        Err(io::Error::from_raw_os_error(-played))
    } else if cleaned < 0 {
        Err(io::Error::from_raw_os_error(-cleaned))
    } else {
        Ok(())
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Alarm clocks must wake from suspend; never silently downgrade to ordinary timers.
    let test_mode = std::env::args().any(|a| a == "--test-no-wake");
    let realtime = fd(if test_mode {
        libc::CLOCK_REALTIME
    } else {
        libc::CLOCK_REALTIME_ALARM
    })?;
    let boottime = fd(if test_mode {
        libc::CLOCK_BOOTTIME
    } else {
        libc::CLOCK_BOOTTIME_ALARM
    })?;
    // Check CAP_WAKE_ALARM now, rather than accepting a timer we cannot deliver.
    arm(&boottime, Some(1), false)?;
    arm(&boottime, None, false)?;
    let path = std::env::var_os("HOKI_CLOCK_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/hoki-clock/state.json"));
    let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if wake < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(store::load(&path)?),
        delivery_error: Mutex::new(None),
        path,
        wake: unsafe { OwnedFd::from_raw_fd(wake) },
    });
    let connection = zbus::blocking::connection::Builder::session()?
        .name(BUS)?
        .serve_at(PATH, Clock(shared.clone()))?
        .build()?;
    let mut ringing_since: Option<Instant> = None;
    let mut feedback_at = Instant::now();
    let mut inhibitor: Option<sleep_client::Client> = None;
    let mut last_snapshot = String::new();
    let mut previous_alerts = std::collections::BTreeSet::new();
    loop {
        let now = Now::read();
        let (ringing, boot_due, real_due, snapshot, alerts) = {
            let mut state = shared.state.lock().unwrap();
            let mut next = state.clone();
            if next.reconcile(&now) {
                store::save(&shared.path, &next)?;
                *state = next;
            }
            let boot = state
                .timers
                .iter()
                .filter(|t| t.started.is_some())
                .map(|t| t.left(&now))
                .chain(state.alarms.iter().filter_map(|a| {
                    a.snooze.as_ref().map(|s| {
                        let elapsed = if s.boot_id == now.boot_id {
                            now.boot - s.boot
                        } else {
                            now.utc - s.utc
                        };
                        (300000 - elapsed).max(0)
                    })
                }))
                .min();
            let real = state
                .alarms
                .iter()
                .filter(|a| a.enabled && !a.ringing && a.snooze.is_none())
                .filter_map(|a| a.next)
                .min();
            let alerts: std::collections::BTreeSet<u64> = state
                .alarms
                .iter()
                .filter(|a| a.ringing)
                .map(|a| a.id)
                .chain(state.timers.iter().filter(|t| t.ringing).map(|t| t.id))
                .collect();
            let mut snapshot = state.snapshot(&now);
            snapshot["delivery_error"] = serde_json::json!(*shared.delivery_error.lock().unwrap());
            (state.ringing(), boot, real, snapshot.to_string(), alerts)
        };
        if snapshot != last_snapshot {
            connection.emit_signal(None::<&str>, PATH, BUS, "Changed", &(snapshot.clone(),))?;
            last_snapshot = snapshot;
        }
        if alerts.iter().any(|id| !previous_alerts.contains(id)) {
            ringing_since = None;
            feedback_at = Instant::now();
        }
        previous_alerts = alerts;
        if ringing {
            let start = *ringing_since.get_or_insert_with(Instant::now);
            if !test_mode && start.elapsed() < Duration::from_secs(120) {
                if inhibitor.is_none() {
                    match sleep_client::Client::connect().and_then(|mut c| {
                        c.inhibit(true, true, "Clock alert")?;
                        Ok(c)
                    }) {
                        Ok(c) => inhibitor = Some(c),
                        Err(e) => {
                            eprintln!("Clock display wake: {e}");
                            *shared.delivery_error.lock().unwrap() =
                                Some(format!("Display wake unavailable: {e}"));
                        }
                    }
                }
                if let Some(client) = inhibitor.as_mut() {
                    if let Err(e)=client.request(serde_json::json!({"command":"inhibit","cpu":true,"display":true,"reason":"Clock alert"})) {
                        *shared.delivery_error.lock().unwrap()=Some(format!("Wake connection lost: {e}"));inhibitor=None;
                    }
                }
                if inhibitor.is_some() && Instant::now() >= feedback_at {
                    match vibrate() {
                        Ok(()) => *shared.delivery_error.lock().unwrap() = None,
                        Err(e) => {
                            eprintln!("Clock vibration: {e}");
                            *shared.delivery_error.lock().unwrap() =
                                Some(format!("Vibration unavailable: {e}"));
                        }
                    }
                    feedback_at = Instant::now() + Duration::from_secs(2);
                }
            } else {
                inhibitor = None;
            }
        } else {
            ringing_since = None;
            inhibitor = None;
            feedback_at = Instant::now();
            *shared.delivery_error.lock().unwrap() = None;
        }
        arm(
            &boottime,
            boot_due.map(|n| (n - (Now::read().boot - now.boot)).max(1)),
            false,
        )?;
        if let Err(e) = arm(&realtime, real_due, true) {
            if e.raw_os_error() == Some(libc::ECANCELED) {
                continue;
            }
            return Err(e.into());
        }
        let mut poll = [
            shared.wake.as_raw_fd(),
            boottime.as_raw_fd(),
            realtime.as_raw_fd(),
        ]
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        let timeout = if ringing { 1000 } else { 60000 }; // poll timeout does not wake a suspended CPU
        let result = unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as _, timeout) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().into());
        }
        for p in poll {
            if p.revents != 0 {
                let mut n = 0u64;
                unsafe {
                    libc::read(p.fd, (&mut n as *mut u64).cast(), 8);
                }
            }
        }
    }
}
