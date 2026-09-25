//! Preconditions for a recording-owned suspend attempt; never release wake holds.
use crate::{count, healthy, recovery_matches, Result};
use serde_json::Value;
/// A returned mem write can abort without sleeping. Keep a cooldown in that case.
/// This threshold is pacing policy, not a continuity or energy guarantee.
pub fn return_needs_cooldown(elapsed: f64, awake: f64) -> bool {
    !elapsed.is_finite()
        || !awake.is_finite()
        || elapsed < 0.0
        || awake < 0.0
        || elapsed - awake < 0.5
}
#[derive(Clone, Copy)]
pub enum SuspendWrite {
    WakeupCount,
    Mem,
}
impl SuspendWrite {
    pub fn name(self) -> &'static str {
        match self {
            Self::WakeupCount => "wakeup_count_commit",
            Self::Mem => "mem_write",
        }
    }
    pub fn retryable(self, errno: Option<i32>) -> bool {
        matches!(
            (self, errno),
            (Self::WakeupCount, Some(libc::EINVAL)) | (Self::Mem, Some(libc::EBUSY))
        )
    }
}
pub fn power_blocker(battery: &str, usb: &str) -> Option<&'static str> {
    if battery.trim() != "Discharging" {
        Some("battery_not_discharging")
    } else if usb.trim() != "DISCONNECTED" {
        Some("usb_not_disconnected")
    } else {
        None
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum RecordingReadiness {
    Ready,
    PendingDurability,
}
pub fn recording_readiness(meta: &Value, boot: &str, status: &Value) -> Result<RecordingReadiness> {
    if meta["phase"] != "started"
        || !recovery_matches(meta, boot, status)?
        || !meta["activation_complete_boottime_seconds"].is_number()
        || meta["activated_handles"]
            .as_array()
            .is_none_or(|v| v.is_empty())
    {
        return Err("recording is not activated or does not own this session".into());
    }
    healthy(status)?;
    if status["storage_status"] != 1
        || status["stopped"] != false
        || status["stop_requested"] != false
        || status["storage_aborted"] == true
    {
        return Err("recording is stopping or storage is unavailable".into());
    }
    let received = count(status, "received")?;
    let submitted = count(status, "submitted_records")?;
    let durable = count(status, "durable_records")?;
    let held = status["wake_held"].as_bool().ok_or("missing wake state")?;
    if durable > submitted || submitted > received {
        return Err("inconsistent recorder durability counters".into());
    }
    if received != durable && !held {
        return Err("undurable data is not protected by the recording wake hold".into());
    }
    Ok(if held || received != durable {
        RecordingReadiness::PendingDurability
    } else {
        RecordingReadiness::Ready
    })
}
pub fn recording_ready(meta: &Value, boot: &str, status: &Value) -> Result<()> {
    match recording_readiness(meta, boot, status)? {
        RecordingReadiness::Ready => Ok(()),
        RecordingReadiness::PendingDurability => {
            Err("recording has pending durability work".into())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ID: &str = "12345678-1234-1234-1234-123456789abc";
    #[test]
    fn returned_call_pacing_requires_measured_sleep() {
        for (elapsed, awake) in [
            (0.0, 0.0),
            (1.0, 1.0),
            (1.0, 1.001),
            (0.599, 0.1),
            (f64::NAN, 0.0),
            (f64::INFINITY, 0.0),
            (1.0, f64::NEG_INFINITY),
            (1.0, f64::NAN),
            (-1.0, 0.0),
            (1.0, -1.0),
        ] {
            assert!(return_needs_cooldown(elapsed, awake));
        }
        assert!(!return_needs_cooldown(0.75, 0.25));
        assert!(!return_needs_cooldown(10.0, 0.15));
    }
    #[test]
    fn retry_errors_are_stage_specific() {
        for errno in [
            None,
            Some(libc::EINVAL),
            Some(libc::EBUSY),
            Some(libc::EIO),
            Some(libc::EPERM),
        ] {
            assert_eq!(
                SuspendWrite::WakeupCount.retryable(errno),
                errno == Some(libc::EINVAL)
            );
            assert_eq!(
                SuspendWrite::Mem.retryable(errno),
                errno == Some(libc::EBUSY)
            );
        }
    }
    #[test]
    fn healthy_pending_work_defers_but_failures_do_not() {
        let meta = json!({"phase":"started","boot_id":ID,"session_id":ID,
            "activation_complete_boottime_seconds":1.0,"activated_handles":[1]});
        let status = json!({"session_id":ID,"storage_status":1,"stopped":false,
            "stop_requested":false,"wake_held":true,"flush_failed":false,
            "wake_error":0,"dropped":"0","input_failures":"0",
            "received":"6","submitted_records":"5","durable_records":"5"});
        assert_eq!(
            recording_readiness(&meta, ID, &status).unwrap(),
            RecordingReadiness::PendingDurability
        );
        for (received, submitted, durable) in [(6, 6, 5), (5, 5, 5)] {
            let mut pending = status.clone();
            pending["received"] = json!(received.to_string());
            pending["submitted_records"] = json!(submitted.to_string());
            pending["durable_records"] = json!(durable.to_string());
            assert_eq!(
                recording_readiness(&meta, ID, &pending).unwrap(),
                RecordingReadiness::PendingDurability
            );
        }
        for (key, value) in [
            ("dropped", json!("1")),
            ("input_failures", json!("1")),
            ("wake_error", json!(-5)),
            ("storage_status", json!(-28)),
            ("storage_aborted", json!(true)),
            ("stop_requested", json!(true)),
            ("flush_failed", json!(true)),
            ("wake_held", json!(false)),
            ("wake_held", Value::Null),
            ("durable_records", json!("7")),
            ("submitted_records", json!("7")),
            ("received", json!("bad")),
            ("session_id", json!("other")),
        ] {
            let mut bad = status.clone();
            bad[key] = value;
            assert!(recording_readiness(&meta, ID, &bad).is_err(), "{key}");
        }
        assert!(recording_readiness(&meta, "other", &status).is_err());
    }
    #[test]
    fn refuses_external_power_and_unknown_states() {
        assert_eq!(power_blocker("Discharging\n", "DISCONNECTED\n"), None);
        for battery in ["Charging", "Full", "Unknown", ""] {
            assert!(power_blocker(battery, "DISCONNECTED").is_some());
        }
        for usb in ["CONFIGURED", "CONNECTED", ""] {
            assert!(power_blocker("Discharging", usb).is_some());
        }
    }
    #[test]
    fn requires_owned_active_durable_recording() {
        let m = json!({"phase":"started","boot_id":ID,"session_id":ID,"activation_complete_boottime_seconds":1.0,"activated_handles":[1]});
        let s = json!({"session_id":ID,"storage_status":1,"stopped":false,"stop_requested":false,"wake_held":false,"flush_failed":false,"wake_error":0,"dropped":"0","input_failures":"0","received":"5","submitted_records":"5","durable_records":"5"});
        assert!(recording_ready(&m, ID, &s).is_ok());
        for (key, value) in [
            ("stopped", json!(true)),
            ("stop_requested", json!(true)),
            ("wake_held", json!(true)),
            ("received", json!("6")),
            ("submitted_records", json!("6")),
            ("dropped", json!("1")),
            ("storage_status", json!(-1)),
            ("session_id", json!("other")),
        ] {
            let mut changed = s.clone();
            changed[key] = value;
            assert!(recording_ready(&m, ID, &changed).is_err(), "{key}");
        }
        for (key, value) in [
            ("phase", json!("closed")),
            ("activated_handles", json!([])),
            ("activation_complete_boottime_seconds", Value::Null),
            ("boot_id", json!("other")),
        ] {
            let mut changed = m.clone();
            changed[key] = value;
            assert!(recording_ready(&changed, ID, &s).is_err(), "{key}");
        }
    }
}
