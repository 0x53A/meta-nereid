slint::include_modules!();

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
pub mod measurement;
use measurement::{RawReading, ReadingDecision};
const MEASUREMENT_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_TIMESTAMP_FUTURE_SKEW_US: u64 = 5_000_000;

struct HealthLease { stream: UnixStream }
impl HealthLease {
    fn connect() -> Result<Self,String> {
        let stream=UnixStream::connect("/run/hoki-health-policy/control.sock").map_err(|e|e.to_string())?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e|e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e|e.to_string())?;
        Ok(Self{stream})
    }
    fn request(&mut self,command:&str)->Result<serde_json::Value,String> {
        writeln!(self.stream,"{}",serde_json::json!({"command":command,"profile":"spo2"})).map_err(|e|e.to_string())?;
        let mut bytes=Vec::new();let mut b=[0];
        while bytes.len()<65536 {
            self.stream.read_exact(&mut b).map_err(|e|e.to_string())?;
            if b[0]==b'\n' {break;} bytes.push(b[0]);
        }
        if bytes.len()>=65536 {return Err("Health reply too large".into());}
        let reply:serde_json::Value=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
        if reply["ok"]!=true {return Err(reply["error"].as_str().unwrap_or("Health request failed").into());}
        Ok(reply)
    }
}

#[cfg(target_os = "linux")]
fn boottime_micros() -> Result<u64, String> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_BOOTTIME matches Android elapsedRealtime, the HAL timestamp domain.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0
        || value.tv_sec < 0
        || value.tv_nsec < 0
    {
        return Err("cannot establish CLOCK_BOOTTIME freshness floor".into());
    }
    (value.tv_sec as u64)
        .checked_mul(1_000_000)
        .and_then(|s| s.checked_add(value.tv_nsec as u64 / 1_000))
        .ok_or_else(|| "CLOCK_BOOTTIME freshness floor overflow".into())
}
#[cfg(not(target_os = "linux"))]
fn boottime_micros() -> Result<u64, String> {
    Err("CLOCK_BOOTTIME freshness floor unavailable".into())
}

struct ControlState {
    generation: u64,
    running: bool,
    cancel: Option<Arc<AtomicBool>>,
}

impl ControlState {
    fn begin(&mut self) -> Option<(u64, Arc<AtomicBool>)> {
        if self.running {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.running = true;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        Some((self.generation, cancel))
    }

    fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.running = false;
    }

    fn finish(&mut self, generation: u64) {
        if self.generation == generation {
            self.running = false;
            self.cancel = None;
        }
    }
}

fn post_if_current(
    weak: &slint::Weak<App>,
    control: &Arc<Mutex<ControlState>>,
    generation: u64,
    callback: impl FnOnce(App) + Send + 'static,
) {
    let weak = weak.clone();
    let control = Arc::clone(control);
    let _ = slint::invoke_from_event_loop(move || {
        if control
            .lock()
            .map(|state| state.generation == generation)
            .unwrap_or(false)
        {
            if let Some(app) = weak.upgrade() {
                callback(app);
            }
        }
    });
}

enum WorkerCommand {
    Start {
        generation: u64,
        cancel: Arc<AtomicBool>,
    },
    Shutdown,
}

