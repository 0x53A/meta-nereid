slint::include_modules!();

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
pub mod measurement;
use measurement::{RawReading, ReadingDecision};
use zbus::blocking::{connection::Builder as ConnectionBuilder, Connection};
use zbus::names::{BusName, InterfaceName, MemberName};
use zbus::zvariant::ObjectPath;

const SERVICE: &str = "com.nokia.SensorService";
const MANAGER_PATH: &str = "/SensorManager";
const MANAGER_IFACE: &str = "local.SensorManager";
const SENSOR_ID: &str = "spo2sensor";
const SENSOR_PATH: &str = "/SensorManager/spo2sensor";
const SENSOR_IFACE: &str = "local.Spo2Sensor";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const SENSOR_SOCKET: &str = "/run/sensord.sock";
const METHOD_TIMEOUT: Duration = Duration::from_secs(3);
const MEASUREMENT_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_TIMESTAMP_FUTURE_SKEW_US: u64 = 5_000_000;

fn bus_name() -> BusName<'static> {
    BusName::try_from(SERVICE).unwrap()
}

fn open_connection() -> Result<Connection, String> {
    ConnectionBuilder::system()
        .map_err(|e| format!("D-Bus system connection: {e}"))?
        .method_timeout(METHOD_TIMEOUT)
        .build()
        .map_err(|e| format!("D-Bus system connection: {e}"))
}

fn load_plugin(conn: &Connection) -> Result<(), String> {
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(MANAGER_PATH).unwrap(),
            Some(InterfaceName::try_from(MANAGER_IFACE).unwrap()),
            MemberName::try_from("loadPlugin").unwrap(),
            &(SENSOR_ID,),
        )
        .map_err(|e| format!("loadPlugin({SENSOR_ID}): {e}"))?;
    let ok: bool = reply
        .body()
        .deserialize()
        .map_err(|e| format!("loadPlugin parse: {e}"))?;
    if !ok {
        return Err(format!("loadPlugin({SENSOR_ID}) returned false"));
    }
    Ok(())
}

fn request_sensor(conn: &Connection) -> Result<i32, String> {
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(MANAGER_PATH).unwrap(),
            Some(InterfaceName::try_from(MANAGER_IFACE).unwrap()),
            MemberName::try_from("requestSensor").unwrap(),
            &(SENSOR_ID, std::process::id() as i64),
        )
        .map_err(|e| format!("requestSensor({SENSOR_ID}): {e}"))?;
    let session_id: i32 = reply
        .body()
        .deserialize()
        .map_err(|e| format!("requestSensor parse: {e}"))?;
    if session_id < 0 {
        return Err(format!(
            "requestSensor returned invalid session id {session_id}"
        ));
    }
    Ok(session_id)
}

// start/stop are on the sensor-specific interface, not local.AbstractSensor.
fn start_sensor(conn: &Connection, session_id: i32) -> Result<(), String> {
    conn.call_method(
        Some(bus_name()),
        ObjectPath::try_from(SENSOR_PATH).unwrap(),
        Some(InterfaceName::try_from(SENSOR_IFACE).unwrap()),
        MemberName::try_from("start").unwrap(),
        &(session_id,),
    )
    .map_err(|e| format!("start({SENSOR_ID}, session {session_id}): {e}"))?;
    Ok(())
}

fn stop_sensor(conn: &Connection, session_id: i32) -> Result<(), String> {
    conn.call_method(
        Some(bus_name()),
        ObjectPath::try_from(SENSOR_PATH).unwrap(),
        Some(InterfaceName::try_from(SENSOR_IFACE).unwrap()),
        MemberName::try_from("stop").unwrap(),
        &(session_id,),
    )
    .map_err(|e| format!("stop({SENSOR_ID}, session {session_id}): {e}"))?;
    Ok(())
}

fn release_sensor(conn: &Connection, session_id: i32) -> Result<(), String> {
    conn.call_method(
        Some(bus_name()),
        ObjectPath::try_from(MANAGER_PATH).unwrap(),
        Some(InterfaceName::try_from(MANAGER_IFACE).unwrap()),
        MemberName::try_from("releaseSensor").unwrap(),
        &(SENSOR_ID, session_id, std::process::id() as i64),
    )
    .map_err(|e| format!("releaseSensor({SENSOR_ID}, session {session_id}): {e}"))?;
    Ok(())
}

