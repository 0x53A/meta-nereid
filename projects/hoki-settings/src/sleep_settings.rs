use crate::sleep_client::Client;
use serde_json::{json, Value};
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
    match result {
        Ok(v) => State {
            config: v["config"].clone(),
            reason: v["reason"].as_str().unwrap_or("unknown").into(),
        },
        Err(_) => State {
            config: Value::Null,
            reason: "unavailable".into(),
        },
    }
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
    let mut client = Client::connect().map_err(|e| e.to_string())?;
    if action.starts_with("set-") {
        let patch = choice_patch(action)?;
        client.request(json!({"command":"configure-patch", "patch":patch})).map_err(|e|e.to_string())?;
        return Ok(String::new());
    }
    let status = client
        .request(json!({"command":"status"}))
        .map_err(|e| e.to_string())?;
    let original = status["config"].clone();
    let mut config = original.clone();
    if action == "toggle-auto-cores" {
        client.request(json!({"command":"configure-auto-cores","patch":{"enabled":config["auto_cores"]["enabled"]!=true}})).map_err(|e|e.to_string())?;
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
    client
        .request(json!({"command":"configure-patch","patch":changed_fields(&original, &config)}))
        .map_err(|e| e.to_string())?;
    Ok(String::new())
}
fn changed_fields(original: &Value, updated: &Value) -> Value {
    Value::Object(updated.as_object().into_iter().flatten()
        .filter(|(key, value)| original[*key] != **value)
        .map(|(key, value)| (key.clone(), value.clone())).collect())
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
        assert_eq!(changed_fields(&original, &updated), json!({"idle_seconds":60}));
    }

}
