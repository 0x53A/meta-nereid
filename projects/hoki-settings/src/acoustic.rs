//! Optional acoustic responder, managed in the Settings user's systemd session.
use std::process::Command;

const UNIT: &str = "acoustic-link.service";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub available: bool,
    pub on: bool,
    pub failed: bool,
    pub transition: Option<bool>,
}
impl State {
    pub fn label(&self) -> &'static str {
        match self.transition {
            Some(true) => "turning on",
            Some(false) => "turning off",
            None if self.failed => "error",
            None if self.on => "on",
            None => "off",
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
    let enabled = properties.get("UnitFileState").copied().unwrap_or("");
    State {
        available: matches!(load, "loaded" | "masked" | "error" | "bad-setting"),
        on: matches!(enabled, "enabled" | "enabled-runtime")
            || matches!(
                active,
                "active" | "activating" | "reloading" | "deactivating"
            ),
        transition: match active {
            "activating" | "reloading" => Some(true),
            "deactivating" => Some(false),
            _ => None,
        },
        failed: active == "failed" || matches!(load, "error" | "bad-setting"),
    }
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(["--user", "--no-ask-password"])
        .args(args)
        .arg(UNIT)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(if error.trim().is_empty() {
            format!("Acoustic SSH: {}", output.status)
        } else {
            error.trim().into()
        })
    }
}

pub fn status() -> Result<State, String> {
    if crate::simulated::enabled() {
        let value = crate::simulated::read("acoustic", "missing");
        return Ok(State {
            available: value != "missing",
            on: value == "on" || value == "error",
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

fn set_enabled_with(enabled: bool, mut run: impl FnMut(&[&str]) -> Result<String, String>) -> Result<String, String> {
    let state = parse_state(&run(&[
        "show",
        "--property=LoadState,ActiveState,UnitFileState",
    ])?);
    if !state.available {
        return Err("Acoustic SSH service is not installed".into());
    }
    run(&[if enabled { "enable" } else { "disable" }, "--now"])?;
    Ok(String::new())
}

pub fn set_enabled(enabled: bool) -> Result<String, String> {
    set_enabled_with(enabled, systemctl)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_service_never_mutates_systemd() {
        let mut calls = 0;
        assert!(set_enabled_with(true, |_| {
            calls += 1;
            Ok("LoadState=not-found\n".into())
        })
        .is_err());
        assert_eq!(calls, 1);
        assert!(!parse_state("LoadState=not-found\nActiveState=inactive\n").available);
    }
    #[test]
    fn handles_manual_start_and_failed_enabled_service() {
        for (properties, expected) in [
            (
                "LoadState=loaded\nActiveState=inactive\nUnitFileState=disabled",
                "enable",
            ),
            (
                "LoadState=loaded\nActiveState=active\nUnitFileState=disabled",
                "disable",
            ),
            (
                "LoadState=loaded\nActiveState=failed\nUnitFileState=enabled",
                "disable",
            ),
        ] {
            let mut calls = 0;
            set_enabled_with(expected == "enable", |args| {
                calls += 1;
                if calls == 1 {
                    return Ok(properties.into());
                }
                assert_eq!(args, &[expected, "--now"]);
                Ok(String::new())
            })
            .unwrap();
            assert_eq!(calls, 2);
        }
        assert_eq!(
            parse_state("LoadState=loaded\nActiveState=failed").label(),
            "error"
        );
    }
    #[test]
    fn external_transitions_are_visible() {
        assert_eq!(
            parse_state("LoadState=loaded\nActiveState=activating").label(),
            "turning on"
        );
        assert_eq!(
            parse_state("LoadState=loaded\nActiveState=deactivating").label(),
            "turning off"
        );
    }
    #[test]
    fn errors_reach_the_ui() {
        assert_eq!(
            set_enabled_with(true, |_| Err("bus unavailable".into())),
            Err("bus unavailable".into())
        );
        let mut calls = 0;
        assert_eq!(
            set_enabled_with(true, |_| {
                calls += 1;
                if calls == 1 {
                    Ok("LoadState=masked\n".into())
                } else {
                    Err("unit is masked".into())
                }
            }),
            Err("unit is masked".into())
        );
    }
}
