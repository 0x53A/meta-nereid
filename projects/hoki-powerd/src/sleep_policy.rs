//! Pure everyday sleep policy, independent of sysfs and IPC.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub brightness: crate::brightness_config::Config,
    #[serde(default)]
    pub auto_cores: crate::auto_cores::Config,
    pub enabled: bool,
    pub sensor_profile: String,
    pub max_sleep_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            brightness: Default::default(),
            auto_cores: Default::default(),
            enabled: false,
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
            } else if key == "brightness" {
                serde_json::to_value(self.brightness.patched(field)?).map_err(|e| e.to_string())?
            } else { field.clone() };
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.brightness.validate()?;
        self.auto_cores.validate()?;
        if !matches!(self.sensor_profile.as_str(), "off" | "daily" | "sleep" | "activity" | "full")
            || !(3..=60).contains(&self.max_sleep_seconds) {
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
    pub display: String,
    pub ready: bool,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub struct Sensor {
    pub ready: bool,
    pub deadline: f64,
    pub profile: String,
}
#[derive(Debug, PartialEq)]
pub struct Decision {
    pub sleep_until: Option<f64>,
    pub reason: String,
}

pub fn decide(
    config: &Config,
    owners: &BTreeMap<u64, Owner>,
    now: f64,
    startup_until: f64,
    retry_after: f64,
    power_safe: bool,
) -> Decision {
    let mut result = Decision {
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
    if !ui.ready || ui.display == "interactive" {
        result.reason = "compositor not ready for suspend".into();
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
                    display: "ambient".into(),
                    ready: true,
                    revision: 7,
                }),
                ..Owner::default()
            },
        )]);
        (config, owners)
    }
    fn policy(c: &Config, o: &BTreeMap<u64, Owner>) -> Decision {
        decide(c, o, 100., 0., 0., true)
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
        assert_eq!(d.sleep_until, None);
        o.remove(&2);
        assert_eq!(policy(&c, &o).sleep_until, Some(115.));
    }
    #[test]
    fn stale_ui_and_unacknowledged_handoff_block_sleep() {
        let (c, mut o) = fixture();
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().ready = false;
        assert_eq!(policy(&c, &o).sleep_until, None);
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().ready = true;
        let stale=decide(&c, &o, 110., 0., 0., true);
        assert_eq!(stale.sleep_until, None);
        assert_eq!(stale.reason,"compositor status stale");
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
    fn only_completed_noninteractive_display_permits_sleep() {
        let (c, mut o) = fixture();
        for display in ["ambient", "off"] {
            o.get_mut(&1).unwrap().ui.as_mut().unwrap().display=display.into();
            assert!(policy(&c,&o).sleep_until.is_some());
        }
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().display="interactive".into();
        assert_eq!(policy(&c,&o).sleep_until,None);
        o.get_mut(&1).unwrap().ui.as_mut().unwrap().display="off".into();
        o.get_mut(&1).unwrap().display=true;
        assert_eq!(policy(&c,&o).sleep_until,None);
        o.clear();
        assert_eq!(policy(&c,&o).sleep_until,None);
    }
    #[test]
    fn power_startup_and_backoff_fail_closed() {
        let (c, o) = fixture();
        for (start, retry, power) in [(101., 0., true), (0., 101., true), (0., 0., false)] {
            assert_eq!(
                decide(&c, &o, 100., start, retry, power).sleep_until,
                None
            );
        }
        let mut invalid = c;
        invalid.max_sleep_seconds = 0;
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
        let updated = current.patched(&json!({"max_sleep_seconds":60})).unwrap();
        assert_eq!(updated.sensor_profile, "activity");
        assert!(updated.auto_cores.enabled);
        assert_eq!(updated.max_sleep_seconds, 60);
        assert!(current.patched(&json!({"max_sleep_seconds":0})).is_err());
        assert!(current.patched(&json!({"typo":true})).is_err());
        assert!(current.patched(&json!({"auto_cores":{"typo":true}})).is_err());
        assert_eq!(current.max_sleep_seconds, 15);
    }
}
