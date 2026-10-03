//! Clock activities are service-owned; the overlay only renders them.
use serde_json::{Value, json};
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, mpsc},
    time::Duration,
};
#[path = "../../shared/clock_client.rs"]
mod client;
pub enum Event {
    Snapshot(Value),
    Visibility(bool),
    Error(String),
    ActionError(String),
}
pub struct Updates {
    pub fd: Arc<OwnedFd>,
    pub rx: mpsc::Receiver<Event>,
    pub tx: mpsc::Sender<Event>,
}
pub fn send(tx: &mpsc::Sender<Event>, fd: &OwnedFd, event: Event) {
    if tx.send(event).is_ok() {
        let one = 1u64;
        unsafe {
            libc::write(fd.as_raw_fd(), (&one as *const u64).cast(), 8);
        }
    }
}
pub fn start() -> Updates {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0);
    let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
    let (tx, rx) = mpsc::channel();
    let f = fd.clone();
    let t = tx.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line.as_deref() {
                Ok("visibility:visible") => send(&t, &f, Event::Visibility(true)),
                Ok("visibility:hidden") => send(&t, &f, Event::Visibility(false)),
                Err(_) => break,
                _ => {}
            }
        }
        std::process::exit(0);
    });
    let owner_tx = tx.clone();
    let owner_fd = fd.clone();
    std::thread::spawn(move || {
        loop {
            let watch = || -> Result<(), String> {
                let connection = zbus::blocking::connection::Builder::session()
                    .map_err(|e| e.to_string())?
                    .method_timeout(Duration::from_secs(2))
                    .build()
                    .map_err(|e| e.to_string())?;
                let proxy = zbus::blocking::Proxy::new(
                    &connection,
                    "org.hoki.Clock1",
                    "/org/hoki/Clock1",
                    "org.hoki.Clock1",
                )
                .map_err(|e| e.to_string())?;
                for owner in proxy.receive_owner_changed().map_err(|e| e.to_string())? {
                    if owner.is_none() {
                        send(
                            &owner_tx,
                            &owner_fd,
                            Event::Error("Clock service disconnected".into()),
                        );
                    } else if let Ok(text) = client::request(r#"{"op":"snapshot"}"#) {
                        if let Ok(value) = serde_json::from_str(&text) {
                            send(&owner_tx, &owner_fd, Event::Snapshot(value));
                        }
                    }
                }
                Ok(())
            };
            let _ = watch();
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    let f = fd.clone();
    let subscriber_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            let tx = &subscriber_tx;
            let run = || -> Result<(), String> {
                let connection = zbus::blocking::connection::Builder::session()
                    .map_err(|e| e.to_string())?
                    .method_timeout(Duration::from_secs(2))
                    .build()
                    .map_err(|e| e.to_string())?;
                let proxy = zbus::blocking::Proxy::new(
                    &connection,
                    "org.hoki.Clock1",
                    "/org/hoki/Clock1",
                    "org.hoki.Clock1",
                )
                .map_err(|e| e.to_string())?;
                let signals = proxy.receive_signal("Changed").map_err(|e| e.to_string())?;
                let initial: String = proxy
                    .call("Command", &(r#"{"op":"snapshot"}"#,))
                    .map_err(|e| e.to_string())?;
                send(
                    &tx,
                    &f,
                    Event::Snapshot(serde_json::from_str(&initial).map_err(|e| e.to_string())?),
                );
                for message in signals {
                    let (text,): (String,) =
                        message.body().deserialize().map_err(|e| e.to_string())?;
                    if let Ok(value) = serde_json::from_str(&text) {
                        send(&tx, &f, Event::Snapshot(value));
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                send(&tx, &f, Event::Error(e));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    Updates { fd, rx, tx }
}
pub fn action(alarm: bool, id: u64, snooze: bool) -> Result<(), String> {
    let op = if alarm {
        if snooze {
            "alarm-snooze"
        } else {
            "alarm-dismiss"
        }
    } else {
        "timer-dismiss"
    };
    client::request(&json!({"op":op,"id":id}).to_string()).map(|_| ())
}
pub fn boottime_ms() -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut t) },
        0
    );
    t.tv_sec as i64 * 1000 + t.tv_nsec as i64 / 1_000_000
}
pub struct View {
    pub snapshot: Value,
    pub received: i64,
}
impl Default for View {
    fn default() -> Self {
        Self {
            snapshot: Value::Null,
            received: boottime_ms(),
        }
    }
}
impl View {
    pub fn active(&self) -> bool {
        self.snapshot["timers"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["running"] == true))
    }
    pub fn alert(&self) -> Option<(bool, u64, String)> {
        for key in ["alarms", "timers"] {
            for value in self.snapshot[key].as_array().into_iter().flatten() {
                if value["ringing"] == true {
                    return Some((
                        key == "alarms",
                        value["id"].as_u64()?,
                        value["label"].as_str().unwrap_or("Clock").into(),
                    ));
                }
            }
        }
        None
    }
    pub fn arcs(&self, width: f32) -> Vec<super::RingArc> {
        let timers = self.snapshot["timers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let stopwatch = self.snapshot["stopwatch"]["running"] == true;
        let count = timers.len() + usize::from(stopwatch);
        if count == 0 {
            return vec![];
        }
        let radius = 208. - width / 2.;
        let point = |angle: f32| {
            let a = angle.to_radians();
            (208. + radius * a.sin(), 208. - radius * a.cos())
        };
        let arc = |start: f32, sweep: f32, paused: bool| {
            let (x1, y1) = point(start);
            let (x2, y2) = point(start + sweep);
            super::RingArc {
                x1,
                y1,
                x2,
                y2,
                large: sweep > 180.,
                paused,
            }
        };
        let slot = 360. / count as f32;
        let mut arcs = Vec::new();
        for (i, t) in timers.iter().enumerate() {
            if t["ringing"] == true {
                continue;
            }
            let remaining = t["remaining"].as_i64().unwrap_or(0)
                - if t["running"] == true {
                    (boottime_ms() - self.received).max(0)
                } else {
                    0
                };
            let progress = (remaining.max(0) as f32
                / t["total"].as_i64().unwrap_or(1).max(1) as f32)
                .clamp(0., 1.);
            let sweep = (slot - 4.) * progress;
            if sweep > 0.1 {
                arcs.push(arc(i as f32 * slot + 2., sweep, t["running"] != true));
            }
        }
        if stopwatch {
            arcs.push(arc(
                (count - 1) as f32 * slot + 2.,
                (slot - 4.).min(20.),
                false,
            ));
        }
        arcs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activities_partition_ring_and_finished_timer_becomes_alert() {
        let view = View {
            snapshot: json!({"timers":[{"id":1,"total":10000,"remaining":5000,"running":false},{"id":2,"total":10000,"remaining":0,"ringing":true,"label":"Tea"}],"stopwatch":{"running":true}}),
            received: boottime_ms(),
        };
        let arcs = view.arcs(3.);
        assert_eq!(arcs.len(), 2);
        assert!(arcs[0].paused);
        for arc in arcs {
            for (x, y) in [(arc.x1, arc.y1), (arc.x2, arc.y2)] {
                assert!(((x - 208.).hypot(y - 208.) - 206.5).abs() < 0.01);
            }
        }
        assert_eq!(view.alert(), Some((false, 2, "Tea".into())));
    }
}