fn run_measurement(
    weak: &slint::Weak<App>,
    control: &Arc<Mutex<ControlState>>,
    generation: u64,
    cancel: &Arc<AtomicBool>,
) {
    let result = (|| -> Result<(), String> {
        // Stop may cancel a command while it is still queued behind an older
        // serial worker request. Skip it before opening D-Bus in that case.
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let floor=boottime_micros()?;
        let mut lease=HealthLease::connect()?;
        lease.request("acquire")?;
        let started_at=Instant::now();
        let mut last_timestamp = floor;
        post_if_current(weak, control, generation, |app| {
            app.set_status_text("Measuring — hold still…".into())
        });
        loop {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            let elapsed = started_at.elapsed();
            if elapsed >= MEASUREMENT_TIMEOUT {
                return Err(
                    "Timed out without a fresh accepted final report. Keep still and try again."
                        .into(),
                );
            }
            let reply=lease.request("status")?;
            if reply["ready"]!=true || reply["spo2"].is_null() {
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            let value=&reply["spo2"];
            let raw=RawReading {
                timestamp_us:value["timestamp_us"].as_u64().ok_or("Invalid SpO2 timestamp")?,
                oxygen:value["oxygen"].as_f64().ok_or("Invalid oxygen")?,
                confidence:value["confidence"].as_f64().ok_or("Invalid confidence")?,
                algorithm:value["algorithm"].as_f64().ok_or("Invalid algorithm")?,
                signal:value["signal"].as_f64().ok_or("Invalid signal")?,
            };
            let elapsed = started_at.elapsed();
            if elapsed >= MEASUREMENT_TIMEOUT {
                return Err("Timed out without an accepted final report.".into());
            }
            let now_us = boottime_micros()?;
            if raw.timestamp_us > now_us.saturating_add(MAX_TIMESTAMP_FUTURE_SKEW_US) {
                return Err(format!(
                    "Sensor timestamp is ahead of CLOCK_BOOTTIME ({}us vs {}us); refusing a clock-mismatched report",
                    raw.timestamp_us, now_us
                ));
            }
            let elapsed_seconds = elapsed.as_secs().min(i32::MAX as u64) as i32;
            post_if_current(weak, control, generation, move |app| {
                app.set_elapsed_seconds(elapsed_seconds);
            });
            if measurement::is_fresh(raw.timestamp_us, floor, &mut last_timestamp) {
                match measurement::inspect(raw) {
                    ReadingDecision::Final(reading) => {
                        let oxygen = reading.oxygen.trunc().clamp(0.0, 100.0) as i32;
                        let confidence = reading.confidence.trunc().clamp(0.0, 100.0) as i32;
                        post_if_current(weak, control, generation, move |app| {
                            app.set_spo2_percentage(oxygen);
                            app.set_spo2_confidence(confidence);
                            app.set_elapsed_seconds(elapsed_seconds);
                            app.set_status_text("Measurement complete".into());
                            app.set_app_state(2);
                        });
                        return Ok(());
                    }
                    ReadingDecision::Progress(message) | ReadingDecision::Rejected(message) => {
                        post_if_current(weak, control, generation, move |app| {
                            app.set_elapsed_seconds(elapsed_seconds);
                            app.set_status_text(message.into());
                        })
                    }
                }
            }
            let wait_until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < wait_until {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    })();
    if let Err(error) = result {
        eprintln!("SpO2 measurement failed: {error}");
        if !cancel.load(Ordering::Acquire) {
            let message = if error.starts_with("Timed out") {
                "No result. Keep still and try again."
            } else if error.contains("Optical sensor busy") {
                "Sensor busy during activity"
            } else if error.starts_with("Baseline spo2Reading failed") {
                "Sensor service unavailable or outdated."
            } else {
                "Sensor unavailable. Try again."
            };
            post_if_current(weak, control, generation, move |app| {
                app.set_app_state(3);
                app.set_error_text(message.into());
                app.set_status_text("Measurement failed".into());
            });
        }
    }
    if let Ok(mut state) = control.lock() {
        state.finish(generation);
    }
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");
    let app = App::new().unwrap();
    let weak = app.as_weak();
    let control = Arc::new(Mutex::new(ControlState {
        generation: 0,
        running: false,
        cancel: None,
    }));
    let (commands, worker_commands) = mpsc::channel();
    let worker_weak = weak.clone();
    let worker_control = Arc::clone(&control);
    let worker = std::thread::spawn(move || {
        while let Ok(command) = worker_commands.recv() {
            match command {
                WorkerCommand::Start { generation, cancel } => {
                    run_measurement(&worker_weak, &worker_control, generation, &cancel)
                }
                WorkerCommand::Shutdown => break,
            }
        }
    });

    let measure_control = Arc::clone(&control);
    let measure_commands = commands.clone();
    let measure_weak = weak.clone();
    app.on_measure(move || {
        let (generation, cancel) = match measure_control.lock() {
            Ok(mut state) => match state.begin() {
                Some(request) => request,
                None => return,
            },
            Err(_) => return,
        };
        if measure_commands
            .send(WorkerCommand::Start { generation, cancel })
            .is_err()
        {
            if let Ok(mut state) = measure_control.lock() {
                state.finish(generation);
            }
            return;
        }
        if let Some(app) = measure_weak.upgrade() {
            app.set_app_state(1);
            app.set_spo2_percentage(0);
            app.set_spo2_confidence(0);
            app.set_elapsed_seconds(0);
            app.set_error_text("".into());
            app.set_status_text("Starting sensor…".into());
        }
    });

    let stop_control = Arc::clone(&control);
    let stop_weak = weak.clone();
    app.on_stop_measure(move || {
        if let Ok(mut state) = stop_control.lock() {
            // The serial worker still owns cleanup for the canceled request;
            // clearing this gate permits a new request to queue behind it.
            state.stop();
        }
        if let Some(app) = stop_weak.upgrade() {
            app.set_app_state(0);
            app.set_status_text("Stopped".into());
            app.set_error_text("".into());
        }
    });
    let _ = app.run();
    if let Ok(mut state) = control.lock() {
        state.stop();
    }
    let _ = commands.send(WorkerCommand::Shutdown);
    let _ = worker.join();
}

#[cfg(test)]
mod lifecycle_tests {
    use super::ControlState;
    use std::sync::atomic::Ordering;

    #[test]
    fn canceled_worker_cannot_wedge_or_clear_a_new_request() {
        let mut state = ControlState {
            generation: 0,
            running: false,
            cancel: None,
        };
        let (old_generation, old_cancel) = state.begin().expect("first request");
        state.stop();
        assert!(old_cancel.load(Ordering::Acquire));
        assert!(!state.running);
        state.finish(old_generation);
        let (new_generation, new_cancel) = state.begin().expect("request after stop");
        assert_ne!(old_generation, new_generation);
        assert!(!new_cancel.load(Ordering::Acquire));
        state.finish(old_generation);
        assert!(state.running, "old cleanup must not clear the new request");
        assert!(
            state.begin().is_none(),
            "only one current request is allowed"
        );
        state.finish(new_generation);
        assert!(!state.running);
    }
}
