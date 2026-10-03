//! Interactive-display preference shared by powerd and the HWC proxy.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub level: u32,
    pub automatic: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            level: 50,
            automatic: false,
        }
    }
}
impl Config {
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
    fn patches_preserve_manual_level_and_reject_invalid_values() {
        let original = Config {
            level: 37,
            automatic: false,
        };
        assert_eq!(
            original
                .patched(&serde_json::json!({"automatic":true}))
                .unwrap(),
            Config {
                level: 37,
                automatic: true
            }
        );
        for patch in [
            serde_json::json!({"level":0}),
            serde_json::json!({"level":101}),
            serde_json::json!({"automatic":1}),
            serde_json::json!({"typo":true}),
        ] {
            assert!(original.patched(&patch).is_err());
        }
    }
}
