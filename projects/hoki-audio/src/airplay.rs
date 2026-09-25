//! A finite, user-requested browse. Never load module-raop-discover.
use crate::command;
use std::{collections::BTreeMap, net::IpAddr, process::Command, time::Duration};

pub const SINK_NAME: &str = "hoki_airplay";
const OWNER: &str = "interactive-v1";

#[derive(Clone, Debug)]
pub struct Speaker {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol: &'static str,
    pub encryption: &'static str,
    pub codec: &'static str,
    pub rate: u32,
    pub unavailable: String,
}

// Avahi uses decimal byte escapes, not octal. Decode only after splitting fields.
fn unescape(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() {
                return None;
            }
            if bytes[i].is_ascii_digit() {
                let digits = bytes.get(i..i + 3)?;
                if !digits.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                let n = std::str::from_utf8(digits).ok()?.parse::<u16>().ok()?;
                out.push(u8::try_from(n).ok()?);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let value = String::from_utf8(out).ok()?;
    (!value.contains('\0')).then_some(value)
}

fn txt_records(s: &str) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        rest = rest.strip_prefix('"')?;
        let mut escaped = false;
        let mut end = None;
        for (i, c) in rest.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = Some(i);
                break;
            }
        }
        let end = end?;
        let value = unescape(&rest[..end])?;
        if let Some((key, value)) = value.split_once('=') {
            result.insert(key.to_ascii_lowercase(), value.to_string());
        }
        rest = rest[end + 1..].trim_start();
    }
    Some(result)
}

pub fn parse_browse(output: &str) -> Vec<Speaker> {
    let mut speakers = BTreeMap::new();
    for line in output.lines() {
        let f: Vec<_> = line.splitn(10, ';').collect();
        if f.len() != 10 || f[0] != "=" || f[4] != "_raop._tcp" {
            continue;
        }
        let Some(service) = unescape(f[3]) else {
            continue;
        };
        let Ok(ip) = f[7].parse::<IpAddr>() else {
            continue;
        };
        if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
            continue;
        }
        let Ok(port) = f[8].parse::<u16>() else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let Some(txt) = txt_records(f[9]) else {
            continue;
        };
        let has = |key: &str, item: &str, default: &str| {
            txt.get(key)
                .map(String::as_str)
                .unwrap_or(default)
                .split(',')
                .any(|s| s.trim() == item)
        };
        let protocol = if has("tp", "UDP", "TCP") {
            "UDP"
        } else {
            "TCP"
        };
        let encryption = if has("et", "1", "0") { "RSA" } else { "none" };
        let codec = if has("cn", "1", "0,1") { "ALAC" } else { "PCM" };
        let rate = txt
            .get("sr")
            .map(String::as_str)
            .unwrap_or("44100")
            .parse::<u32>()
            .unwrap_or(0);
        let unavailable = if has("pw", "true", "false") || has("pw", "1", "false") {
            "Password required"
        } else if !has("tp", "TCP", "TCP") && !has("tp", "UDP", "TCP") {
            "Unsupported transport"
        } else if !has("et", "0", "0") && !has("et", "1", "0") {
            "Pairing not supported"
        } else if !has("cn", "0", "0,1") && !has("cn", "1", "0,1") {
            "Unsupported audio codec"
        } else if rate != 44100 || !has("ss", "16", "16") || !has("ch", "2", "2") {
            "Unsupported audio format"
        } else {
            ""
        };
        let name = service
            .split_once('@')
            .map_or(service.as_str(), |(_, name)| name)
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect::<String>();
        if name.is_empty() {
            continue;
        }
        let address = match ip {
            IpAddr::V6(v6) if v6.is_unicast_link_local() => {
                // Interface is used only as an IPv6 scope, never as command syntax.
                if !f[1]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
                {
                    continue;
                }
                format!("{v6}%{}", f[1])
            }
            _ => ip.to_string(),
        };
        let speaker = Speaker {
            name,
            address,
            port,
            protocol,
            encryption,
            codec,
            rate,
            unavailable: unavailable.into(),
        };
        let key = (service, f[5].to_string());
        // One row per advertised service; prefer IPv4 over duplicate IPv6 answers.
        if ip.is_ipv4() || !speakers.contains_key(&key) {
            speakers.insert(key, speaker);
        }
    }
    let mut result: Vec<_> = speakers.into_values().collect();
    result.sort_by_key(|s| s.name.to_lowercase());
    result.truncate(64);
    result
}

