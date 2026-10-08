use crate::sleep_client::Client;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
fn display_request(patch: Option<&Value>) -> Result<Value, String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let mut stream =
        std::os::unix::net::UnixStream::connect(format!("{runtime}/hoki-compositor.sock"))
            .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    let command = patch
        .map(|v| format!("configure-display {v}"))
        .unwrap_or_else(|| "display-status".into());
    writeln!(stream, "{command}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if reply["ok"] != true {
        return Err(reply["error"]
            .as_str()
            .unwrap_or("display settings unavailable")
            .into());
    }
    Ok(reply)
}
#[derive(Clone, Default)]
pub struct State {
    pub config: Value,
    pub reason: String,
}
pub fn status() -> State {
    let result = (|| {
        let mut c = Client::connect()?;
        c.request(json!({"command":"status"}))
    })();
    let mut state = match result {
        Ok(v) => State {
            config: v["config"].clone(),
            reason: v["reason"].as_str().unwrap_or("unknown").into(),
        },
        Err(_) => State {
            config: json!({}),
            reason: "power coordinator unavailable".into(),
        },
    };
    match display_request(None) {
        Ok(display) => {
            for (key, value) in display["config"].as_object().into_iter().flatten() {
                state.config[key] = value.clone();
            }
        }
        Err(_) => state.reason.push_str("; display settings unavailable"),
    }
    state
}
fn next<'a>(current: &str, values: &'a [&str]) -> &'a str {
    values[(values
        .iter()
        .position(|v| *v == current)
        .unwrap_or(values.len() - 1)
        + 1)
        % values.len()]
}
pub fn ambient_faces() -> Vec<String> {
    let mut faces = std::fs::read_dir("/usr/share/hoki/ambient-faces").into_iter().flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|s| s == "json") && e.path().is_file())
        .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    faces.sort();
    faces
}

fn choice_patch(action: &str) -> Result<Value, String> {
    let (kind, value) = action.split_once(':').ok_or("Invalid setting")?;
    Ok(match kind {
        "set-face-mode" if ["primary", "secondary", "automatic"].contains(&value) => json!({"face_mode":value}),
        "set-sensor-profile" if ["off", "daily", "sleep", "activity", "full"].contains(&value) => json!({"sensor_profile":value}),
        "set-idle-time" if ["15", "30", "60", "120"].contains(&value) => json!({"idle_seconds":value.parse::<u64>().unwrap()}),
        "set-ambient-face" if ambient_faces().iter().any(|f| f == value) => json!({"ambient_face":value}),
        _ => return Err("Unsupported setting value".into()),
    })
}

pub fn action(action: &str) -> Result<String, String> {
    let mut client = None;
    action_with(action, display_request, |request| {
        if client.is_none() {
            client = Some(Client::connect().map_err(|e| e.to_string())?);
        }
        client.as_mut().unwrap().request(request).map_err(|e| e.to_string())
    })
}

