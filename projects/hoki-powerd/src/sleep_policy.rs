//! Pure everyday sleep policy, independent of sysfs and IPC.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub auto_cores: crate::auto_cores::Config,
    pub enabled: bool,
    pub face_mode: String,
    pub ambient_face: String,
    pub idle_seconds: u64,
    pub sensor_profile: String,
    pub max_sleep_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            auto_cores: Default::default(),
            enabled: false,
            face_mode: "automatic".into(),
            ambient_face: "hoki-digital".into(),
            idle_seconds: 30,
            sensor_profile: "off".into(),
            max_sleep_seconds: 15,
        }
    }
}
impl Config {
    pub fn patched(&self, patch: &serde_json::Value) -> Result<Self, String> {
        let fields = patch.as_object().ok_or("sleep patch must be an object")?;
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        for (key, field) in fields {
            value[key] = if key == "auto_cores" {
                serde_json::to_value(crate::auto_cores::patched(&self.auto_cores, field)?)
                    .map_err(|e| e.to_string())?
            } else { field.clone() };
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.auto_cores.validate()?;
        if !matches!(
            self.face_mode.as_str(),
            "primary" | "secondary" | "automatic"
        ) || !matches!(
            self.sensor_profile.as_str(),
            "off" | "daily" | "sleep" | "activity" | "full"
        ) || !(5..=3600).contains(&self.idle_seconds)
            || !(3..=60).contains(&self.max_sleep_seconds)
            || self.ambient_face.is_empty()
            || self.ambient_face.len() > 64
            || !self
                .ambient_face
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid sleep configuration".into());
        }
        Ok(())
    }
}

#[derive(Default, Clone, Debug)]
pub struct Owner {
    pub cpu: bool,
    pub display: bool,
    pub reason: String,
    pub ui: Option<Ui>,
    pub sensor: Option<Sensor>,
}
#[derive(Clone, Debug)]
pub struct Ui {
    pub at: f64,
    pub idle: f64,
    pub foreground: bool,
    pub display: String,
    pub generation: u64,
    pub manual_off: bool,
    pub handoff_failed: bool,
}
#[derive(Clone, Debug)]
pub struct Sensor {
    pub ready: bool,
    pub deadline: f64,
    pub profile: String,
}
#[derive(Debug, PartialEq)]
pub struct Decision {
    pub display: &'static str,
    pub sleep_until: Option<f64>,
    pub reason: String,
}

