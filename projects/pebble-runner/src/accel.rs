//! Accelerometer service for the Pebble compat layer.
//!
//! On ARM (watch): reads from sensorfw via D-Bus.
//! On desktop (emulator): returns zero data.

use std::time::Instant;

/// Pebble AccelData — packed to match C ABI exactly.
/// ```c
/// typedef struct __attribute__((__packed__)) AccelData {
///   int16_t x, y, z;
///   bool did_vibrate;
///   uint64_t timestamp;
/// } AccelData; // 15 bytes
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AccelData {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub did_vibrate: u8,
    pub timestamp: u64,
}

/// AccelDataHandler: void (*)(AccelData *data, uint32_t num_samples)
pub type AccelDataHandler = extern "C" fn(*mut AccelData, u32);

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static mut ACCEL_HANDLER: Option<AccelDataHandler> = None;
static mut ACCEL_SAMPLES_PER_UPDATE: u32 = 0;
static mut ACCEL_SAMPLING_RATE: u32 = 25; // default 25 Hz
static mut ACCEL_LAST_POLL: Option<Instant> = None;
static mut ACCEL_LATEST: AccelData = AccelData {
    x: 0,
    y: 0,
    z: -1000, // -1G (gravity pointing down when face-up)
    did_vibrate: 0,
    timestamp: 0,
};

#[cfg(target_arch = "arm")]
static mut DBUS_CONN: Option<zbus::blocking::Connection> = None;
#[cfg(target_arch = "arm")]
static mut SENSOR_SESSION: Option<i32> = None;

/// Reset accel state between app launches.
pub fn reset() {
    #[cfg(target_arch = "arm")]
    stop_sensor();
    unsafe {
        ACCEL_HANDLER = None;
        ACCEL_SAMPLES_PER_UPDATE = 0;
        ACCEL_SAMPLING_RATE = 25;
        ACCEL_LAST_POLL = None;
        ACCEL_LATEST = AccelData {
            x: 0,
            y: 0,
            z: -1000,
            did_vibrate: 0,
            timestamp: 0,
        };
        #[cfg(target_arch = "arm")]
        {
            SENSOR_SESSION = None;
            // The old connection/session was closed before clearing callbacks.
        }
    }
}

// ---------------------------------------------------------------------------
// Sensorfw D-Bus (ARM only)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "arm")]
fn ensure_sensor_started() {
    use zbus::blocking::Connection;
    use zbus::names::{BusName, InterfaceName, MemberName};
    use zbus::zvariant::ObjectPath;

    unsafe {
        if SENSOR_SESSION.is_some() {
            return;
        }

        let conn = match &DBUS_CONN {
            Some(c) => c,
            None => {
                match Connection::system() {
                    Ok(c) => {
                        DBUS_CONN = Some(c);
                        DBUS_CONN.as_ref().unwrap()
                    }
                    Err(e) => {
                        eprintln!("[accel] D-Bus connect failed: {e}");
                        return;
                    }
                }
            }
        };

        let bus = BusName::try_from("com.nokia.SensorService").unwrap();
        let mgr_path = ObjectPath::try_from("/SensorManager").unwrap();
        let mgr_iface = InterfaceName::try_from("local.SensorManager").unwrap();

        // loadPlugin
        let _ = conn.call_method(
            Some(bus.clone()),
            mgr_path.clone(),
            Some(mgr_iface.clone()),
            MemberName::try_from("loadPlugin").unwrap(),
            &("accelerometersensor",),
        );

        // requestSensor
        let pid = std::process::id() as i64;
        let reply = conn.call_method(
            Some(bus.clone()),
            mgr_path,
            Some(mgr_iface),
            MemberName::try_from("requestSensor").unwrap(),
            &("accelerometersensor", pid),
        );

        if let Ok(reply) = reply {
            let body = reply.body();
            if let Ok(session_id) = body.deserialize::<i32>() {
                if session_id < 0 { return; }
                // Retain the session even if startup fails so it can be released.
                SENSOR_SESSION = Some(session_id);
                let sensor_path = ObjectPath::try_from("/SensorManager/accelerometersensor").unwrap();
                let sensor_iface = InterfaceName::try_from("local.AccelerometerSensor").unwrap();
                let started = conn.call_method(
                    Some(bus),
                    sensor_path,
                    Some(sensor_iface),
                    MemberName::try_from("start").unwrap(),
                    &(session_id,),
                );
                if started.is_err() { stop_sensor(); return; }
                eprintln!("[accel] sensorfw started (session {})", session_id);
            }
        }
    }
}