pub fn discover(cancelled: impl Fn() -> bool) -> Result<Vec<Speaker>, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("avahi-browse");
    command.args([
        "--resolve",
        "--parsable",
        "--terminate",
        "--ignore-local",
        "_raop._tcp",
    ]);
    // The launcher can terminate the GUI abruptly. Do not leave a browser behind.
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let output =
        command::output_cancellable(&mut command, Duration::from_secs(8), 256 * 1024, cancelled)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => "AirPlay discovery is not installed".into(),
                std::io::ErrorKind::TimedOut => "Search timed out. Try again".into(),
                std::io::ErrorKind::Interrupted => "Search cancelled".into(),
                _ => format!("Cannot search: {e}"),
            })?;
    if !output.status.success() {
        eprintln!(
            "AirPlay discovery: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err("Discovery unavailable. Check Wi-Fi and Avahi".into());
    }
    Ok(parse_browse(&String::from_utf8_lossy(&output.stdout)))
}

pub trait Pulse {
    fn run(&mut self, args: &[String]) -> Result<String, String>;
}

pub struct Server;
impl Pulse for Server {
    fn run(&mut self, args: &[String]) -> Result<String, String> {
        crate::pactl(args)
    }
}

fn owned_modules(pulse: &mut impl Pulse) -> Result<Vec<u32>, String> {
    let json = pulse.run(&["--format=json".into(), "list".into(), "sinks".into()])?;
    let sinks: Vec<serde_json::Value> =
        serde_json::from_str(&json).map_err(|_| "Cannot read AirPlay outputs")?;
    if sinks.iter().any(|s| s["name"] == SINK_NAME && !is_owned(s)) {
        return Err("AirPlay output name is already in use".into());
    }
    Ok(sinks
        .iter()
        .filter(|s| s["properties"]["hoki.airplay"] == OWNER && s["name"] == SINK_NAME)
        .filter_map(|s| {
            s["owner_module"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
        })
        .collect())
}

pub fn disconnect(pulse: &mut impl Pulse) -> Result<(), String> {
    for module in owned_modules(pulse)? {
        pulse.run(&["unload-module".into(), module.to_string()])?;
    }
    Ok(())
}

pub fn connect(pulse: &mut impl Pulse, speaker: &Speaker) -> Result<(), String> {
    if !speaker.unavailable.is_empty() {
        return Err(speaker.unavailable.clone());
    }
    // Never create two sinks for this feature, even when changing receivers.
    disconnect(pulse)?;
    // Network names are display data. Never forward arbitrary TXT/module arguments.
    let label: String = speaker
        .name
        .chars()
        .filter(|c| c.is_alphanumeric() || " ._-()".contains(*c))
        .take(96)
        .collect();
    let args = vec![
        "load-module".into(),
        "module-raop-sink".into(),
        format!("sink_name={SINK_NAME}"),
        format!("server=[{}]:{}", speaker.address, speaker.port),
        format!("protocol={}", speaker.protocol),
        format!("encryption={}", speaker.encryption),
        format!("codec={}", speaker.codec),
        format!("rate={}", speaker.rate),
        "format=s16le".into(),
        "channels=2".into(),
        "autoreconnect=false".into(),
        format!(
            "sink_properties='device.description=\"AirPlay: {label}\" hoki.airplay=\"{OWNER}\"'"
        ),
    ];
    let loaded = pulse.run(&args);
    if let Err(error) = loaded {
        // A timed-out pactl may still have applied its command. Reconcile by marker.
        let cleanup = disconnect(pulse);
        return Err(match cleanup {
            Ok(()) => format!("Could not add speaker: {error}"),
            Err(_) => "Connection failed; refresh outputs to check cleanup".into(),
        });
    }
    if let Err(error) = pulse.run(&["set-default-sink".into(), SINK_NAME.into()]) {
        let _ = disconnect(pulse);
        return Err(format!("Could not select speaker: {error}"));
    }
    // Creating a sink does not prove the receiver accepted the audio session.
    Ok(())
}

pub fn is_owned(sink: &serde_json::Value) -> bool {
    sink["name"] == SINK_NAME && sink["properties"]["hoki.airplay"] == OWNER
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(name: &str, ip: &str, txt: &str) -> String {
        format!("=;wlan0;IPv4;001122334455@{name};_raop._tcp;local;speaker.local;{ip};5000;{txt}")
    }
    #[test]
    fn discovery_decodes_names_deduplicates_and_rejects_unsupported_receivers() {
        let txt = r#""tp=TCP,UDP" "et=0,1" "cn=0,1" "ss=16" "ch=2" "sr=44100" "note=a;b\"c""#;
        let input = [
            record(r"K\195\188che\032one", "fe80::1", txt),
            record(r"K\195\188che\032one", "192.168.1.9", txt),
            record("Locked", "192.168.1.10", r#""pw=true""#),
            record("Pairing", "192.168.1.11", r#""et=4""#),
            record("Malformed", "bad address", txt),
        ]
        .join("\n");
        let list = parse_browse(&input);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "Küche one");
        assert_eq!(list[0].address, "192.168.1.9");
        assert_eq!(
            (list[0].protocol, list[0].codec, list[0].encryption),
            ("UDP", "ALAC", "RSA")
        );
        assert!(list[0].unavailable.is_empty());
        assert_eq!(list[1].unavailable, "Password required");
        assert_eq!(list[2].unavailable, "Pairing not supported");
    }
    #[test]
    fn malformed_and_control_data_cannot_become_module_arguments() {
        assert!(unescape(r"bad\999").is_none());
        assert!(unescape(r"bad\000").is_none());
        assert!(txt_records(r#""cn=0" "broken"#).is_none());
        let list = parse_browse(&record("A", "192.168.1.3", r#""sr=44100 sink_name=other""#));
        assert_eq!(list[0].unavailable, "Unsupported audio format");
        assert!(parse_browse(&record("A", "127.0.0.1", "")).is_empty());
    }

    #[test]
    fn real_avahi_shape_handles_escaped_at_and_ipv4_address_in_ipv6_answer() {
        // Anonymized shape observed on the watch: Avahi's interface protocol
        // need not match the resolved address family, and optional format keys
        // can be absent from modern receivers' legacy RAOP advertisements.
        let line = r#"=;wlan0;IPv6;001122334455\064Living\032room;_raop._tcp;local;receiver.local;192.168.1.20;7000;"tp=UDP" "sf=0x4" "et=0,4" "cn=0,1""#;
        let list = parse_browse(line);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Living room");
        assert_eq!(list[0].address, "192.168.1.20");
        assert_eq!(list[0].port, 7000);
        assert_eq!(list[0].encryption, "none");
        assert!(list[0].unavailable.is_empty());
    }
    struct Fake {
        calls: Vec<Vec<String>>,
        replies: std::collections::VecDeque<Result<String, String>>,
    }
    impl Pulse for Fake {
        fn run(&mut self, args: &[String]) -> Result<String, String> {
            self.calls.push(args.to_vec());
            self.replies.pop_front().expect("unexpected command")
        }
    }
    fn owned() -> String {
        serde_json::json!([
            {"name":SINK_NAME,"owner_module":42,"properties":{"hoki.airplay":OWNER}},
            {"name":"external","owner_module":43,"properties":{}}
        ])
        .to_string()
    }
    #[test]
    fn only_selected_speaker_is_loaded_and_old_owned_sink_is_removed_first() {
        let list = parse_browse(
            &[
                record("First", "192.168.1.2", ""),
                record("Second", "192.168.1.3", ""),
            ]
            .join("\n"),
        );
        let mut pulse = Fake {
            calls: vec![],
            replies: [Ok(owned()), Ok("".into()), Ok("44".into()), Ok("".into())].into(),
        };
        connect(&mut pulse, &list[1]).unwrap();
        assert_eq!(pulse.calls[1], ["unload-module", "42"]);
        assert_eq!(pulse.calls[2][1], "module-raop-sink");
        assert!(pulse.calls[2].contains(&"server=[192.168.1.3]:5000".into()));
        assert_eq!(pulse.calls[3], ["set-default-sink", SINK_NAME]);
        assert!(pulse.replies.is_empty());
    }
    #[test]
    fn failed_load_reconciles_possibly_created_sink_without_retry() {
        let speaker = parse_browse(&record("First", "192.168.1.2", "")).remove(0);
        let mut pulse = Fake {
            calls: vec![],
            replies: [
                Ok("[]".into()),
                Err("timeout".into()),
                Ok(owned()),
                Ok("".into()),
            ]
            .into(),
        };
        assert!(connect(&mut pulse, &speaker).is_err());
        assert_eq!(pulse.calls[3], ["unload-module", "42"]);
        assert_eq!(
            pulse.calls.iter().filter(|c| c[0] == "load-module").count(),
            1
        );
    }
}
