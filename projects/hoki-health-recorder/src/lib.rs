mod profile_owner;
pub mod reboot_reconcile;
pub mod sleep_backend;
pub mod sleep_plan;
pub mod sleep_transaction;
pub mod ssc_helper;
pub mod ssc_quiesce;
pub mod storage_admission;
pub mod suspend_policy;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn select_ssc(inventory: &Value, status: &Value, boot: &str, kind: &str) -> Result<String> {
    if !valid_session_id(boot)
        || inventory["version"] != 1
        || inventory["boot_id"] != boot
        || inventory["parse_error"] != false
        || inventory["interrupted"] != false
        || !inventory["session_id"]
            .as_str()
            .is_some_and(valid_session_id)
    {
        return Err("invalid, interrupted or stale SSC inventory".into());
    }
    let accepted = status["accepted"]
        .as_u64()
        .ok_or("missing SSC accepted count")?;
    if status["version"] != 1
        || status["phase"] != "closed"
        || status["archive_complete"] != true
        || status["archive_error"] != 0
        || status["rejected"] != 0
        || accepted == 0
        || status["durable"].as_u64() != Some(accepted)
        || status["accepted_not_confirmed_durable"] != 0
    {
        return Err("SSC discovery archive did not finalize cleanly".into());
    }
    let streams = inventory["streams"]
        .as_array()
        .ok_or("missing SSC streams")?;
    let mut matches = streams.iter().filter(|s| s["data_type"] == kind);
    let selected = matches.next().ok_or("SSC data type was not queried")?;
    if matches.next().is_some() || selected["status"] != "unique" || selected["responses"] != 1 {
        return Err("SSC endpoint is missing or ambiguous".into());
    }
    let ids = selected["suids"].as_array().ok_or("missing SSC IDs")?;
    if ids.len() != 1 {
        return Err("SSC endpoint must have one ID".into());
    }
    let id = ids[0].as_str().ok_or("invalid SSC ID")?;
    if id.len() != 36
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || &id[..2] != "09"
        || &id[18..20] != "11"
    {
        return Err("invalid canonical SSC ID".into());
    }
    Ok(id.to_string())
}
/// Optional systemd readiness; an explicitly configured broken endpoint is an error.
pub fn notify_ready(endpoint: Option<&std::ffi::OsStr>) -> Result<()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let Some(endpoint) = endpoint else {
        return Ok(());
    };
    let bytes = endpoint.as_bytes();
    let address = match bytes.first() {
        Some(b'@') if bytes.len() > 1 => SocketAddr::from_abstract_name(&bytes[1..])?,
        Some(b'/') => SocketAddr::from_pathname(endpoint)?,
        _ => return Err("invalid NOTIFY_SOCKET".into()),
    };
    let socket = UnixDatagram::unbound()?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    socket.connect_addr(&address)?;
    let message = b"READY=1\nSTATUS=HAL sensor activation complete; freshness unverified";
    if socket.send(message)? != message.len() {
        return Err("short readiness datagram".into());
    }
    Ok(())
}
#[derive(Debug)]
pub struct ControlError {
    pub code: i64,
    pub command: String,
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.command, self.code)
    }
}
impl std::error::Error for ControlError {}
pub fn recovery_matches(metadata: &Value, boot: &str, status: &Value) -> Result<bool> {
    let saved_boot = metadata["boot_id"]
        .as_str()
        .ok_or("missing saved boot identity")?;
    let saved_session = metadata["session_id"]
        .as_str()
        .ok_or("missing saved session identity")?;
    if !valid_session_id(saved_boot) || !valid_session_id(saved_session) || !valid_session_id(boot)
    {
        return Err("invalid recovery identity".into());
    }
    if !matches!(
        metadata["phase"].as_str(),
        Some("started" | "closed" | "failed")
    ) {
        return Err("unknown controller phase".into());
    }
    if boot != saved_boot {
        return Ok(false);
    }
    let active = status["session_id"]
        .as_str()
        .ok_or("backend lacks session identity")?;
    Ok(active == saved_session)
}