pub fn decide(
    config: &Config,
    owners: &BTreeMap<u64, Owner>,
    now: f64,
    generation: u64,
    startup_until: f64,
    retry_after: f64,
    power_safe: bool,
) -> Decision {
    let mut result = Decision {
        display: "interactive",
        sleep_until: None,
        reason: "disabled".into(),
    };
    if !config.enabled {
        return result;
    }
    let ui = owners.values().find_map(|o| o.ui.as_ref());
    let Some(ui) = ui else {
        result.reason = "compositor unavailable".into();
        return result;
    };
    // BOOTTIME freshness: a long suspend requires fresh UI state after resume.
    if now - ui.at > 4.0 {
        result.reason = "compositor status stale".into();
        return result;
    }
    if owners.values().any(|o| o.display) {
        result.reason = "display inhibitor".into();
        return result;
    }
    if ui.handoff_failed {
        result.reason = "display handoff failed; wake or change settings to retry".into();
        return result;
    }
    let idle = ui.idle >= config.idle_seconds as f64;
    if ui.manual_off {
        result.display = "off";
    } else if !ui.foreground
        && (config.face_mode == "secondary" || (config.face_mode == "automatic" && idle))
    {
        result.display = "ambient";
    } else if idle && config.face_mode == "primary" && !ui.foreground {
        result.display = "off";
    } else {
        result.reason = "interactive".into();
        return result;
    }
    if ui.generation != generation || ui.display != result.display {
        result.reason = "display handoff pending".into();
        return result;
    }
    if now < startup_until {
        result.reason = "startup reconciliation".into();
        return result;
    }
    if now < retry_after {
        result.reason = "suspend backoff".into();
        return result;
    }
    if !power_safe {
        result.reason = "external power or unknown power state".into();
        return result;
    }
    if let Some(owner) = owners.values().find(|o| o.cpu) {
        result.reason = format!("CPU inhibitor: {}", owner.reason);
        return result;
    }
    let mut until = now + config.max_sleep_seconds as f64;
    let mut matching_sensor = config.sensor_profile == "off";
    for sensor in owners.values().filter_map(|o| o.sensor.as_ref()) {
        matching_sensor |= sensor.profile == config.sensor_profile;
        if !sensor.ready || sensor.deadline <= now + 2.0 {
            result.reason = "sensor maintenance required".into();
            return result;
        }
        until = until.min(sensor.deadline);
    }
    if !matching_sensor {
        result.reason = "sensor profile not ready".into();
        return result;
    }
    result.reason = "ready".into();
    result.sleep_until = Some(until);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Config, BTreeMap<u64, Owner>) {
        let mut config = Config::default();
        config.enabled = true;
        let owners = BTreeMap::from([(
            1,
            Owner {
                ui: Some(Ui {
                    at: 100.,
                    idle: 40.,
                    foreground: false,
                    display: "ambient".into(),
                    generation: 7,
                    manual_off: false,
                    handoff_failed: false,
                }),
                ..Owner::default()
            },
        )]);
        (config, owners)
    }
    fn policy(c: &Config, o: &BTreeMap<u64, Owner>) -> Decision {
        decide(c, o, 100., 7, 0., 0., true)
    }
    #[test]
    fn playback_keeps_cpu_awake_but_allows_ambient() {
        let (c, mut o) = fixture();
        o.insert(
            2,
            Owner {
                cpu: true,
                reason: "audio".into(),
                ..Owner::default()
            },
        );
        let d = policy(&c, &o);
        assert_eq!(d.display, "ambient");
        assert_eq!(d.sleep_until, None);
        o.remove(&2);
        assert_eq!(policy(&c, &o).sleep_until, Some(115.));
    }
    #[test]
    fn stale_ui_and_unacknowledged_handoff_block_sleep() {
        let (c, mut o) = fixture();
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().generation = 6;
        assert_eq!(policy(&c, &o).sleep_until, None);
        assert_eq!(decide(&c, &o, 110., 7, 0., 0., true).display, "interactive");
    }
    #[test]
    fn sensors_bound_sleep_and_missing_or_unready_profiles_block() {
        let (mut c, mut o) = fixture();
        c.sensor_profile = "sleep".into();
        assert_eq!(policy(&c, &o).sleep_until, None);
        o.insert(
            2,
            Owner {
                sensor: Some(Sensor {
                    ready: true,
                    deadline: 108.,
                    profile: "sleep".into(),
                }),
                ..Owner::default()
            },
        );
        assert_eq!(policy(&c, &o).sleep_until, Some(108.));
        o.get_mut(&2).unwrap().sensor.as_mut().unwrap().deadline = 101.;
        assert_eq!(policy(&c, &o).sleep_until, None);
    }
    #[test]
    fn modes_and_inhibitors_are_independent() {
        let (mut c, mut o) = fixture();
        c.face_mode = "secondary".into();
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().idle = 0.;
        assert_eq!(policy(&c, &o).display, "ambient");
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().foreground = true;
        assert_eq!(policy(&c, &o).display, "interactive");
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().foreground = false;
        c.face_mode = "primary".into();
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().idle = 40.;
        assert_eq!(policy(&c, &o).display, "off");
        assert_eq!(policy(&c, &o).sleep_until, None);
        o.get_mut(&1).unwrap().display = true;
        assert_eq!(policy(&c, &o).display, "interactive");
    }
    #[test]
    fn power_startup_and_backoff_fail_closed() {
        let (c, o) = fixture();
        for (start, retry, power) in [(101., 0., true), (0., 101., true), (0., 0., false)] {
            assert_eq!(
                decide(&c, &o, 100., 7, start, retry, power).sleep_until,
                None
            );
        }
        let mut invalid = c;
        invalid.idle_seconds = 0;
        assert!(invalid.validate().is_err());
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn independent_updates_preserve_each_other_and_validate_before_commit() {
        let current = Config::default().patched(&json!({"sensor_profile":"activity",
            "auto_cores":{"enabled":true}})).unwrap();
        let updated = current.patched(&json!({"idle_seconds":60})).unwrap();
        assert_eq!(updated.sensor_profile, "activity");
        assert!(updated.auto_cores.enabled);
        assert_eq!(updated.idle_seconds, 60);
        assert!(current.patched(&json!({"idle_seconds":0})).is_err());
        assert!(current.patched(&json!({"typo":true})).is_err());
        assert!(current.patched(&json!({"auto_cores":{"typo":true}})).is_err());
        assert_eq!(current.idle_seconds, 30);
    }
}
