//! Explicit desktop hardware substitute. No command or bus access in this mode.
use std::path::PathBuf;

fn root() -> Option<PathBuf> {
    std::env::var_os("HOKI_SIM_STATE").map(PathBuf::from)
}
pub fn enabled() -> bool {
    root().is_some()
}
fn volume_at(root: &std::path::Path) -> i32 {
    std::fs::read_to_string(root.join("acoustic-volume")).ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|v| (0..=100).contains(v))
        .unwrap_or(crate::acoustic_volume::DEFAULT)
}
pub fn volume() -> Option<i32> {
    root().map(|root| volume_at(&root))
}
pub fn sysfs_path(path: &str) -> PathBuf {
    root().map_or_else(
        || PathBuf::from(path),
        |r| r.join(path.trim_start_matches('/')),
    )
}
pub fn read(name: &str, default: &str) -> String {
    root()
        .and_then(|r| std::fs::read_to_string(r.join(name)).ok())
        .unwrap_or_else(|| default.into())
        .trim()
        .into()
}
pub fn radio_status() -> Option<(String, String, String)> {
    root()?;
    let mode = read("radio", "off");
    Some((
        if mode.contains("wifi") { "on" } else { "off" }.into(),
        if mode.contains("bt") { "on" } else { "off" }.into(),
        if read("offline", "false") == "true" { "on" } else { "off" }.into(),
    ))
}
pub fn action(action: &str) -> Result<String, String> {
    let root = root().ok_or("Simulator state missing")?;
    action_at(&root, action)
}

