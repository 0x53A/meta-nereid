//! Absolute action targets and confirmation of observed backend state.
use crate::acoustic;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub wifi: String,
    pub bt: String,
    pub airplane: String,
    pub usb: String,
    pub acoustic: acoustic::State,
    pub volume: i32,
    pub recording: crate::health_recording::State,
}

#[derive(Default)]
pub struct PollVersion(AtomicU64);
impl PollVersion {
    pub fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    pub fn advance(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    pub fn accepts(&self, version: u64, busy: bool) -> bool {
        !busy && version == self.current()
    }
}

pub fn prepare(action: &str, state: &Snapshot) -> Result<String, String> {
    let current = match action {
        "toggle-wifi" => &state.wifi,
        "toggle-bt" => &state.bt,
        "toggle-airplane" => &state.airplane,
        "toggle-acoustic" => {
            if !state.acoustic.available || state.acoustic.transition.is_some() {
                return Err("Acoustic SSH is unavailable or still changing.".into());
            }
            return Ok(format!(
                "set-acoustic:{}",
                if state.acoustic.on { "off" } else { "on" }
            ));
        }
        "toggle-recording" => {
            if !state.recording.available || state.recording.transition == Some(false) {
                return Err("Health recording is unavailable or still changing.".into());
            }
            return Ok(format!(
                "set-recording:{}",
                if state.recording.on || state.recording.transition == Some(true) { "off" } else { "on" }
            ));
        }
        _ => return Ok(action.into()),
    };
    let target = match current.as_str() {
        "on" => "off",
        "off" => "on",
        _ => return Err("Waiting for the current radio state. Try again shortly.".into()),
    };
    Ok(format!(
        "set-{}:{target}",
        action.trim_start_matches("toggle-")
    ))
}

pub fn transition_label(action: &str) -> Option<&'static str> {
    match action.split_once(':') {
        Some(("set-recording", "on")) => Some("starting"),
        Some(("set-recording", "off")) => Some("stopping"),
        Some(("set-wifi" | "set-bt" | "set-airplane" | "set-acoustic", "on")) => Some("turning on"),
        Some(("set-wifi" | "set-bt" | "set-airplane" | "set-acoustic", "off")) => {
            Some("turning off")
        }
        _ => None,
    }
}

fn confirmed(action: &str, state: &Snapshot) -> bool {
    match action.split_once(':') {
        Some(("set-wifi", target)) => state.wifi == target,
        Some(("set-bt", target)) => state.bt == target,
        Some(("set-airplane", target)) => state.airplane == target,
        Some(("set-acoustic", target)) => {
            state.acoustic.available
                && state.acoustic.transition.is_none()
                && (target == "off" || !state.acoustic.failed)
                && state.acoustic.on == (target == "on")
        }
        Some(("set-recording", target)) => {
            state.recording.available
                && state.recording.transition.is_none()
                && !state.recording.failed
                && state.recording.on == (target == "on")
        }
        _ => match action {
            "set-usb-developer" => state.usb == "SSH",
            "set-usb-adb" => state.usb == "ADB",
            "set-usb-charging" => state.usb == "Charge",
            _ => true,
        },
    }
}