#[cfg(target_arch = "arm")]
fn stop_sensor() {
    unsafe {
        // Closing this private connection also lets sensorfw retire sessions if
        // a stop/release reply is lost (SensorManager::dbusClientUnregistered).
        let conn = DBUS_CONN.take();
        let session = SENSOR_SESSION.take();
        if let Some(conn) = conn {
            if let Some(session) = session {
                let _ = conn.call_method(Some("com.nokia.SensorService"),
                    "/SensorManager/accelerometersensor", Some("local.AccelerometerSensor"), "stop", &(session,));
                let _ = conn.call_method(Some("com.nokia.SensorService"),
                    "/SensorManager", Some("local.SensorManager"), "releaseSensor",
                    &("accelerometersensor", session, std::process::id() as i64));
            }
            let _ = conn.close();
        }
    }
}

#[cfg(target_arch = "arm")]
fn read_sensorfw() -> Option<(i16, i16, i16)> {
    use zbus::names::{BusName, InterfaceName, MemberName};
    use zbus::zvariant::ObjectPath;

    unsafe {
        let conn = DBUS_CONN.as_ref()?;
        let bus = BusName::try_from("com.nokia.SensorService").unwrap();
        let path = ObjectPath::try_from("/SensorManager/accelerometersensor").unwrap();
        let props_iface = InterfaceName::try_from("org.freedesktop.DBus.Properties").unwrap();

        let reply = conn.call_method(
            Some(bus),
            path,
            Some(props_iface),
            MemberName::try_from("Get").unwrap(),
            &("local.AccelerometerSensor", "xyz"),
        ).ok()?;

        let body = reply.body();
        let variant: zbus::zvariant::OwnedValue = body.deserialize().ok()?;
        // (tddd) = (u64, f64, f64, f64) — timestamp + x,y,z in milli-G
        let (_, x, y, z): (u64, f64, f64, f64) =
            zbus::zvariant::OwnedValue::try_into(variant).ok()?;

        // Pebble AccelData x/y/z are int16 milli-G
        Some((x as i16, y as i16, z as i16))
    }
}

/// Poll the hardware and update ACCEL_LATEST.
fn update_latest() {
    #[cfg(target_arch = "arm")]
    {
        ensure_sensor_started();
        if let Some((x, y, z)) = read_sensorfw() {
            let mut now_ms: libc::time_t = 0;
            unsafe { libc::time(&mut now_ms); }
            unsafe {
                ACCEL_LATEST = AccelData {
                    x,
                    y,
                    z,
                    did_vibrate: 0,
                    timestamp: now_ms as u64 * 1000,
                };
            }
        }
    }

    #[cfg(not(target_arch = "arm"))]
    {
        // Desktop: just leave the default zero data
    }
}

// ---------------------------------------------------------------------------
// Pebble API functions (called from jump table on ARM)
// ---------------------------------------------------------------------------

/// accel_data_service_subscribe(samples_per_update, handler)
#[no_mangle]
pub extern "C" fn pbl_accel_data_service_subscribe(samples_per_update: u32, handler: AccelDataHandler) {
    println!("[accel] subscribe(samples_per_update={}, handler={:p})",
        samples_per_update, handler as *const ());
    unsafe {
        ACCEL_HANDLER = Some(handler);
        ACCEL_SAMPLES_PER_UPDATE = if samples_per_update == 0 { 1 } else { samples_per_update };
        ACCEL_LAST_POLL = None;
    }
}

