//! Display policy belongs to the compositor. Powerd never selects a screen.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub face_mode: String,
    pub ambient_face: String,
    pub idle_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            face_mode: "automatic".into(),
            ambient_face: "hoki-digital".into(),
            idle_seconds: 30,
        }
    }
}
impl Config {
    pub fn patched(&self, patch: &serde_json::Value) -> Result<Self, String> {
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        for (key, field) in patch.as_object().ok_or("display patch must be an object")? {
            value[key] = field.clone();
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.face_mode.as_str(),
            "primary" | "secondary" | "automatic"
        ) || !(5..=3600).contains(&self.idle_seconds)
            || self.ambient_face.is_empty()
            || self.ambient_face.len() > 64
            || !self
                .ambient_face
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err("invalid display configuration".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Interactive,
    Ambient,
    Off,
}
impl Display {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Ambient => "ambient",
            Self::Off => "off",
        }
    }
}
pub struct State {
    pub locked: bool,
    pub watchface: bool,
    pub idle_seconds: f64,
    pub manual_off: bool,
    pub display_inhibited: bool,
    pub handoff_failed: bool,
}
pub fn decide(config: &Config, state: State) -> Display {
    use Display::*;
    if state.display_inhibited {
        return Interactive;
    }
    if state.handoff_failed {
        return Interactive;
    }
    if state.manual_off {
        return Off;
    }
    let idle = state.idle_seconds >= config.idle_seconds as f64;
    // Both PIN entry and the read-only locked face get a full interaction window.
    // No low-power renderer may bypass lock composition.
    if state.locked {
        return if idle { Off } else { Interactive };
    }
    if !state.watchface {
        return Interactive;
    }
    match config.face_mode.as_str() {
        "secondary" => Ambient,
        "automatic" if idle => Ambient,
        "primary" if idle => Off,
        _ => Interactive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State {
            locked: false,
            watchface: true,
            idle_seconds: 0.,
            manual_off: false,
            display_inhibited: false,
            handoff_failed: false,
        }
    }
    #[test]
    fn lock_has_an_interactive_interval_in_every_face_mode() {
        for mode in ["primary", "secondary", "automatic"] {
            let c = Config {
                face_mode: mode.into(),
                ..Config::default()
            };
            for watchface in [false, true] {
                assert_eq!(
                    decide(
                        &c,
                        State {
                            locked: true,
                            watchface,
                            ..state()
                        }
                    ),
                    Display::Interactive
                );
                assert_eq!(
                    decide(
                        &c,
                        State {
                            locked: true,
                            watchface,
                            idle_seconds: 31.,
                            ..state()
                        }
                    ),
                    Display::Off
                );
            }
        }
    }
    #[test]
    fn unlocked_faces_apps_and_constraints() {
        let mut c = Config::default();
        assert_eq!(decide(&c, state()), Display::Interactive);
        assert_eq!(
            decide(
                &c,
                State {
                    idle_seconds: 31.,
                    ..state()
                }
            ),
            Display::Ambient
        );
        c.face_mode = "secondary".into();
        assert_eq!(decide(&c, state()), Display::Ambient);
        assert_eq!(
            decide(
                &c,
                State {
                    watchface: false,
                    ..state()
                }
            ),
            Display::Interactive
        );
        assert_eq!(
            decide(
                &c,
                State {
                    manual_off: true,
                    ..state()
                }
            ),
            Display::Off
        );
        assert_eq!(
            decide(
                &c,
                State {
                    locked: true,
                    manual_off: true,
                    display_inhibited: true,
                    ..state()
                }
            ),
            Display::Interactive
        );
        assert_eq!(
            decide(
                &c,
                State {
                    handoff_failed: true,
                    ..state()
                }
            ),
            Display::Interactive
        );
        c.face_mode = "primary".into();
        assert_eq!(
            decide(
                &c,
                State {
                    idle_seconds: 31.,
                    ..state()
                }
            ),
            Display::Off
        );
    }
    #[test]
    fn patches_preserve_other_fields_and_reject_invalid_values() {
        let c = Config::default();
        let changed = c.patched(&serde_json::json!({"idle_seconds":60})).unwrap();
        assert_eq!(changed.face_mode, c.face_mode);
        assert_eq!(changed.idle_seconds, 60);
        for patch in [
            serde_json::json!({"idle_seconds":0}),
            serde_json::json!({"ambient_face":"../escape"}),
            serde_json::json!({"typo":true}),
        ] {
            assert!(c.patched(&patch).is_err());
        }
    }
}
