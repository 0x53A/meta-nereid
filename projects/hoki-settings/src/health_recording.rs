//! System service state for the optional health recording backend.
//!
//! The recorder is a system service because it owns the sensor path. Settings
//! only reads its state and requests explicit start/stop operations; it never
//! enables the unit at boot.
use std::process::Command;

const UNIT: &str = "hoki-health-recording.service";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub available: bool,
    pub on: bool,
    pub failed: bool,
    pub transition: Option<bool>,
}

impl State {
    pub fn label(&self) -> &'static str {
        if !self.available {
            "unavailable"
        } else {
            match self.transition {
                Some(true) => "starting",
                Some(false) => "stopping",
                None if self.failed => "error",
                None if self.on => "on",
                None => "off",
            }
        }
    }
}

fn parse_state(text: &str) -> State {
    let properties: std::collections::HashMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let load = properties.get("LoadState").copied().unwrap_or("not-found");
    let active = properties.get("ActiveState").copied().unwrap_or("");
    State {
        available: load != "not-found" && !load.is_empty(),
        on: active == "active",
        transition: match active {
            "activating" => Some(true),
            "deactivating" => Some(false),
            _ => None,
        },
        failed: active == "failed" || matches!(load, "error" | "bad-setting"),
    }
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(["--no-ask-password"])
        .args(args)
        .arg(UNIT)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(if error.trim().is_empty() {
            format!("Health recording: {}", output.status)
        } else {
            error.trim().into()
        })
    }
}

pub fn status() -> Result<State, String> {
    if crate::simulated::enabled() {
        let value = crate::simulated::read("recording", "missing");
        return Ok(State {
            available: value != "missing",
            on: value == "on",
            failed: value == "error",
            transition: match value.as_str() {
                "starting" => Some(true),
                "stopping" => Some(false),
                _ => None,
            },
        });
    }
    systemctl(&["show", "--property=LoadState,ActiveState,UnitFileState"]).map(|s| parse_state(&s))
}

fn set_enabled_with(
    enabled: bool,
    mut run: impl FnMut(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    let state = parse_state(&run(&[
        "show",
        "--property=LoadState,ActiveState,UnitFileState",
    ])?);
    if !state.available {
        return Err("Health recording service is not installed".into());
    }
    run(&["--no-block", if enabled { "start" } else { "stop" }])?;
    Ok(if enabled {
        "Starting recording…".into()
    } else {
        "Stopping recording…".into()
    })
}

pub fn set_enabled(enabled: bool) -> Result<String, String> {
    set_enabled_with(enabled, systemctl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_only_means_recording_ready() {
        assert_eq!(parse_state("LoadState=loaded\nActiveState=active\n").label(), "on");
        assert_eq!(parse_state("LoadState=loaded\nActiveState=activating\n").label(), "starting");
        assert_eq!(parse_state("LoadState=loaded\nActiveState=deactivating\n").label(), "stopping");
        assert_eq!(parse_state("LoadState=loaded\nActiveState=inactive\n").label(), "off");
        assert_eq!(parse_state("LoadState=loaded\nActiveState=failed\n").label(), "error");
        assert_eq!(parse_state("LoadState=not-found\nActiveState=inactive\n").label(), "unavailable");
    }

    #[test]
    fn start_stop_are_system_service_no_block_requests() {
        for enabled in [true, false] {
            let mut calls = Vec::new();
            let result = set_enabled_with(enabled, |args| {
                calls.push(args.iter().map(|value| value.to_string()).collect::<Vec<_>>());
                if calls.len() == 1 {
                    Ok("LoadState=loaded\nActiveState=inactive\n".into())
                } else {
                    Ok(String::new())
                }
            })
            .unwrap();
            assert_eq!(
                result,
                if enabled {
                    "Starting recording…"
                } else {
                    "Stopping recording…"
                }
            );
            assert_eq!(calls[1], vec!["--no-block", if enabled { "start" } else { "stop" }]);
        }
    }

    #[test]
    fn missing_service_never_mutates_systemd() {
        let mut calls = 0;
        assert!(set_enabled_with(true, |_| {
            calls += 1;
            Ok("LoadState=not-found\nActiveState=inactive\n".into())
        })
        .is_err());
        assert_eq!(calls, 1);
    }
}
