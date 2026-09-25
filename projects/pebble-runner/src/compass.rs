//! Sensorfw compass bridge. Pebble angles increase counter-clockwise, in 1/65536 turns.
use std::time::{Duration, Instant};
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heading {
    pub magnetic_heading: i32,
    pub true_heading: i32,
    pub compass_status: i32,
    pub is_declination_valid: bool,
}
impl Heading {
    const UNAVAILABLE: Self = Self {
        magnetic_heading: 0,
        true_heading: 0,
        compass_status: -1,
        is_declination_valid: false,
    };
}
fn convert(timestamp: u64, degrees: f64, level: i32) -> Heading {
    if timestamp == 0 || !(0.0..360.0).contains(&degrees) {
        return Heading {
            compass_status: 0,
            ..Heading::UNAVAILABLE
        };
    }
    let angle = (((360.0 - degrees).rem_euclid(360.0) * 65536.0 / 360.0).round() as i32) % 65536;
    Heading {
        magnetic_heading: angle,
        true_heading: angle,
        compass_status: if level >= 3 {
            2
        } else if level > 0 {
            1
        } else {
            0
        },
        is_declination_valid: false,
    }
}
fn changed(old: Heading, new: Heading, filter: i32) -> bool {
    let delta = (new.magnetic_heading - old.magnetic_heading).abs();
    old.compass_status != new.compass_status || delta.min(65536 - delta) >= filter
}
static mut HANDLER: Option<extern "C" fn(Heading)> = None;
static mut LAST: Option<Heading> = None;
static mut FILTER: i32 = 65536 / 360;
static mut NEXT_POLL: Option<Instant> = None;
#[cfg(target_arch = "arm")]
static mut SENSOR: Option<Sensor> = None;
#[cfg(target_arch = "arm")]
struct Sensor {
    connection: zbus::blocking::Connection,
    session: i32,
    stream: Option<std::os::unix::net::UnixStream>,
    timestamp: u64,
    fresh_at: Instant,
}
#[cfg(target_arch = "arm")]
impl Sensor {
    fn start() -> Result<Self, String> {
        let connection = zbus::blocking::connection::Builder::system()
            .map_err(|e| format!("connect: {e}"))?
            .method_timeout(Duration::from_millis(300))
            .build()
            .map_err(|e| format!("connect: {e}"))?;
        let bus = Some("com.nokia.SensorService");
        let loaded = connection
            .call_method(
                bus,
                "/SensorManager",
                Some("local.SensorManager"),
                "loadPlugin",
                &("compasssensor",),
            )
            .map_err(|e| format!("loadPlugin: {e}"))?
            .body()
            .deserialize::<bool>()
            .map_err(|e| format!("plugin reply: {e}"))?;
        if !loaded {
            return Err("compass plugin unavailable".into());
        }
        let session = connection
            .call_method(
                bus,
                "/SensorManager",
                Some("local.SensorManager"),
                "requestSensor",
                &("compasssensor", std::process::id() as i64),
            )
            .map_err(|e| format!("requestSensor: {e}"))?
            .body()
            .deserialize::<i32>()
            .map_err(|e| format!("session reply: {e}"))?;
        if session < 0 {
            return Err(format!("invalid session: {session}"));
        }
        let mut sensor = Self {
            connection,
            session,
            stream: None,
            timestamp: 0,
            fresh_at: Instant::now(),
        };
        // Sensorfw retires D-Bus-only sessions after 10 seconds. Keep its data
        // socket connected even though we read the portable D-Bus representation.
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect("/run/sensord.sock")
            .map_err(|e| format!("sensor socket: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(300)))
            .map_err(|e| e.to_string())?;
        let mut greeting = [0];
        stream
            .read_exact(&mut greeting)
            .map_err(|e| format!("sensor greeting: {e}"))?;
        if greeting != [b'\n'] {
            return Err("invalid sensor socket greeting".into());
        }
        stream
            .write_all(&session.to_ne_bytes())
            .map_err(|e| format!("sensor session: {e}"))?;
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        sensor.stream = Some(stream);
        // Dropping on failure releases the session as well.
        sensor
            .connection
            .call_method(
                bus,
                "/SensorManager/compasssensor",
                Some("local.CompassSensor"),
                "start",
                &(session,),
            )
            .map_err(|e| format!("start: {e}"))?;
        Ok(sensor)
    }
    fn read(&mut self) -> Result<Heading, String> {
        // Drain unused binary samples to avoid building a queue in sensorfw.
        use std::io::Read;
        let mut buffer = [0; 4096];
        for _ in 0..64 {
            match self.stream.as_mut().unwrap().read(&mut buffer) {
                Ok(0) => return Err("sensor socket closed".into()),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("sensor stream: {e}")),
            }
        }
        let reply = self
            .connection
            .call_method(
                Some("com.nokia.SensorService"),
                "/SensorManager/compasssensor",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("local.CompassSensor", "value"),
            )
            .map_err(|e| format!("read: {e}"))?;
        let value = reply
            .body()
            .deserialize::<zbus::zvariant::OwnedValue>()
            .map_err(|e| format!("value: {e}"))?;
        // Sensorfw builds expose angles as either qreal/double or int32.
        let floats = value.try_clone().map_err(|e| format!("heading: {e}"))?;
        let (time, magnetic, level) = match <(u64, f64, f64, f64, i32)>::try_from(floats) {
            Ok((time, _, magnetic, _, level)) => (time, magnetic, level),
            Err(_) => {
                let (time, _, magnetic, _, level): (u64, i32, i32, i32, i32) =
                    value.try_into().map_err(|e| format!("heading: {e}"))?;
                (time, magnetic as f64, level)
            }
        };
        if time != self.timestamp {
            self.timestamp = time;
            self.fresh_at = Instant::now();
        }
        let mut heading = convert(time, magnetic, level);
        if self.fresh_at.elapsed() > Duration::from_secs(2) {
            heading.compass_status = 0;
        }
        Ok(heading)
    }
}
#[cfg(target_arch = "arm")]
impl Drop for Sensor {
    fn drop(&mut self) {
        let _ = self.connection.call_method(
            Some("com.nokia.SensorService"),
            "/SensorManager/compasssensor",
            Some("local.CompassSensor"),
            "stop",
            &(self.session,),
        );
        let _ = self.connection.call_method(
            Some("com.nokia.SensorService"),
            "/SensorManager",
            Some("local.SensorManager"),
            "releaseSensor",
            &("compasssensor", self.session, std::process::id() as i64),
        );
        // The private D-Bus connection closes here, also retiring orphaned sessions.
    }
}
fn read() -> Heading {
    #[cfg(target_arch = "arm")]
    unsafe {
        if SENSOR.is_none() {
            match Sensor::start() {
                Ok(sensor) => SENSOR = Some(sensor),
                Err(error) => {
                    eprintln!("[compass] {error}");
                    return Heading::UNAVAILABLE;
                }
            }
        }
        match SENSOR.as_mut().unwrap().read() {
            Ok(heading) => return heading,
            Err(error) => {
                eprintln!("[compass] {error}");
                SENSOR = None;
                return Heading::UNAVAILABLE;
            }
        }
    }
    #[cfg(not(target_arch = "arm"))]
    Heading::UNAVAILABLE
}
pub fn peek(data: *mut Heading) -> i32 {
    if data.is_null() {
        return -4;
    }
    let heading = read();
    unsafe {
        data.write(heading);
        #[cfg(target_arch = "arm")]
        if HANDLER.is_none() {
            SENSOR = None;
        }
    }
    0
}
pub fn set_filter(filter: i32) -> i32 {
    if !(0..=32768).contains(&filter) {
        return -4;
    }
    unsafe {
        FILTER = filter;
    }
    0
}
pub fn subscribe(handler: Option<extern "C" fn(Heading)>) {
    unsafe {
        HANDLER = handler;
        LAST = None;
        NEXT_POLL = None;
    }
    if handler.is_none() {
        reset();
    }
}
pub fn has_handler() -> bool {
    unsafe { HANDLER.is_some() }
}
pub fn poll() -> bool {
    let Some(handler) = (unsafe { HANDLER }) else {
        return false;
    };
    let now = Instant::now();
    if unsafe { NEXT_POLL.is_some_and(|deadline| now < deadline) } {
        return false;
    }
    let heading = read();
    unsafe {
        NEXT_POLL = Some(
            now + if heading.compass_status < 0 {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(250)
            },
        );
        if LAST.is_some_and(|old| !changed(old, heading, FILTER)) {
            return false;
        }
        LAST = Some(heading);
    }
    handler(heading);
    true
}
pub fn reset() {
    unsafe {
        HANDLER = None;
        LAST = None;
        NEXT_POLL = None;
        FILTER = 65536 / 360;
        #[cfg(target_arch = "arm")]
        {
            SENSOR = None;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heading_direction_status_and_wraparound_filter() {
        assert_eq!(std::mem::size_of::<Heading>(), 16);
        assert_eq!(convert(1, 90.0, 3).magnetic_heading, 49152);
        assert_eq!(convert(1, 180.0, 3).magnetic_heading, 32768);
        assert_eq!(convert(0, 90.0, 3).compass_status, 0);
        assert_eq!(convert(1, 90.0, 1).compass_status, 1);
        assert!(!convert(1, 90.0, 3).is_declination_valid);
        assert!(!changed(convert(1, 359.0, 3), convert(1, 0.0, 3), 400));
        assert!(changed(convert(1, 359.0, 1), convert(1, 0.0, 3), 400));
    }
}

#[cfg(all(test, target_arch = "arm"))]
pub fn sample_timestamp() -> u64 {
    unsafe { SENSOR.as_ref().map_or(0, |s| s.timestamp) }
}
