//! Display preferences shared by powerd and the HWC proxy.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub level: u32,
    pub automatic: bool,
    pub ambient_automatic: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            level: 50,
            automatic: false,
            ambient_automatic: false,
        }
    }
}
impl Config {
    /// Status is an extensible read protocol: independently deployed consumers
    /// must still apply known preferences when powerd adds another field.
    /// Saved configuration and mutation requests remain strict.
    #[allow(dead_code)]
    pub fn from_status(value: &serde_json::Value) -> Result<Self, String> {
        let mut fields = value.as_object().ok_or("brightness status must be an object")?.clone();
        fields.retain(|key, _| matches!(key.as_str(), "level" | "automatic" | "ambient_automatic"));
        let config: Self = serde_json::from_value(fields.into()).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=100).contains(&self.level) {
            return Err("brightness must be 1–100%".into());
        }
        Ok(())
    }
    pub fn patched(&self, patch: &serde_json::Value) -> Result<Self, String> {
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        for (key, field) in patch
            .as_object()
            .ok_or("brightness patch must be an object")?
        {
            value[key] = field.clone();
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_extensions_do_not_break_consumers_but_writes_stay_strict() {
        let value = serde_json::json!({"level":37,"automatic":false,"future_mode":true});
        let config = Config::from_status(&value).unwrap();
        assert_eq!(config.level, 37);
        assert!(!config.ambient_automatic);
        assert!(serde_json::from_value::<Config>(value).is_err());
        assert!(config.patched(&serde_json::json!({"future_mode":true})).is_err());
        for value in [serde_json::json!({"level":0}), serde_json::json!({"automatic":"yes"}), serde_json::Value::Null] {
            assert!(Config::from_status(&value).is_err());
        }
    }
    #[test]
    fn patches_preserve_manual_level_and_reject_invalid_values() {
        let original = Config {
            level: 37,
            automatic: false,
            ambient_automatic: false,
        };
        assert_eq!(
            original
                .patched(&serde_json::json!({"automatic":true}))
                .unwrap(),
            Config {
                level: 37,
                automatic: true,
                ambient_automatic: false,
            }
        );
        for patch in [
            serde_json::json!({"level":0}),
            serde_json::json!({"level":101}),
            serde_json::json!({"automatic":1}),
            serde_json::json!({"ambient_automatic":1}),
            serde_json::json!({"typo":true}),
        ] {
            assert!(original.patched(&patch).is_err());
        }
    }
    #[test]
    fn legacy_preferences_and_independent_modes() {
        let old: Config = serde_json::from_value(serde_json::json!({"level":37,"automatic":true})).unwrap();
        assert!(!old.ambient_automatic);
        let changed = old.patched(&serde_json::json!({"ambient_automatic":true})).unwrap();
        assert!(changed.automatic && changed.ambient_automatic);
        assert_eq!(changed.level, 37);
        let changed = changed.patched(&serde_json::json!({"automatic":false})).unwrap();
        assert!(!changed.automatic && changed.ambient_automatic);
    }
}