/// Keep the UI busy until the requested state is observed. Failures and
/// reboot-required replies still refresh actual state, without claiming success.
pub fn settle(
    action: &str,
    mut result: Result<String, String>,
    mut read: impl FnMut() -> Snapshot,
    mut retry: impl FnMut() -> bool,
) -> (Result<String, String>, Snapshot) {
    let mut state = read();
    if matches!(&result, Ok(message) if message.is_empty()) {
        while !confirmed(action, &state) {
            if action.starts_with("set-recording:")
                && (!state.recording.available || state.recording.failed)
            {
                result = Err("Health recording is unavailable or failed. Showing the current state.".into());
                break;
            }
            if !retry() {
                result =
                    Err("The requested state was not confirmed. Showing the current state.".into());
                break;
            }
            state = read();
        }
    }
    (result, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn targets_are_absolute_and_transitions_are_directional() {
        let mut state = Snapshot {
            wifi: "off".into(),
            ..Default::default()
        };
        let action = prepare("toggle-wifi", &state).unwrap();
        assert_eq!(action, "set-wifi:on");
        assert_eq!(transition_label(&action), Some("turning on"));
        state.wifi = "on".into();
        assert!(confirmed(&action, &state)); // an external enable cannot invert our request
        assert_eq!(
            transition_label(&prepare("toggle-wifi", &state).unwrap()),
            Some("turning off")
        );
        state.wifi = "?".into();
        assert!(prepare("toggle-wifi", &state).is_err());
    }
    #[test]
    fn rejects_polls_started_before_or_during_an_action() {
        let version = PollVersion::default();
        let before = version.current();
        version.advance();
        let during = version.current();
        assert!(!version.accepts(before, true));
        assert!(!version.accepts(during, true));
        version.advance();
        assert!(!version.accepts(before, false));
        assert!(!version.accepts(during, false));
        assert!(version.accepts(version.current(), false));
    }
    #[test]
    fn waits_for_observed_state_in_both_directions() {
        for (action, old, new) in [("set-wifi:on", "off", "on"), ("set-wifi:off", "on", "off")] {
            let mut reads = 0;
            let (result, state) = settle(
                action,
                Ok(String::new()),
                || {
                    reads += 1;
                    Snapshot {
                        wifi: if reads < 3 { old } else { new }.into(),
                        ..Default::default()
                    }
                },
                || true,
            );
            assert!(result.is_ok());
            assert_eq!(reads, 3);
            assert_eq!(state.wifi, new);
        }
    }
    #[test]
    fn failure_timeout_and_reboot_required_preserve_actual_state() {
        for result in [
            Err("backend failed".into()),
            Ok("Restart required".into()),
            Ok(String::new()),
        ] {
            let expected = result.clone();
            let (result, state) = settle(
                "set-usb-adb",
                result,
                || Snapshot {
                    usb: "SSH".into(),
                    ..Default::default()
                },
                || false,
            );
            assert_eq!(state.usb, "SSH");
            if expected == Ok(String::new()) {
                assert!(result.is_err());
            } else {
                assert_eq!(result, expected);
            }
        }
    }
    #[test]
    fn acoustic_transition_is_not_a_confirmed_on_state() {
        let mut state = Snapshot::default();
        state.acoustic = acoustic::State {
            available: true,
            on: true,
            transition: Some(true),
            failed: false,
        };
        assert!(!confirmed("set-acoustic:on", &state));
        assert!(prepare("toggle-acoustic", &state).is_err());
        state.acoustic.transition = None;
        assert!(confirmed("set-acoustic:on", &state));
        state.acoustic.failed = true;
        assert!(!confirmed("set-acoustic:on", &state));
    }

    #[test]
    fn recording_targets_are_absolute_and_require_observed_state() {
        let mut state = Snapshot {
            recording: crate::health_recording::State {
                available: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(prepare("toggle-recording", &state).unwrap(), "set-recording:on");
        assert_eq!(transition_label("set-recording:on"), Some("starting"));
        state.recording.on = true;
        assert!(confirmed("set-recording:on", &state));
        assert_eq!(prepare("toggle-recording", &state).unwrap(), "set-recording:off");
        assert_eq!(transition_label("set-recording:off"), Some("stopping"));
        state.recording.transition = Some(false);
        assert!(prepare("toggle-recording", &state).is_err());
    }

    #[test]
    fn recording_failure_stops_settle_without_waiting_for_long_timeout() {
        let (result, state) = settle(
            "set-recording:on",
            Ok(String::new()),
            || Snapshot {
                recording: crate::health_recording::State {
                    available: true,
                    failed: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            || panic!("failed recording must not keep polling"),
        );
        assert!(result.is_err());
        assert!(state.recording.failed);
    }
}