fn action_with(
    action: &str,
    mut display: impl FnMut(Option<&Value>) -> Result<Value, String>,
    mut power: impl FnMut(Value) -> Result<Value, String>,
) -> Result<String, String> {
    let display_action = matches!(
        action.split_once(':').map_or(action, |(kind, _)| kind),
        "cycle-face-mode" | "cycle-idle-time" | "cycle-ambient-face"
            | "set-face-mode" | "set-idle-time" | "set-ambient-face"
    );
    if action.starts_with("set-") {
        let patch = choice_patch(action)?;
        if display_action {
            display(Some(&patch))?;
        } else {
            power(json!({"command":"configure-patch", "patch":patch}))?;
        }
        return Ok(String::new());
    }
    let original = if display_action {
        display(None)?["config"].clone()
    } else {
        power(json!({"command":"status"}))?["config"].clone()
    };
    let mut config = original.clone();
    if action == "toggle-auto-cores" {
        power(json!({"command":"configure-auto-cores","patch":{"enabled":config["auto_cores"]["enabled"]!=true}}))?;
        return Ok(String::new());
    }
    match action {
        "toggle-sleep" => config["enabled"] = json!(config["enabled"] != true),
        "cycle-face-mode" => {
            config["face_mode"] = json!(next(
                config["face_mode"].as_str().unwrap_or(""),
                &["primary", "secondary", "automatic"]
            ))
        }
        "cycle-sensor-profile" => {
            config["sensor_profile"] = json!(next(
                config["sensor_profile"].as_str().unwrap_or(""),
                &["off", "daily", "sleep", "activity", "full"]
            ))
        }
        "cycle-idle-time" => {
            let values = [15, 30, 60, 120];
            let index = values
                .iter()
                .position(|n| Some(*n) == config["idle_seconds"].as_u64())
                .unwrap_or(3);
            config["idle_seconds"] = json!(values[(index + 1) % values.len()]);
        }
        "cycle-ambient-face" => {
            let mut faces = std::fs::read_dir("/usr/share/hoki/ambient-faces")
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|s| s == "json") && e.path().is_file())
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .collect::<Vec<_>>();
            faces.sort();
            if faces.is_empty() {
                return Err("No ambient faces installed".into());
            }
            let index = faces
                .iter()
                .position(|s| Some(s.as_str()) == config["ambient_face"].as_str())
                .unwrap_or(faces.len() - 1);
            config["ambient_face"] = json!(faces[(index + 1) % faces.len()]);
        }
        _ => return Err("Unknown sleep setting".into()),
    }
    let patch = changed_fields(&original, &config);
    if display_action {
        display(Some(&patch))?;
    } else {
        power(json!({"command":"configure-patch","patch":patch}))?;
    }
    Ok(String::new())
}
fn changed_fields(original: &Value, updated: &Value) -> Value {
    Value::Object(
        updated
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, value)| original[*key] != **value)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_display_choices_work_without_powerd_and_preserve_other_fields() {
        let mut config = json!({"face_mode":"automatic", "idle_seconds":30, "ambient_face":"hoki-digital"});
        for action in ["set-face-mode:secondary", "set-idle-time:60"] {
            action_with(action, |patch| {
                // Absolute selections do not read and resubmit a stale full config.
                let patch = patch.expect("absolute display choice must send a patch");
                assert_eq!(patch.as_object().unwrap().len(), 1);
                for (key, value) in patch.as_object().unwrap() { config[key] = value.clone(); }
                Ok(json!({"ok":true, "config":config}))
            }, |_| panic!("display choice must not contact powerd")).unwrap();
        }
        assert_eq!(config, json!({"face_mode":"secondary", "idle_seconds":60, "ambient_face":"hoki-digital"}));
    }
    #[test]
    fn sensor_choices_and_display_failures_do_not_cross_owners() {
        let mut sent = Vec::new();
        action_with("set-sensor-profile:full", |_| panic!("sensor choice must not contact compositor"), |request| {
            sent.push(request); Ok(json!({"ok":true}))
        }).unwrap();
        assert_eq!(sent, vec![json!({"command":"configure-patch", "patch":{"sensor_profile":"full"}})]);
        assert_eq!(action_with("set-face-mode:primary", |_| Err("compositor unavailable".into()),
            |_| panic!("display errors must not fall back to powerd")), Err("compositor unavailable".into()));
        action_with("set-idle-time:0", |_| panic!("invalid choice"), |_| panic!("invalid choice")).unwrap_err();
    }
    #[test]
    fn legacy_display_cycle_still_uses_compositor() {
        let mut requests = Vec::new();
        action_with("cycle-idle-time", |patch| {
            requests.push(patch.cloned());
            Ok(json!({"ok":true,"config":{"idle_seconds":30}}))
        }, |_| panic!("display cycle must not contact powerd")).unwrap();
        assert_eq!(requests, vec![None, Some(json!({"idle_seconds":60}))]);
    }
    #[test]
    fn profile_cycles_are_explicit() {
        assert_eq!(
            next("full", &["off", "daily", "sleep", "activity", "full"]),
            "off"
        );
    }
    #[test]
    fn single_setting_does_not_send_a_stale_full_configuration() {
        let original = json!({"enabled":true,"idle_seconds":30,"sensor_profile":"off",
            "auto_cores":{"enabled":false}});
        let mut updated = original.clone();
        updated["idle_seconds"] = json!(60);
        assert_eq!(
            changed_fields(&original, &updated),
            json!({"idle_seconds":60})
        );
    }
}
