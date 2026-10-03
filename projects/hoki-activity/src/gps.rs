// SPDX-License-Identifier: GPL-3.0-only
//! Dedicated GeoClue client. Raw records flow into the daemon's private journal.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zbus::{
    blocking::{connection::Builder, MessageIterator, Proxy},
    MatchRule, Message,
};
const SERVICE: &str = "org.freedesktop.Geoclue.Providers.Hybris";
const PATH: &str = "/org/freedesktop/Geoclue/Providers/Hybris";
const BASE: &str = "org.freedesktop.Geoclue";
type Error = Box<dyn std::error::Error>;
fn boot_ms() -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_BOOTTIME agrees with the Python activity journal across suspend.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut t) } != 0 {
        panic!("CLOCK_BOOTTIME unavailable");
    }
    t.tv_sec as i64 * 1000 + t.tv_nsec as i64 / 1_000_000
}
fn utc_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn number(n: f64) -> Value {
    if n.is_nan() {
        json!("NaN")
    } else if n == f64::INFINITY {
        json!("+Infinity")
    } else if n == f64::NEG_INFINITY {
        json!("-Infinity")
    } else {
        json!(n)
    }
}
fn arguments(message: &Message, member: &str) -> Result<Value, Error> {
    let body = message.body();
    Ok(match member {
        "GetPosition" | "PositionChanged" => {
            let (fields, t, lat, lon, alt, (level, h, v)) =
                body.deserialize::<(i32, i32, f64, f64, f64, (i32, f64, f64))>()?;
            json!([
                fields,
                t,
                number(lat),
                number(lon),
                number(alt),
                [level, number(h), number(v)]
            ])
        }
        "GetVelocity" | "VelocityChanged" => {
            let (fields, t, speed, direction, climb) =
                body.deserialize::<(i32, i32, f64, f64, f64)>()?;
            json!([fields, t, number(speed), number(direction), number(climb)])
        }
        "GetSatellite" | "GetLastSatellite" | "SatelliteChanged" => {
            let values =
                body.deserialize::<(i32, i32, i32, Vec<i32>, Vec<(i32, i32, i32, i32)>)>()?;
            json!(values)
        }
        "GetStatus" | "StatusChanged" => json!([body.deserialize::<i32>()?]),
        "GetProviderInfo" => json!(body.deserialize::<(String, String)>()?),
        "NameOwnerChanged" => json!(body.deserialize::<(String, String, String)>()?),
        "AddReference" | "SetOptions" => json!([]),
        _ => return Err(format!("Unsupported GeoClue member {member}").into()),
    })
}
fn fresh(args: &Value, signal: bool, now: i64, elapsed: i64) -> bool {
    let age = now - args[1].as_i64().unwrap_or(0) * 1000;
    signal
        && args[0].as_i64().is_some_and(|f| f & 3 == 3)
        && args[2]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() <= 90.)
        && args[3]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() <= 180.)
        && args[1].as_i64().is_some_and(|t| t > 0)
        && (-1000..=10000).contains(&age)
        && age <= elapsed + 1000
}
struct Output {
    start: i64,
    sequence: u64,
}
impl Output {
    fn write_at(&mut self, mut v: Value, boot: i64, utc: i64) -> Result<(), Error> {
        self.sequence += 1;
        v["sequence"] = json!(self.sequence);
        v["boottime_ms"] = json!(boot);
        v["elapsed_ms"] = json!(boot - self.start);
        v["received_utc_ms"] = json!(utc);
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "{v}")?;
        out.flush()?;
        Ok(())
    }
    fn write(&mut self, v: Value) -> Result<(), Error> {
        self.write_at(v, boot_ms(), utc_ms())
    }
    fn message(
        &mut self,
        msg: &Message,
        iface: &str,
        member: &str,
        signal: bool,
        boot: i64,
        utc: i64,
    ) -> Result<(), Error> {
        let args = arguments(msg, member)?;
        let mut event = json!({"event":"dbus","source":if signal{"signal"}else{"snapshot"},"interface":iface,"member":member,
            "signature":msg.body().signature().to_string(),"arguments":args,"applied_to_ui":signal});
        if iface.ends_with(".Position") {
            event["fresh_for_session"] = json!(fresh(&args, signal, utc, boot - self.start));
        }
        self.write_at(event, boot, utc)
    }
}
enum Event {
    Signal(Message, i64, i64),
    Stop,
    Lost(String),
}
pub fn run() -> Result<(), Error> {
    let mut output = Output {
        start: boot_ms(),
        sequence: 0,
    };
    output.write(json!({"event":"session_start","schema":1,"storage":"activity_journal","clock":"CLOCK_BOOTTIME"}))?;
    let result = collect(&mut output);
    output.write(json!({"event":"session_end","reason":if result.is_ok(){"user_stop"}else{"gps_error"},
        "error":result.as_ref().err().map(|e|e.to_string()).unwrap_or_default(),"reference_release":"disconnect dedicated D-Bus client"}))?;
    result
}
fn collect(output: &mut Output) -> Result<(), Error> {
    let conn = Builder::session()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let (tx, rx) = mpsc::sync_channel(128);
    // Install signal subscriptions before acquiring hardware; cached snapshots
    // are retained but never classified as live fixes.
    let rules = [
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE)?
            .path(PATH)?
            .build(),
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(SERVICE)?
            .build(),
    ];
    for rule in rules {
        let iterator = MessageIterator::for_match_rule(rule, &conn, Some(128))?;
        let tx = tx.clone();
        std::thread::spawn(move || {
            for message in iterator {
                let event = match message {
                    Ok(m) => Event::Signal(m, boot_ms(), utc_ms()),
                    Err(e) => Event::Lost(e.to_string()),
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
            let _ = tx.send(Event::Lost("GeoClue bus disconnected".into()));
        });
    }
    let stop_tx = tx.clone();
    std::thread::spawn(move || {
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        let _ = stop_tx.send(Event::Stop);
    });
    let proxy = Proxy::new(&conn, SERVICE, PATH, BASE)?;
    for member in ["AddReference", "SetOptions"] {
        output.write(
            json!({"event":"method_call","interface":BASE,"member":member,
            "arguments":if member=="SetOptions" {json!([{"UpdateInterval":1000}])}else{json!([])}}),
        )?;
        let reply = if member == "SetOptions" {
            let options = HashMap::from([("UpdateInterval", zbus::zvariant::Value::I32(1000))]);
            proxy.call_method(member, &(options,))?
        } else {
            proxy.call_method(member, &())?
        };
        output.message(&reply, BASE, member, false, boot_ms(), utc_ms())?;
    }
    for (suffix, member) in [
        ("", "GetProviderInfo"),
        ("", "GetStatus"),
        (".Position", "GetPosition"),
        (".Velocity", "GetVelocity"),
        (".Satellite", "GetSatellite"),
        (".Satellite", "GetLastSatellite"),
    ] {
        let iface = format!("{BASE}{suffix}");
        let proxy = Proxy::new(&conn, SERVICE, PATH, iface.as_str())?;
        output.write(
            json!({"event":"method_call","interface":iface,"member":member,"arguments":[]}),
        )?;
        match proxy.call_method(member,&()) {
            Ok(reply)=>output.message(&reply,&iface,member,false,boot_ms(),utc_ms())?,
            Err(e)=>output.write(json!({"event":"dbus_error","interface":iface,"member":member,"error":e.to_string()}))?
        }
    }
    let mut next_heartbeat = boot_ms() + 10000;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Event::Stop) => {
                conn.close()?;
                return Ok(());
            }
            Ok(Event::Lost(e)) => return Err(e.into()),
            Ok(Event::Signal(msg, boot, utc)) => {
                let header = msg.header();
                let iface = header.interface().map(|s| s.as_str()).unwrap_or("");
                let member = header.member().map(|s| s.as_str()).unwrap_or("");
                if member == "NameOwnerChanged" {
                    let (_, old, new) = msg.body().deserialize::<(String, String, String)>()?;
                    output.write(
                        json!({"event":"provider_owner_changed","old_owner":old,"new_owner":new}),
                    )?;
                    if !old.is_empty() {
                        conn.close()?;
                        return Err(
                            "GPS provider restarted; start a new activity to reconnect".into()
                        );
                    }
                } else if iface.starts_with(BASE) {
                    output.message(&msg, iface, member, true, boot, utc)?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => return Err(e.into()),
        }
        if boot_ms() >= next_heartbeat {
            output.write(json!({"event":"heartbeat"}))?;
            next_heartbeat = boot_ms() + 10000;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_position_preserves_nonfinite_values_and_freshness_is_session_scoped() {
        let msg = Message::signal(PATH, format!("{BASE}.Position"), "PositionChanged")
            .unwrap()
            .build(&(
                7i32,
                100i32,
                52f64,
                13f64,
                f64::NAN,
                (6i32, 5f64, f64::INFINITY),
            ))
            .unwrap();
        let a = arguments(&msg, "PositionChanged").unwrap();
        assert_eq!(a[4], "NaN");
        assert_eq!(a[5][2], "+Infinity");
        assert!(fresh(&a, true, 101000, 2000));
        assert!(!fresh(&a, false, 101000, 2000));
        assert!(!fresh(&a, true, 112000, 15000));
        assert!(!fresh(&a, true, 105000, 1000));
    }
}
