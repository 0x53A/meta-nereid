//! Minimal native equivalent of the PebbleKit JS bundled with Piny Wings.
//! The app still receives the real KiezelPay answer; this never grants a license.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const UUID: [u8; 16] = [
    0x17, 0xb3, 0x32, 0x24, 0x31, 0x7d, 0x44, 0xd7,
    0x9e, 0x70, 0x62, 0x80, 0xd4, 0x2f, 0xb9, 0x39,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tuple {
    pub key: u32,
    pub kind: u8,
    pub value: Vec<u8>,
}

impl Tuple {
    pub fn uint(key: u32, value: u32) -> Self {
        Self { key, kind: 2, value: value.to_le_bytes().to_vec() }
    }

    pub fn string(key: u32, value: &str) -> Self {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        Self { key, kind: 1, value: bytes }
    }
}

pub fn ready() -> Vec<Tuple> { vec![Tuple::uint(7, 1)] }

fn account_token() -> Result<String, String> {
    if let Ok(token) = std::env::var("PEBBLE_ACCOUNT_TOKEN") {
        if !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Ok(token);
        }
        return Err("PEBBLE_ACCOUNT_TOKEN must contain only letters, digits, or hyphens".into());
    }
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("No data directory for Pebble account token")?;
    let path = base.join("pebble-runner/account-token");
    if let Ok(token) = std::fs::read_to_string(&path) {
        let token = token.trim().to_owned();
        if !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Ok(token);
        }
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let token = std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|e| e.to_string())?.trim().to_owned();
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(&path).map_err(|e| e.to_string())?;
    file.write_all(token.as_bytes()).map_err(|e| e.to_string())?;
    Ok(token)
}

fn number(tuples: &[Tuple], key: u32) -> Option<u32> {
    let tuple = tuples.iter().find(|tuple| tuple.key == key && tuple.kind >= 2)?;
    if tuple.value.len() > 4 { return None; }
    Some(tuple.value.iter().enumerate().fold(0, |sum, (i, &b)| sum | (b as u32) << (i * 8)))
}

pub fn is_status_request(tuples: &[Tuple]) -> bool {
    number(tuples, 8).is_some() && number(tuples, 3).is_some()
}

pub fn status(tuples: &[Tuple]) -> Result<Vec<Tuple>, String> {
    let nonce = number(tuples, 8).ok_or("Missing nonce")?;
    let app_id = number(tuples, 3).ok_or("Missing app ID")?;
    let token = account_token()?;
    let cache_buster = SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?.as_millis();
    let url = format!("https://kiezelpay.com/api/tp/status?random={nonce}&app_id={app_id}&account_token={token}&cache_buster={cache_buster}");
    let response = ureq::get(&url).timeout(Duration::from_secs(5)).call()
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = response.into_json().map_err(|e| e.to_string())?;
    let status = body.get("status").and_then(|v| v.as_str()).ok_or("Missing status")?;
    let started = status == "started";
    let mut result = vec![Tuple::uint(1, u32::from(status != "unpaid" && !started))];
    if let Some(id) = body.get("paymentId").and_then(|v| v.as_u64()) {
        let id = u32::try_from(id).map_err(|_| "Payment ID exceeds uint32")?;
        result.push(Tuple::uint(2, id));
    }
    if let Some(hash) = body.get("hash").and_then(|v| v.as_str()) {
        result.push(Tuple::string(4, hash));
    }
    result.push(Tuple::uint(6, u32::from(started)));
    Ok(result)
}

pub fn internet_failure() -> Vec<Tuple> { vec![Tuple::uint(5, 1)] }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn piny_status_request_requires_nonce_and_app_id() {
        assert!(is_status_request(&[Tuple::uint(8, 123), Tuple::uint(3, 456)]));
        assert!(!is_status_request(&[Tuple::uint(8, 123)]));
    }
}
