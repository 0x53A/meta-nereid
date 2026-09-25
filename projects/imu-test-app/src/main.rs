slint::include_modules!();

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::names::{BusName, InterfaceName, MemberName};
use zbus::zvariant::ObjectPath;

const SERVICE: &str = "com.nokia.SensorService";
const MANAGER_PATH: &str = "/SensorManager";
const MANAGER_IFACE: &str = "local.SensorManager";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

const ACCEL_ID: &str = "accelerometersensor";
const ACCEL_IFACE: &str = "local.AccelerometerSensor";
const ACCEL_PROP: &str = "xyz";

const GYRO_ID: &str = "gyroscopesensor";
const GYRO_IFACE: &str = "local.GyroscopeSensor";
const GYRO_PROP: &str = "value";

fn bus_name() -> BusName<'static> {
    BusName::try_from(SERVICE).unwrap()
}

/// Load a sensor plugin (must be called before requestSensor).
fn load_plugin(conn: &Connection, sensor_id: &str) -> Result<(), String> {
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(MANAGER_PATH).unwrap(),
            Some(InterfaceName::try_from(MANAGER_IFACE).unwrap()),
            MemberName::try_from("loadPlugin").unwrap(),
            &(sensor_id,),
        )
        .map_err(|e| format!("loadPlugin({sensor_id}): {e}"))?;

    let body = reply.body();
    let ok: bool = body
        .deserialize()
        .map_err(|e| format!("loadPlugin parse: {e}"))?;
    if !ok {
        return Err(format!("loadPlugin({sensor_id}) returned false"));
    }
    Ok(())
}

/// Request a sensor session from the SensorManager.
fn request_sensor(conn: &Connection, sensor_id: &str) -> Result<i32, String> {
    let pid = std::process::id() as i64;
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(MANAGER_PATH).unwrap(),
            Some(InterfaceName::try_from(MANAGER_IFACE).unwrap()),
            MemberName::try_from("requestSensor").unwrap(),
            &(sensor_id, pid),
        )
        .map_err(|e| format!("requestSensor({sensor_id}): {e}"))?;

    let body = reply.body();
    let session_id: i32 = body
        .deserialize()
        .map_err(|e| format!("requestSensor parse: {e}"))?;
    if session_id < 0 { return Err(format!("Invalid session: {session_id}")); }
    Ok(session_id)
}

/// Start a sensor session. start/stop live on the sensor-specific interface.
fn start_sensor(conn: &Connection, sensor_id: &str, sensor_iface: &str, session_id: i32) -> Result<(), String> {
    let path = format!("/SensorManager/{sensor_id}");
    conn.call_method(
        Some(bus_name()),
        ObjectPath::try_from(path.as_str()).unwrap(),
        Some(InterfaceName::try_from(sensor_iface).unwrap()),
        MemberName::try_from("start").unwrap(),
        &(session_id,),
    )
    .map_err(|e| format!("start({sensor_id}): {e}"))?;
    Ok(())
}

/// Read the XYZ property from a sensor. Returns (x, y, z) as f64.
/// The D-Bus type is Variant containing struct (tddd).
fn read_xyz(
    conn: &Connection,
    sensor_id: &str,
    iface: &str,
    prop: &str,
) -> Result<(f64, f64, f64), String> {
    let path = format!("/SensorManager/{sensor_id}");
    let reply = conn
        .call_method(
            Some(bus_name()),
            ObjectPath::try_from(path.as_str()).unwrap(),
            Some(InterfaceName::try_from(PROPS_IFACE).unwrap()),
            MemberName::try_from("Get").unwrap(),
            &(iface, prop),
        )
        .map_err(|e| format!("Get({sensor_id}/{prop}): {e}"))?;

    let body = reply.body();
    // The reply is a Variant containing (tddd) — uint64 timestamp + 3 doubles
    // Deserialize as (u64, f64, f64, f64) inside a zvariant Value
    let variant: zbus::zvariant::OwnedValue = body
        .deserialize()
        .map_err(|e| format!("deserialize variant: {e}"))?;

    // OwnedValue implements TryFrom for deserializable types
    let (_, x, y, z): (u64, f64, f64, f64) = zbus::zvariant::OwnedValue::try_into(variant)
        .map_err(|e| format!("downcast (tddd): {e}"))?;

    Ok((x, y, z))
}

// sensorfw expires sessions without data sockets after ten seconds, even
// when the client uses D-Bus properties for its displayed values.
fn connect_stream(id: i32) -> Result<UnixStream, String> {
    let mut stream = UnixStream::connect("/run/sensord.sock").map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
    stream.write_all(&id.to_ne_bytes()).map_err(|e| e.to_string())?;
    let mut tag = [0];
    stream.read_exact(&mut tag).map_err(|e| e.to_string())?;
    if tag != [b'\n'] { return Err("Invalid sensor socket greeting".into()); }
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(stream)
}

fn drain_stream(stream: &mut UnixStream) -> Result<(), String> {
    let mut buffer = [0; 4096];
    for _ in 0..64 {
        match stream.read(&mut buffer) {
            Ok(0) => return Err("Sensor service disconnected".into()),
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn poll_sensors(weak: &slint::Weak<App>) -> Result<(), String> {
    let conn = Connection::system().map_err(|e| e.to_string())?;
    load_plugin(&conn, ACCEL_ID)?;
    load_plugin(&conn, GYRO_ID)?;
    let accel_id = request_sensor(&conn, ACCEL_ID)?;
    let mut accel_stream = connect_stream(accel_id)?;
    let gyro_id = request_sensor(&conn, GYRO_ID)?;
    let mut gyro_stream = connect_stream(gyro_id)?;
    start_sensor(&conn, ACCEL_ID, ACCEL_IFACE, accel_id)?;
    start_sensor(&conn, GYRO_ID, GYRO_IFACE, gyro_id)?;
    eprintln!("IMU sensor sessions connected");
    loop {
        drain_stream(&mut accel_stream)?;
        drain_stream(&mut gyro_stream)?;
        let (ax, ay, az) = read_xyz(&conn, ACCEL_ID, ACCEL_IFACE, ACCEL_PROP)?;
        let (gx, gy, gz) = read_xyz(&conn, GYRO_ID, GYRO_IFACE, GYRO_PROP)?;
        let weak = weak.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_connected(true);
                app.set_status_text("Sensors active".into());
                app.set_accel_x(ax as f32); app.set_accel_y(ay as f32); app.set_accel_z(az as f32);
                app.set_gyro_x(gx as f32); app.set_gyro_y(gy as f32); app.set_gyro_z(gz as f32);
            }
        }).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");
    let app = App::new().unwrap();
    let weak = app.as_weak();
    std::thread::spawn(move || loop {
        // A fresh connection and sockets replace sessions lost on daemon restart.
        if let Err(error) = poll_sensors(&weak) {
            eprintln!("IMU reconnecting: {error}");
            let weak = weak.clone();
            if slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_connected(false);
                    app.set_status_text(format!("Reconnecting: {error}").into());
                }
            }).is_err() { break; }
        }
        std::thread::sleep(Duration::from_secs(1));
    });
    app.run().unwrap();
}
