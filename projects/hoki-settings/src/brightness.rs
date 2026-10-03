//! Saved preferences from our power coordinator; HWC proxy applies the hardware level.
use crate::sleep_client::Client;
use serde_json::json;
#[derive(Clone, Default)]
pub struct State {
    pub available: bool,
    pub level: i32,
    pub maximum: i32,
    pub automatic: bool,
    pub status: String,
}
pub fn read() -> State {
    if crate::simulated::enabled() {
        return State {
            available: true,
            level: crate::simulated::read("brightness", "50")
                .parse::<i32>()
                .unwrap_or(50)
                .clamp(1, 100),
            maximum: 100,
            automatic: crate::simulated::read("auto-brightness", "off") == "on",
            status: String::new(),
        };
    }
    let read = || -> Result<State, Box<dyn std::error::Error>> {
        let reply = Client::connect()?.request(json!({"command":"status"}))?;
        let value = &reply["config"]["brightness"];
        let level = value["level"]
            .as_i64()
            .filter(|n| (1..=100).contains(n))
            .ok_or("brightness unavailable")?;
        let automatic = value["automatic"]
            .as_bool()
            .ok_or("brightness unavailable")?;
        let runtime = std::fs::read("/run/hoki-hwc-proxy/brightness.json")
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let status = runtime
            .filter(|v| {
                let age = crate::sleep_client::boottime().unwrap_or(f64::INFINITY)
                    - v["at"].as_f64().unwrap_or(0.);
                (0.0..10.0).contains(&age) && v["config"] == *value
            })
            .map(|v| {
                v["status"]
                    .as_str()
                    .unwrap_or("Display status unavailable")
                    .to_string()
            })
            .unwrap_or_else(|| "Waiting for display controller".into());
        Ok(State {
            available: true,
            level: level as i32,
            maximum: 100,
            automatic,
            status,
        })
    };
    read().unwrap_or_default()
}
pub fn action(action: &str) -> Result<String, String> {
    let patch = if let Some(level) = action.strip_prefix("brightness:") {
        let level = level.parse::<i32>().map_err(|e| e.to_string())?;
        if !(1..=100).contains(&level) {
            return Err("Invalid brightness level".into());
        }
        json!({"level":level})
    } else {
        match action {
            "set-auto-brightness:on" => json!({"automatic":true}),
            "set-auto-brightness:off" => json!({"automatic":false}),
            _ => return Err("Unknown brightness action".into()),
        }
    };
    Client::connect()
        .and_then(|mut c| c.request(json!({"command":"configure-brightness","patch":patch})))
        .map_err(|e| e.to_string())?;
    Ok(String::new())
}
