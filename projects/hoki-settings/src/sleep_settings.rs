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
pub fn action(action: &str) -> Result<String, String> {
    let display_action = matches!(
        action,
        "cycle-face-mode" | "cycle-idle-time" | "cycle-ambient-face"
    );
    let mut client = if display_action {
        None
    } else {
        Some(Client::connect().map_err(|e| e.to_string())?)
    };
    let original = if display_action {
        display_request(None)?["config"].clone()
    } else {
        client
            .as_mut()
            .unwrap()
            .request(json!({"command":"status"}))
            .map_err(|e| e.to_string())?["config"]
            .clone()
    };
    let mut config = original.clone();
    if action == "toggle-auto-cores" {
        client.as_mut().unwrap().request(json!({"command":"configure-auto-cores","patch":{"enabled":config["auto_cores"]["enabled"]!=true}})).map_err(|e|e.to_string())?;
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
        display_request(Some(&patch))?;
    } else {
        client
            .as_mut()
            .unwrap()
            .request(json!({"command":"configure-patch","patch":patch}))
            .map_err(|e| e.to_string())?;
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