pub fn request(socket: &Path, query: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut data = serde_json::to_vec(&query)?;
    data.push(b'\n');
    if data.len() > 8192 {
        return Err("request exceeds protocol limit".into());
    }
    stream.write_all(&data)?;
    let mut reply = Vec::new();
    BufReader::new(stream)
        .take(131072)
        .read_until(b'\n', &mut reply)?;
    if reply.last() != Some(&b'\n') {
        return Err("truncated or oversized control reply".into());
    }
    let value: Value = serde_json::from_slice(&reply)?;
    match value.get("error").and_then(Value::as_i64) {
        Some(0) => Ok(value),
        Some(code) => Err(ControlError {
            code,
            command: query["command"].to_string(),
        }
        .into()),
        None => Err("reply lacks integer error code".into()),
    }
}
pub fn valid_session_id(token: &str) -> bool {
    token.len() == 36
        && token.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}
pub fn owned_request(socket: &Path, token: &str, mut query: Value) -> Result<Value> {
    if !valid_session_id(token) {
        return Err("invalid session identity".into());
    }
    let command = query["command"]
        .as_str()
        .ok_or("missing command")?
        .to_string();
    query
        .as_object_mut()
        .ok_or("request must be object")?
        .insert("session_id".into(), json!(token));
    let reply = request(socket, query)?;
    if matches!(command.as_str(), "open" | "status") && reply["session_id"] != token {
        return Err("backend session identity changed or was not acknowledged".into());
    }
    Ok(reply)
}
fn integer(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing integer {key}").into())
}
pub fn select(inventory: &Value, latency: u64) -> Result<Vec<Value>> {
    const MAX_DEMAND_NS: u64 = 60_000_000_000;
    if latency > MAX_DEMAND_NS {
        return Err("latency exceeds backend demand limit".into());
    }
    let sensors = inventory
        .get("sensors")
        .and_then(Value::as_array)
        .ok_or("missing inventory")?;
    if sensors.is_empty() || sensors.len() > 256 {
        return Err("invalid inventory size".into());
    }
    let mut types: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
    let mut handles = BTreeSet::new();
    for sensor in sensors {
        let typ = integer(sensor, "type")?;
        if typ <= 0 {
            return Err("invalid sensor type".into());
        }
        let handle = integer(sensor, "handle")?;
        if !(0..=i32::MAX as i64).contains(&handle) || !handles.insert(handle) {
            return Err("invalid or duplicate sensor handle".into());
        }
        if integer(sensor, "flags")? < 0 {
            return Err("invalid flags".into());
        }
        types.entry(typ).or_default().push(sensor);
    }
    let mut selected = Vec::new();
    for (typ, variants) in types {
        let wake: Vec<_> = variants
            .iter()
            .copied()
            .filter(|s| s["flags"].as_i64().unwrap() & 1 != 0)
            .collect();
        let choices = if wake.is_empty() { variants } else { wake };
        if choices.len() != 1 {
            return Err(format!("ambiguous descriptor for type {typ}").into());
        }
        let sensor = choices[0];
        let min = integer(sensor, "min_delay_us")?;
        let max = integer(sensor, "max_delay_us")?;
        if min > 0 && max > 0 && min > max {
            return Err("inverted delay limits".into());
        }
        let mut period: u64 = if matches!(typ, 1 | 4 | 16 | 35 | 65572) {
            40_000_000
        } else {
            200_000_000
        };
        if min > 0 {
            period = period.max((min as u64).checked_mul(1000).ok_or("delay overflow")?);
        }
        if max > 0 {
            period = period.min((max as u64).checked_mul(1000).ok_or("delay overflow")?);
        }
        if period > MAX_DEMAND_NS {
            return Err("sensor period exceeds backend demand limit".into());
        }
        selected.push(json!({"sensor":sensor,"period_ns":period,"latency_ns":latency}));
    }
    Ok(selected)
}
pub fn count(status: &Value, key: &str) -> Result<u64> {
    Ok(status
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing counter {key}"))?
        .parse()?)
}
pub fn healthy(status: &Value) -> Result<()> {
    if integer(status, "storage_status")? < 0
        || status["flush_failed"] != false
        || count(status, "dropped")? != 0
        || count(status, "input_failures")? != 0
        || integer(status, "wake_error")? != 0
    {
        return Err("recorder reports storage, input, flush or wake failure".into());
    }
    Ok(())
}
pub fn finalized(status: &Value) -> Result<bool> {
    healthy(status)?;
    if status["stopped"] != true {
        return Ok(false);
    }
    if integer(status, "storage_status")? != 1
        || count(status, "submitted_records")? != count(status, "durable_records")?
        || count(status, "received")? != count(status, "durable_records")?
        || status["wake_held"] != false
    {
        return Err("stopped recorder is not durable and wake-free".into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    fn sensor(typ: i64, handle: i64, flags: i64, min: i64, max: i64) -> Value {
        json!({"type":typ,"handle":handle,"flags":flags,"min_delay_us":min,"max_delay_us":max})
    }
    #[test]
    fn ssc_selection_rejects_incomplete_stale_and_ambiguous_discovery() {
        let boot = "12345678-1234-1234-1234-123456789abc";
        let id = "0914ab556a13704e0211b5977bea186aabb9";
        let i = json!({"version":1,"boot_id":boot,"session_id":boot,"parse_error":false,"interrupted":false,
            "streams":[{"data_type":"fsl_min","status":"unique","responses":1,"suids":[id]}]});
        let status = json!({"version":1,"phase":"closed","archive_complete":true,"archive_error":0,
            "rejected":0,"accepted":50,"durable":50,"accepted_not_confirmed_durable":0});
        assert_eq!(select_ssc(&i, &status, boot, "fsl_min").unwrap(), id);
        assert!(select_ssc(
            &i,
            &status,
            "12345678-1234-1234-1234-123456789abd",
            "fsl_min"
        )
        .is_err());
        assert!(select_ssc(&i, &status, boot, "missing").is_err());
        for (pointer, value) in [
            ("/parse_error", json!(true)),
            ("/interrupted", json!(true)),
            ("/streams/0/suids", json!([])),
            ("/streams/0/suids", json!([id, id])),
            ("/streams/0/status", json!("empty")),
            ("/streams/0/responses", json!(2)),
            (
                "/streams/0/suids/0",
                json!("0914ab556a13704e0210b5977bea186aabb9"),
            ),
        ] {
            let mut changed = i.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                select_ssc(&changed, &status, boot, "fsl_min").is_err(),
                "{pointer}"
            );
        }
        let mut duplicate = i.clone();
        duplicate["streams"]
            .as_array_mut()
            .unwrap()
            .push(i["streams"][0].clone());
        assert!(select_ssc(&duplicate, &status, boot, "fsl_min").is_err());
        for (key, value) in [
            ("phase", json!("started")),
            ("archive_complete", json!(false)),
            ("rejected", json!(1)),
            ("durable", json!(49)),
            ("accepted", json!("50")),
        ] {
            let mut changed = status.clone();
            changed[key] = value;
            assert!(select_ssc(&i, &changed, boot, "fsl_min").is_err(), "{key}");
        }
    }
    #[test]
    fn readiness_requires_delivery_when_configured() {
        use std::ffi::OsStr;
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        assert!(notify_ready(None).is_ok());
        assert!(notify_ready(Some(OsStr::new("relative"))).is_err());
        assert!(notify_ready(Some(OsStr::new("@"))).is_err());
        let name = format!("hoki-ready-{}", std::process::id());
        let receiver =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
                .unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert!(notify_ready(Some(OsStr::new(&format!("@{name}")))).is_ok());
        let mut bytes = [0u8; 256];
        let n = receiver.recv(&mut bytes).unwrap();
        assert!(bytes[..n].starts_with(b"READY=1\nSTATUS="));
        drop(receiver);
        assert!(notify_ready(Some(OsStr::new(&format!("@{name}")))).is_err());
    }
    #[test]
    fn wake_variant_and_descriptor_limits() {
        let list = json!({"sensors":[sensor(1,1,0,0,0),sensor(1,2,1,50000,100000),sensor(4,3,1,1000,30000)]});
        let plan = select(&list, 20_000_000_000).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0]["sensor"]["handle"], 2);
        assert_eq!(plan[0]["period_ns"], 50_000_000u64);
        assert_eq!(plan[1]["period_ns"], 30_000_000u64);
    }
    #[test]
    fn ambiguous_or_inverted_descriptors_rejected() {
        assert!(select(
            &json!({"sensors":[sensor(1,1,1,0,0),sensor(1,2,1,0,0)]}),
            20
        )
        .is_err());
        assert!(select(&json!({"sensors":[sensor(1,1,1,30000,1000)]}), 20).is_err());
        assert!(select(&json!({"sensors":[]}), 20).is_err());
    }
    #[test]
    fn duplicate_handles_and_unaddressable_descriptors_are_rejected() {
        // Different types must not cause two demands to address the same handle.
        for sensors in [
            vec![sensor(1, 7, 1, 0, 0), sensor(4, 7, 1, 0, 0)],
            vec![sensor(1, 7, 0, 0, 0), sensor(1, 7, 1, 0, 0)],
            vec![sensor(1, -1, 1, 0, 0)],
            vec![sensor(1, i32::MAX as i64 + 1, 1, 0, 0)],
            vec![sensor(1, 7, 1, 60_000_001, 0)],
        ] {
            assert!(select(&json!({"sensors":sensors}), 20).is_err());
        }
        assert!(select(&json!({"sensors":[sensor(1, 7, 1, 0, 0)]}), 60_000_000_001).is_err());
        assert!(select(&json!({"sensors":[sensor(1, 0, 1, 60_000_000, 0)]}), 60_000_000_000).is_ok());
    }
    fn status() -> Value {
        json!({"storage_status":1,"flush_failed":false,"dropped":"0","input_failures":"0",
               "wake_error":0,"stopped":true,"received":"7","submitted_records":"7","durable_records":"7","wake_held":false})
    }
    #[test]
    fn final_requires_durable_and_no_loss() {
        let mut s = status();
        assert!(finalized(&s).unwrap());
        s["durable_records"] = json!("6");
        assert!(finalized(&s).is_err());
        s = status();
        s["dropped"] = json!("1");
        assert!(finalized(&s).is_err());
        s = status();
        s["wake_held"] = json!(true);
        assert!(finalized(&s).is_err());
        s = status();
        s["stopped"] = json!(false);
        assert!(!finalized(&s).unwrap());
    }
    fn exchange_with_owner(response: Vec<u8>, owner: Option<&str>) -> Result<Value> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("hoki-controller-test-{}-{n}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("control");
        let listener = UnixListener::bind(&path).unwrap();
        let expected_owner = owner.map(str::to_string);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut query = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut query)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&query).unwrap()["command"],
                "status"
            );
            if let Some(token) = expected_owner {
                assert_eq!(
                    serde_json::from_str::<Value>(&query).unwrap()["session_id"],
                    token
                );
            }
            let _ = stream.write_all(&response);
        });
        let result = match owner {
            Some(token) => owned_request(&path, token, json!({"command":"status"})),
            None => request(&path, json!({"command":"status"})),
        };
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
        result
    }
    fn exchange(response: Vec<u8>) -> Result<Value> {
        exchange_with_owner(response, None)
    }
    #[test]
    fn owned_requests_require_matching_identity() {
        let token = "12345678-1234-1234-1234-123456789abc";
        assert!(valid_session_id(token));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("12345678-1234-1234-1234-123456789abz"));
        let response = format!("{{\"error\":0,\"session_id\":\"{token}\"}}\n");
        assert!(exchange_with_owner(response.into_bytes(), Some(token)).is_ok());
        assert!(exchange_with_owner(
            b"{\"error\":0,\"session_id\":\"different\"}\n".to_vec(),
            Some(token)
        )
        .is_err());
        assert!(exchange_with_owner(b"{\"error\":0}\n".to_vec(), Some(token)).is_err());
    }
    #[test]
    fn recovery_rejects_stale_or_malformed_identity() {
        let boot = "12345678-1234-1234-1234-123456789abc";
        let token = "12345678-1234-1234-1234-123456789abd";
        let other = "12345678-1234-1234-1234-123456789abe";
        let m = json!({"phase":"started","boot_id":boot,"session_id":token});
        assert!(recovery_matches(&m, boot, &json!({"session_id":token})).unwrap());
        assert!(!recovery_matches(&m, other, &json!({"session_id":token})).unwrap());
        assert!(!recovery_matches(&m, boot, &json!({"session_id":other})).unwrap());
        assert!(recovery_matches(&m, boot, &json!({})).is_err());
        assert!(recovery_matches(
            &json!({"phase":"started","boot_id":boot}),
            boot,
            &json!({"session_id":token})
        )
        .is_err());
    }
    #[test]
    fn socket_reply_validation() {
        assert!(exchange(b"{\"error\":0}\n".to_vec()).is_ok());
        for data in [
            b"{\"error\":-5}\n".to_vec(),
            b"{\"error\":0}".to_vec(),
            b"{}\n".to_vec(),
            vec![b'x'; 131073],
        ] {
            assert!(exchange(data).is_err());
        }
    }
}
