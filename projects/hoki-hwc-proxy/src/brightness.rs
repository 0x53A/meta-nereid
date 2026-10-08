//! Interactive brightness, serialized with HWC/Sidekick ownership transitions.
use crate::{brightness_config::Config, sleep_client::Client};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Duration,
};
use zbus::blocking::{Connection, Proxy};

const BACKLIGHT: &str = "/sys/class/leds/lcd-backlight";
const STATUS: &str = "/run/hoki-hwc-proxy/brightness.json";
struct State {
    interactive: bool,
    sensor: Option<Als>,
    applied: Option<u32>,
    retry_at: f64,
}
pub struct Controller(Arc<(Mutex<State>, Condvar)>);
fn wait_for_display(shared: &(Mutex<State>, Condvar)) -> MutexGuard<'_, State> {
    shared.1.wait_while(shared.0.lock().unwrap(), |s| !s.interactive).unwrap()
}
impl Controller {
    pub fn start() -> Self {
        let state = Arc::new((Mutex::new(State {
            interactive: true,
            sensor: None,
            applied: None,
            retry_at: 0.,
        }), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            loop {
                let (lock, wake) = &*shared;
                // No timer, coordinator polling, status-file writes, or ALS
                // traffic while off or while Sidekick owns the display.
                drop(wait_for_display(&shared));
                let result = (|| -> Result<()> {
                    let reply = Client::connect()?.request(json!({"command":"status"}))?;
                    let config = Config::from_status(&reply["config"]["brightness"])
                        .map_err(anyhow::Error::msg)?;
                    // A coordinator request may have been in flight when the
                    // display turned off. Never apply it after the handoff.
                    let mut state = lock.lock().unwrap();
                    if !state.interactive { return Ok(()); }
                    let status = state.apply(&config);
                    let value = json!({"at":now(),"config":config,"status":status});
                    fs::create_dir_all("/run/hoki-hwc-proxy")?;
                    fs::write(format!("{STATUS}.pending"), serde_json::to_vec(&value)?)?;
                    fs::rename(format!("{STATUS}.pending"), STATUS)?;
                    Ok(())
                })();
                if result.is_err() {
                    // A missing coordinator must not leave a sensor session running.
                    lock.lock().unwrap().sensor = None;
                }
                let state = lock.lock().unwrap();
                drop(wake.wait_timeout_while(state, Duration::from_secs(1), |s| s.interactive).unwrap());
            }
        });
        Self(state)
    }
    pub fn transition(&self, interactive: bool, change: impl FnOnce() -> Result<()>) -> Result<()> {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        state.interactive = false;
        state.sensor = None;
        state.applied = None;
        wake.notify_all();
        // No sysfs writes or live ALS session during HWC/Sidekick transitions.
        change()?;
        state.interactive = interactive;
        state.retry_at = 0.;
        wake.notify_all();
        Ok(())
    }
}
fn now() -> f64 {
    crate::sleep_client::boottime().unwrap_or(f64::INFINITY)
}
impl State {
    fn apply(&mut self, config: &Config) -> String {
        if !self.interactive {
            self.sensor = None;
            return "Saved for interactive display".into();
        }
        let mut level = config.level;
        let mut status = String::new();
        if config.automatic {
            let sample = (|| -> Result<Option<u32>> {
                if self.sensor.is_none() {
                    if now() < self.retry_at {
                        bail!("sensor retry pending");
                    }
                    self.sensor = Some(Als::start()?);
                }
                self.sensor.as_mut().unwrap().lux()
            })();
            match sample {
                Ok(Some(lux)) => {
                    level = automatic_level(lux);
                    // Small changes should not make a steady scene flicker.
                    if let Some(old) = self.applied {
                        if old.abs_diff(level) < 3 {
                            level = old;
                        }
                    }
                    status = format!("Auto · {level}%");
                }
                Ok(None) => {
                    status = "Waiting for light sensor".into();
                }
                Err(error) => {
                    self.sensor = None;
                    if now() >= self.retry_at {
                        tracing::warn!(%error, "Automatic brightness light sensor unavailable");
                        self.retry_at = now() + 10.;
                    }
                    status = "Light sensor unavailable; using manual level".into();
                }
            }
        } else {
            self.sensor = None;
        }
        if self.applied != Some(level) {
            match write_level(level) {
                Ok(()) => self.applied = Some(level),
                Err(_) => return "Could not apply display brightness".into(),
            }
        }
        status
    }
}
fn write_level(percent: u32) -> Result<()> {
    let maximum: u32 = fs::read_to_string(format!("{BACKLIGHT}/max_brightness"))?
        .trim()
        .parse()?;
    if maximum == 0 {
        bail!("invalid backlight range");
    }
    fs::write(
        format!("{BACKLIGHT}/brightness"),
        hardware_level(percent, maximum).to_string(),
    )?;
    Ok(())
}
fn hardware_level(percent: u32, maximum: u32) -> u32 {
    ((u64::from(percent.clamp(1, 100)) * u64::from(maximum) / 100).max(1)) as u32
}
fn automatic_level(lux: u32) -> u32 {
    // Gentle logarithmic ramp, with a readable floor in darkness.
    (10. + 25. * (1. + lux as f64).log10())
        .round()
        .clamp(10., 100.) as u32
}