/// Read additive `local.Spo2Sensor.spo2Reading` `(tdddd)`. Keep raw fields
/// intact until `measurement::inspect` performs every finite/range/enum check.
fn read_spo2(conn: &Connection) -> Result<RawReading, String> {
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(SENSOR_PATH).unwrap(),
            Some(InterfaceName::try_from(PROPS_IFACE).unwrap()),
            MemberName::try_from("Get").unwrap(),
            &(SENSOR_IFACE, "spo2Reading"),
        )
        .map_err(|e| format!("Get local.Spo2Sensor.spo2Reading: {e}"))?;
    let variant: zbus::zvariant::OwnedValue = reply
        .body()
        .deserialize()
        .map_err(|e| format!("Get spo2Reading variant: {e}"))?;
    let (timestamp_us, oxygen, confidence, algorithm, signal): (u64, f64, f64, f64, f64) =
        zbus::zvariant::OwnedValue::try_into(variant)
            .map_err(|e| format!("Get spo2Reading tuple (tdddd): {e}"))?;
    Ok(RawReading {
        timestamp_us,
        oxygen,
        confidence,
        algorithm,
        signal,
    })
}

// Follows imu-test-app's handshake and drain protocol. Holding this stream
// for the whole session prevents sensorfw's no-data socket expiry.
fn connect_stream(session_id: i32) -> Result<UnixStream, String> {
    let mut stream =
        UnixStream::connect(SENSOR_SOCKET).map_err(|e| format!("connect {SENSOR_SOCKET}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("socket read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("socket write timeout: {e}"))?;
    stream
        .write_all(&session_id.to_ne_bytes())
        .map_err(|e| format!("sensor socket session handshake: {e}"))?;
    let mut tag = [0];
    stream
        .read_exact(&mut tag)
        .map_err(|e| format!("sensor socket handshake greeting: {e}"))?;
    if tag != [b'\n'] {
        return Err("sensor socket returned an invalid greeting".into());
    }
    stream
        .set_nonblocking(true)
        .map_err(|e| format!("sensor socket nonblocking mode: {e}"))?;
    Ok(stream)
}

fn drain_stream(stream: &mut UnixStream) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    for _ in 0..64 {
        match stream.read(&mut buffer) {
            Ok(0) => return Err("sensor socket disconnected".into()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("sensor socket read: {e}")),
        }
    }
    Ok(())
}

struct SensorSession {
    conn: Connection,
    session_id: i32,
    stream: Option<UnixStream>,
    started: bool,
}
impl SensorSession {
    fn start(&mut self) -> Result<(), String> {
        // Arm cleanup before entering D-Bus: a timeout can leave the daemon's
        // session active even when the client sees an error.
        self.started = true;
        start_sensor(&self.conn, self.session_id)
    }
}
impl Drop for SensorSession {
    fn drop(&mut self) {
        if self.started {
            if let Err(error) = stop_sensor(&self.conn, self.session_id) {
                eprintln!("SpO2 cleanup stop failed: {error}");
            }
        }
        if let Err(error) = release_sensor(&self.conn, self.session_id) {
            eprintln!("SpO2 cleanup release failed: {error}");
        }
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
        let conn = open_connection()?;
        load_plugin(&conn)?;
        let session_id = request_sensor(&conn)?;
        let mut session = SensorSession {
            conn,
            session_id,
            stream: None,
            started: false,
        };
        let baseline = read_spo2(&session.conn).map_err(|error| format!("Baseline spo2Reading failed; sensorfw update required (no legacy fallback): {error}"))?;
        session.stream = Some(connect_stream(session_id)?);
        let start_floor = boottime_micros()?;
        if baseline.timestamp_us > start_floor.saturating_add(MAX_TIMESTAMP_FUTURE_SKEW_US) {
            return Err(format!(
                "Baseline sensor timestamp is ahead of CLOCK_BOOTTIME ({}us vs {}us); refusing a clock-mismatched channel",
                baseline.timestamp_us, start_floor
            ));
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        session.start()?;
        let floor = baseline.timestamp_us.max(start_floor);
        let started_at = Instant::now();
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
            drain_stream(session.stream.as_mut().expect("session stream installed"))?;
            let raw = read_spo2(&session.conn)?;
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
