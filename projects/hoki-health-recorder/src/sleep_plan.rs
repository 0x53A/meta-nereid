//! Prepare narrowly scoped sleep configuration changes; no device writes.
use crate::{select_ssc, valid_session_id, Result};
use serde_json::{json, Value};
use std::ops::Range;

struct Field {
    number: u64,
    wire: u8,
    data: Range<usize>,
    value: u64,
}
fn varint(data: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let b = *data.get(*at).ok_or("truncated protobuf varint")?;
        *at += 1;
        if shift == 63 && b > 1 {
            return Err("protobuf varint overflow".into());
        }
        value |= ((b & 127) as u64) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err("invalid protobuf varint".into())
}
fn fields(data: &[u8]) -> Result<Vec<Field>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < data.len() {
        let key = varint(data, &mut at)?;
        let number = key >> 3;
        let wire = (key & 7) as u8;
        if number == 0 || number > 0x1fffffff {
            return Err("invalid protobuf field".into());
        }
        let (start, end, value) = match wire {
            0 => {
                let start = at;
                let value = varint(data, &mut at)?;
                (start, at, value)
            }
            1 | 5 | 2 => {
                let size = match wire {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(varint(data, &mut at)?)?,
                };
                let start = at;
                at = at
                    .checked_add(size)
                    .filter(|end| *end <= data.len())
                    .ok_or("truncated protobuf field")?;
                (start, at, 0)
            }
            _ => return Err("unsupported protobuf wire type".into()),
        };
        out.push(Field {
            number,
            wire,
            data: start..end,
            value,
        });
    }
    Ok(out)
}
fn one(data: &[u8], number: u64, wire: u8) -> Result<Field> {
    let mut matches = fields(data)?.into_iter().filter(|f| f.number == number);
    let field = matches.next().ok_or("missing configuration field")?;
    if field.wire != wire || matches.next().is_some() {
        return Err("duplicate or mistyped configuration field".into());
    }
    Ok(field)
}
fn flag(data: &[u8], number: u64) -> Result<(usize, u8)> {
    let f = one(data, number, 0)?;
    if f.value > 1 || f.data.len() != 1 {
        return Err("unsupported or noncanonical configuration flag".into());
    }
    Ok((f.data.start, f.value as u8))
}
fn decode(text: &str) -> Result<Vec<u8>> {
    if text.len() > 8192
        || text.len() % 2 != 0
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid configuration payload hex".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(&text[i..i + 2], 16)?))
        .collect()
}
fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
fn clean_archive(status: &Value) -> Result<()> {
    let accepted = status["accepted"]
        .as_u64()
        .ok_or("missing configuration archive count")?;
    if status["version"] != 1
        || status["phase"] != "closed"
        || status["archive_complete"] != true
        || status["archive_error"] != 0
        || status["rejected"] != 0
        || accepted == 0
        || status["durable"].as_u64() != Some(accepted)
        || status["accepted_not_confirmed_durable"] != 0
    {
        return Err("configuration archive incomplete".into());
    }
    Ok(())
}
pub fn prepare(
    inventory: &Value,
    discovery_status: &Value,
    snapshots: &[Value; 3],
    statuses: &[Value; 3],
    boot: &str,
) -> Result<Value> {
    let cfg = select_ssc(inventory, discovery_status, boot, "fsl_cfg")?;
    let sleep = select_ssc(inventory, discovery_status, boot, "fsl_sleep")?;
    let mut operations = Vec::new();
    let mut unchanged = Vec::new();
    let specs = [
        ("user", "--user-config", 769, 768, &cfg),
        ("tracking", "--tracking-config", 776, 776, &sleep),
        ("detect", "--detect-config", 876, 876, &sleep),
    ];
    for (i, (kind, mode, event, request, source)) in specs.into_iter().enumerate() {
        clean_archive(&statuses[i])?;
        let snapshot = &snapshots[i];
        if snapshot["version"] != 1
            || snapshot["boot_id"] != boot
            || snapshot["mode"] != mode
            || snapshot["event_id"] != event
            || snapshot["source"] != *source
            || !snapshot["session_id"]
                .as_str()
                .is_some_and(valid_session_id)
        {
            return Err("configuration snapshot identity mismatch".into());
        }
        let data = decode(
            snapshot["payload_hex"]
                .as_str()
                .ok_or("missing configuration payload")?,
        )?;
        let mut enabled = data.clone();
        let mut enable = Vec::new();
        let mut restore = Vec::new();
        let mut changed = Vec::new();
        if i == 0 {
            // Reply permission is field1; request permission is field3. Only RHR/sleep bits change.
            let permission = one(&data, 1, 2)?;
            let nested = &data[permission.data.clone()];
            for (field, name) in [
                (3, "resting_heart_rate_permission"),
                (4, "sleep_permission"),
            ] {
                let (offset, value) = flag(nested, field)?;
                if value == 0 {
                    enabled[permission.data.start + offset] = 1;
                    enable.extend_from_slice(&[(field * 8) as u8, 1]);
                    restore.extend_from_slice(&[(field * 8) as u8, value]);
                    changed.push(name);
                }
            }
            if !enable.is_empty() {
                enable.splice(0..0, [26, enable.len() as u8]);
                restore.splice(0..0, [26, restore.len() as u8]);
            }
        } else {
            let (offset, value) = flag(&data, 1)?;
            if value == 0 {
                enabled[offset] = 1;
                enable = vec![8, 1];
                restore = vec![8, value];
                changed.push(if i == 1 { "tracking" } else { "detect" });
            }
        }
        if changed.is_empty() {
            unchanged.push(kind);
            continue;
        }
        operations.push(
            json!({"kind":kind,"source":source,"request_id":request,"readback_id":event,
   "snapshot_session_id":snapshot["session_id"],"changed_fields":changed,
   "baseline_reply_hex":hex(&data),"enabled_reply_hex":hex(&enabled),
   "enable_payload_hex":hex(&enable),"restore_payload_hex":hex(&restore)}),
        );
    }
    Ok(
        json!({"version":1,"phase":"prepared","boot_id":boot,"discovery_session_id":inventory["session_id"],
  "scope":"sleep/RHR permissions and top-level sleep tracking/detection only",
  "discovery_inventory":inventory,"discovery_archive_status":discovery_status,
  "baseline_snapshots":snapshots,"baseline_archive_statuses":statuses,
  "operations":operations,"already_enabled":unchanged,"restore_order":"reverse_operations",
  "requires_baseline_revalidation":true,"requires_independent_restoration":true}),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    const BOOT: &str = "12345678-1234-1234-1234-123456789abc";
    pub(crate) fn fixture() -> (Value, Value, [Value; 3], [Value; 3]) {
        let cfg = "090101010101010101110202020202020202";
        let sleep = "090303030303030303110404040404040404";
        let inventory = json!({"version":1,"boot_id":BOOT,"session_id":BOOT,"parse_error":false,"interrupted":false,
   "streams":[{"data_type":"fsl_cfg","status":"unique","responses":1,"suids":[cfg]},
    {"data_type":"fsl_sleep","status":"unique","responses":1,"suids":[sleep]}]});
        let status = json!({"version":1,"phase":"closed","archive_complete":true,"archive_error":0,"rejected":0,"accepted":5,"durable":5,"accepted_not_confirmed_durable":0});
        let modes = [
            (
                "--user-config",
                769,
                cfg,
                "0a0808001001180020001208080010011800200a",
            ),
            (
                "--tracking-config",
                776,
                sleep,
                "080012060800103c18001a1508001211080015000000001d000000002500000000",
            ),
            (
                "--detect-config",
                876,
                sleep,
                "0800120c080010d80418002500000000",
            ),
        ];
        let snapshots=modes.map(|(mode,event,source,payload)|json!({"version":1,"boot_id":BOOT,"session_id":BOOT,"mode":mode,"event_id":event,"source":source,"payload_hex":payload}));
        (
            inventory,
            status.clone(),
            snapshots,
            [status.clone(), status.clone(), status],
        )
    }
    #[test]
    fn plan_preserves_nested_settings_and_restores_only_owned_flags() {
        let (i, d, s, a) = fixture();
        let p = prepare(&i, &d, &s, &a, BOOT).unwrap();
        let ops = p["operations"].as_array().unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0]["enable_payload_hex"], "1a0418012001");
        assert_eq!(ops[0]["restore_payload_hex"], "1a0418002000");
        assert_eq!(
            ops[0]["enabled_reply_hex"],
            "0a0808001001180120011208080010011800200a"
        );
        for index in [1, 2] {
            assert_eq!(ops[index]["enable_payload_hex"], "0801");
            assert_eq!(ops[index]["restore_payload_hex"], "0800");
            assert_eq!(
                &ops[index]["enabled_reply_hex"].as_str().unwrap()[4..],
                &s[index]["payload_hex"].as_str().unwrap()[4..]
            );
        }
        let mut mixed = s.clone();
        mixed[0]["payload_hex"] = json!("0a0808001001180120001208080010011800200a");
        let p = prepare(&i, &d, &mixed, &a, BOOT).unwrap();
        assert_eq!(p["operations"][0]["enable_payload_hex"], "1a022001");
        assert_eq!(p["operations"][0]["restore_payload_hex"], "1a022000");
        for (index, op) in ops.iter().enumerate() {
            mixed[index]["payload_hex"] = op["enabled_reply_hex"].clone();
        }
        let p = prepare(&i, &d, &mixed, &a, BOOT).unwrap();
        assert!(p["operations"].as_array().unwrap().is_empty());
        assert_eq!(p["already_enabled"].as_array().unwrap().len(), 3);
        assert_eq!(p["baseline_snapshots"], json!(mixed));
    }
    #[test]
    fn plan_rejects_bad_identity_incomplete_archive_and_unsupported_flags() {
        let (i, d, s, a) = fixture();
        for (key, value) in [
            ("boot_id", json!("12345678-1234-1234-1234-123456789abd")),
            ("source", s[0]["source"].clone()),
            ("event_id", json!(876)),
            ("mode", json!("--detect-config")),
        ] {
            let mut bad = s.clone();
            bad[1][key] = value;
            assert!(prepare(&i, &d, &bad, &a, BOOT).is_err());
        }
        for payload in [
            "0802", "088000", "08000800", "1200", "080012ff", "0800ff", "xx",
        ] {
            let mut bad = s.clone();
            bad[1]["payload_hex"] = json!(payload);
            assert!(prepare(&i, &d, &bad, &a, BOOT).is_err(), "{payload}");
        }
        let mut bad = a.clone();
        bad[0]["durable"] = json!(4);
        assert!(prepare(&i, &d, &s, &bad, BOOT).is_err());
        let mut extended = s.clone();
        extended[1]["payload_hex"] =
            json!(format!("{}a00107", s[1]["payload_hex"].as_str().unwrap()));
        let p = prepare(&i, &d, &extended, &a, BOOT).unwrap();
        assert!(p["operations"][1]["enabled_reply_hex"]
            .as_str()
            .unwrap()
            .ends_with("a00107"));
    }
}