struct Als {
    conn: Connection,
    service: String,
    id: i32,
    stream: Option<UnixStream>,
    floor: u64,
}
impl Als {
    fn manager<'a>(conn: &'a Connection, service: &'a str) -> zbus::Result<Proxy<'a>> {
        Proxy::new(conn, service, "/SensorManager", "local.SensorManager")
    }
    fn proxy(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(
            &self.conn,
            self.service.as_str(),
            "/SensorManager/alssensor",
            "local.ALSSensor",
        )
    }
    fn start() -> Result<Self> {
        let conn = zbus::blocking::connection::Builder::system()?
            .method_timeout(Duration::from_millis(750))
            .build()?;
        // Bind the current unique owner. Never auto-activate sensorfwd when
        // a recording session has deliberately stopped it to own the HAL.
        let service = zbus::blocking::fdo::DBusProxy::new(&conn)?
            .get_name_owner("com.nokia.SensorService".try_into()?)?
            .to_string();
        let manager = Self::manager(&conn, &service)?;
        let loaded: bool = manager.call("loadPlugin", &("alssensor",))?;
        if !loaded {
            bail!("ALS plugin unavailable");
        }
        let id: i32 = manager.call("requestSensor", &("alssensor", std::process::id() as i64))?;
        if id < 0 {
            bail!("ALS request rejected");
        }
        drop(manager);
        let mut sensor = Self {
            conn,
            service,
            id,
            stream: None,
            floor: (now() * 1e6) as u64,
        };
        let mut stream = UnixStream::connect("/run/sensord.sock")?;
        stream.set_read_timeout(Some(Duration::from_millis(750)))?;
        stream.set_write_timeout(Some(Duration::from_millis(750)))?;
        stream.write_all(&id.to_ne_bytes())?;
        let mut greeting = [0];
        stream.read_exact(&mut greeting)?;
        if greeting != [b'\n'] {
            bail!("invalid sensor greeting");
        }
        stream.set_nonblocking(true)?;
        sensor.stream = Some(stream);
        sensor.proxy()?.call::<_, _, ()>("start", &(id,))?;
        Ok(sensor)
    }
    fn lux(&mut self) -> Result<Option<u32>> {
        let mut bytes = [0u8; 4096];
        for _ in 0..16 {
            match self
                .stream
                .as_mut()
                .context("missing ALS stream")?
                .read(&mut bytes)
            {
                Ok(0) => bail!("ALS disconnected"),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        let reply = self.conn.call_method(
            Some(self.service.as_str()),
            "/SensorManager/alssensor",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("local.ALSSensor", "lux"),
        )?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        let (_timestamp, lux): (u64, u32) = value.try_into()?;
        // sensorfw ALSSensorChannel caches only value_, leaving the property's
        // timestamp at zero, and suppresses repeated integer lux samples. Allow
        // the first conversion after start; do not interpret zero timestamps
        // or a quiet stream as a sensor failure.
        if now() * 1e6 < self.floor as f64 + 1_000_000. {
            return Ok(None);
        }
        Ok(Some(lux))
    }
}
impl Drop for Als {
    fn drop(&mut self) {
        if let Ok(proxy) = self.proxy() {
            let _ = proxy.call::<_, _, ()>("stop", &(self.id,));
        }
        if let Ok(manager) = Self::manager(&self.conn, &self.service) {
            let _ = manager.call::<_, _, bool>(
                "releaseSensor",
                &("alssensor", self.id, std::process::id() as i64),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_worker_waits_for_display_transition() {
        let state = Arc::new((Mutex::new(State {
            interactive: false, sensor: None, applied: None, retry_at: 0.,
        }), Condvar::new()));
        let worker_state = state.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let state = wait_for_display(&worker_state);
            tx.send(state.interactive).unwrap();
        });
        // Even a spurious notification must not cause a coordinator/ALS poll.
        state.1.notify_all();
        assert!(rx.recv_timeout(Duration::from_millis(40)).is_err());
        let controller = Controller(state);
        assert!(controller.transition(true, || bail!("handoff failed")).is_err());
        assert!(rx.recv_timeout(Duration::from_millis(40)).is_err());
        controller.transition(true, || Ok(())).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }
    #[test]
    fn display_transitions_gate_hardware_and_fail_closed() {
        let state = Arc::new((Mutex::new(State {
            interactive: true,
            sensor: None,
            applied: Some(50),
            retry_at: 10.,
        }), Condvar::new()));
        let controller = Controller(state.clone());
        controller.transition(false, || Ok(())).unwrap();
        {
            let mut state = state.0.lock().unwrap();
            assert!(!state.interactive);
            assert_eq!(state.applied, None);
            assert_eq!(
                state.apply(&Config {
                    level: 35,
                    automatic: true,
                    ambient_automatic: true,
                }),
                "Saved for interactive display"
            );
            assert!(state.sensor.is_none());
        }
        assert!(
            controller
                .transition(true, || anyhow::bail!("handoff failed"))
                .is_err()
        );
        assert!(!state.0.lock().unwrap().interactive);
        controller.transition(true, || Ok(())).unwrap();
        assert!(state.0.lock().unwrap().interactive);
    }
    #[test]
    fn manual_range_never_blanks_display_and_preserves_default() {
        assert_eq!(hardware_level(50, 255), 127);
        assert_eq!(hardware_level(1, 255), 2);
        assert_eq!(hardware_level(100, 255), 255);
        assert_eq!(hardware_level(1, 10), 1);
    }
    #[test]
    fn auto_curve_is_bounded_and_monotonic() {
        let mut previous = 0;
        for lux in [0, 1, 10, 100, 1000, 10000, u32::MAX] {
            let level = automatic_level(lux);
            assert!((10..=100).contains(&level));
            assert!(level >= previous);
            previous = level;
        }
    }
}