fn action_at(root: &std::path::Path, action: &str) -> Result<String, String> {
    if let Some(value) = action.strip_prefix("acoustic-volume:") {
        let percent = value.parse::<i32>().map_err(|e| e.to_string())?;
        std::fs::write(root.join("acoustic-volume"), format!("{}\n", percent.clamp(0, 100)))
            .map_err(|e| e.to_string())?;
        return Ok(String::new());
    }
    let radio = std::fs::read_to_string(root.join("radio")).unwrap_or_else(|_| "off".into());
    // Absolute requests are idempotent even if state changed since the UI read it.
    if let Some((kind, target @ ("on" | "off"))) = action.split_once(':') {
        let enabled = target == "on";
        let current = match kind {
            "set-wifi" => radio.contains("wifi"),
            "set-bt" => radio.contains("bt"),
            "set-airplane" => std::fs::read_to_string(root.join("offline")).unwrap_or_default().trim() == "true",
            "set-acoustic" => {
                let state = std::fs::read_to_string(root.join("acoustic"))
                    .map_err(|_| "Acoustic SSH service is not installed".to_string())?;
                if state.trim() == "missing" { return Err("Acoustic SSH service is not installed".into()); }
                std::fs::write(root.join("acoustic"), target).map_err(|e| e.to_string())?;
                return Ok(String::new());
            }
            "set-recording" => {
                let state = std::fs::read_to_string(root.join("recording"))
                    .unwrap_or_else(|_| "missing".into());
                if state.trim() == "missing" {
                    return Err("Health recording service is not installed".into());
                }
                if state.trim() != target {
                    std::fs::write(root.join("recording"), target).map_err(|e| e.to_string())?;
                }
                return Ok(if enabled {
                    "Starting recording…".into()
                } else {
                    "Stopping recording…".into()
                });
            }
            _ => return Err(format!("Unsupported simulated action: {action}")),
        };
        if current != enabled {
            action_at(root, &format!("toggle-{}", kind.trim_start_matches("set-")))?;
        }
        return Ok(String::new());
    }
    let (name, value) = match action {
        "toggle-acoustic" => {
            let state = std::fs::read_to_string(root.join("acoustic"))
                .map_err(|_| "Acoustic SSH service is not installed".to_string())?;
            if state.trim() == "missing" { return Err("Acoustic SSH service is not installed".into()); }
            ("acoustic", if matches!(state.trim(), "on" | "error") { "off" } else { "on" })
        },
        "toggle-recording" => {
            let state = std::fs::read_to_string(root.join("recording"))
                .map_err(|_| "Health recording service is not installed".to_string())?;
            if state.trim() == "missing" {
                return Err("Health recording service is not installed".into());
            }
            ("recording", if state.trim() == "on" { "off" } else { "on" })
        },
        "toggle-wifi" => ("radio", match (radio.contains("wifi"), radio.contains("bt")) {
            (false, false) => "wifi", (false, true) => "wifi+bt",
            (true, false) => "off", (true, true) => "bt",
        }),
        "toggle-bt" => ("radio", match (radio.contains("wifi"), radio.contains("bt")) {
            (false, false) => "bt", (true, false) => "wifi+bt",
            (false, true) => "off", (true, true) => "wifi",
        }),
        "toggle-airplane" => {
            let offline = std::fs::read_to_string(root.join("offline")).unwrap_or_default();
            let restored;
            let mode = if offline.trim() == "true" {
                restored = std::fs::read_to_string(root.join("radio-before-airplane"))
                    .unwrap_or_else(|_| "off".into());
                restored.trim()
            } else {
                std::fs::write(root.join("radio-before-airplane"), radio.trim()).map_err(|e| e.to_string())?;
                "off"
            };
            std::fs::write(root.join("radio"), mode).map_err(|e| e.to_string())?;
            ("offline", if offline.trim() == "true" { "false" } else { "true" })
        },
        "poweroff" | "reboot" | "bootloader" => ("last-power-action", action),
        "set-usb-developer" => ("usb", "developer_mode"),
        "set-usb-adb" => ("usb", "adb_mode"),
        "set-usb-charging" => ("usb", "charging_only"),
        _ => return Err(format!("Unsupported simulated action: {action}")),
    };
    std::fs::write(root.join(name), value).map_err(|e| e.to_string())?;
    if action == "toggle-recording" {
        Ok(if value == "on" {
            "Starting recording…".into()
        } else {
            "Stopping recording…".into()
        })
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hardware_actions_only_change_the_private_state() {
        let directory =
            std::env::temp_dir().join(format!("hoki-settings-sim-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        for action in ["poweroff", "reboot", "bootloader"] {
            super::action_at(&directory, action).unwrap();
            assert_eq!(
                std::fs::read_to_string(directory.join("last-power-action")).unwrap(),
                action
            );
        }
        super::action_at(&directory, "toggle-bt").unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("radio")).unwrap(),
            "bt"
        );
        super::action_at(&directory, "toggle-airplane").unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("radio")).unwrap(),
            "off"
        );
        assert_eq!(std::fs::read_to_string(directory.join("offline")).unwrap(), "true");
        super::action_at(&directory, "toggle-airplane").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("radio")).unwrap(), "bt");
        super::action_at(&directory, "toggle-bt").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("radio")).unwrap(), "off");
        assert_eq!(std::fs::read_to_string(directory.join("offline")).unwrap(), "false");
        super::action_at(&directory, "toggle-airplane").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("offline")).unwrap(), "true");
        super::action_at(&directory, "toggle-airplane").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("radio")).unwrap(), "off");
        super::action_at(&directory, "set-usb-adb").unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("usb")).unwrap(),
            "adb_mode"
        );
        assert!(super::action_at(&directory, "toggle-acoustic").is_err());
        std::fs::write(directory.join("acoustic"), "off").unwrap();
        super::action_at(&directory, "toggle-acoustic").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("acoustic")).unwrap(), "on");
        super::action_at(&directory, "toggle-acoustic").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("acoustic")).unwrap(), "off");
        std::fs::write(directory.join("recording"), "off").unwrap();
        super::action_at(&directory, "toggle-recording").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("recording")).unwrap(), "on");
        super::action_at(&directory, "set-recording:on").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("recording")).unwrap(), "on");
        // Absolute requests are safe to repeat and never invert the target.
        super::action_at(&directory, "set-recording:on").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("recording")).unwrap(), "on");
        super::action_at(&directory, "set-recording:off").unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("recording")).unwrap(), "off");
        assert!(super::action_at(&directory, "unknown").is_err());
        for action in ["set-wifi:on", "set-bt:on", "set-airplane:on", "set-airplane:off", "set-acoustic:on", "set-acoustic:off"] {
            super::action_at(&directory, action).unwrap();
            let snapshot = || ["radio", "offline", "acoustic"].map(|name| std::fs::read_to_string(directory.join(name)).ok());
            let before = snapshot();
            super::action_at(&directory, action).unwrap();
            assert_eq!(snapshot(), before);
        }
        assert_eq!(super::volume_at(&directory), 30);
        super::action_at(&directory, "acoustic-volume:73").unwrap();
        assert_eq!(super::volume_at(&directory), 73);
        super::action_at(&directory, "acoustic-volume:1000").unwrap();
        assert_eq!(super::volume_at(&directory), 100);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