/// accel_data_service_unsubscribe()
#[no_mangle]
pub extern "C" fn pbl_accel_data_service_unsubscribe() {
    #[cfg(target_arch = "arm")]
    stop_sensor();
    println!("[accel] unsubscribe");
    unsafe {
        ACCEL_HANDLER = None;
        ACCEL_SAMPLES_PER_UPDATE = 0;
    }
}

/// accel_service_set_sampling_rate(rate) — rate is enum {10,25,50,100}
#[no_mangle]
pub extern "C" fn pbl_accel_service_set_sampling_rate(rate: u32) -> i32 {
    println!("[accel] set_sampling_rate({})", rate);
    unsafe {
        ACCEL_SAMPLING_RATE = match rate {
            10 | 25 | 50 | 100 => rate,
            _ => 25,
        };
    }
    0
}

/// accel_service_set_samples_per_update(num)
#[no_mangle]
pub extern "C" fn pbl_accel_service_set_samples_per_update(num: u32) -> i32 {
    println!("[accel] set_samples_per_update({})", num);
    unsafe {
        ACCEL_SAMPLES_PER_UPDATE = if num == 0 { 1 } else { num };
    }
    0
}

/// accel_service_peek(data: *mut AccelData) -> int (0=success)
#[no_mangle]
pub extern "C" fn pbl_accel_service_peek(data: *mut AccelData) -> i32 {
    if data.is_null() { return -1; }
    update_latest();
    unsafe {
        *data = ACCEL_LATEST;
        #[cfg(target_arch = "arm")]
        if ACCEL_HANDLER.is_none() { stop_sensor(); }
    }
    0
}

/// Called from the event loop to deliver accel data to the registered handler.
/// Returns true if the handler was called.
pub fn poll_and_deliver() -> bool {
    unsafe {
        let handler = match ACCEL_HANDLER {
            Some(h) => h,
            None => return false,
        };

        // Check timing: deliver at the sampling rate
        let rate = ACCEL_SAMPLING_RATE.max(1);
        let interval_ms = (1000 * ACCEL_SAMPLES_PER_UPDATE) / rate;
        let now = Instant::now();

        if let Some(last) = ACCEL_LAST_POLL {
            if now.duration_since(last).as_millis() < interval_ms as u128 {
                return false;
            }
        }
        ACCEL_LAST_POLL = Some(now);

        // Collect samples
        let num = ACCEL_SAMPLES_PER_UPDATE.max(1) as usize;
        let mut samples = vec![AccelData {
            x: 0, y: 0, z: 0, did_vibrate: 0, timestamp: 0,
        }; num];

        // Read latest for each sample (in practice, sensorfw polls fast enough)
        for s in samples.iter_mut() {
            update_latest();
            *s = ACCEL_LATEST;
        }

        handler(samples.as_mut_ptr(), num as u32);
        true
    }
}

/// Get the current AccelData (for emulator use — returns a copy).
pub fn peek_latest() -> AccelData {
    update_latest();
    unsafe { ACCEL_LATEST }
}

/// Check if a handler is subscribed (for emulator event loop).
pub fn has_handler() -> bool {
    unsafe { ACCEL_HANDLER.is_some() }
}

/// Get samples_per_update (for emulator).
pub fn samples_per_update() -> u32 {
    unsafe { ACCEL_SAMPLES_PER_UPDATE }
}

/// Get sampling rate (for emulator).
pub fn sampling_rate() -> u32 {
    unsafe { ACCEL_SAMPLING_RATE }
}

/// Get/set the last poll time (for emulator).
pub fn should_deliver() -> bool {
    unsafe {
        let handler = ACCEL_HANDLER;
        if handler.is_none() {
            return false;
        }
        let rate = ACCEL_SAMPLING_RATE.max(1);
        let interval_ms = (1000 * ACCEL_SAMPLES_PER_UPDATE.max(1)) / rate;
        let now = Instant::now();
        if let Some(last) = ACCEL_LAST_POLL {
            if now.duration_since(last).as_millis() < interval_ms as u128 {
                return false;
            }
        }
        ACCEL_LAST_POLL = Some(now);
        true
    }
}
