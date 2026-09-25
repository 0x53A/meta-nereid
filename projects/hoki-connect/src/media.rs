use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Default, Serialize, Deserialize)]
pub struct Media {
    pub players: Vec<String>,
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub volume: Option<i64>,
    #[serde(skip)]
    requested_volume: Option<i64>,
}
fn text(v: &Value) -> String {
    v.as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}
/// Local crown command only; never forward arbitrary methods or unbounded values.
pub fn volume_adjustment(command: &str) -> Option<i64> {
    let n: i64 = command.strip_prefix("volume-adjust:")?.parse().ok()?;
    ((-100..=100).contains(&n) && n != 0).then_some(n)
}
/// Absolute target is bound to the selected player; stale selection is rejected.
pub fn volume_target(command: &str) -> Option<(String, i64)> {
    let body: Value = serde_json::from_str(command.strip_prefix("volume-set:")?).ok()?;
    let player = body["player"].as_str()?;
    let volume = body["volume"].as_i64()?;
    if player.is_empty() || player.len() > 512 || !(0..=100).contains(&volume) {
        return None;
    }
    Some((player.to_owned(), volume))
}
impl Media {
    pub fn sent_volume(&mut self, volume: Option<i64>) {
        if volume.is_some() {
            self.requested_volume = volume;
        }
    }
    pub fn request(&self) -> Value {
        json!({"player":self.player,"requestNowPlaying":true,"requestVolume":true})
    }
    pub fn select_next(&mut self) {
        self.select_direction(false);
    }
    pub fn select_previous(&mut self) {
        self.select_direction(true);
    }
    fn select_direction(&mut self, backwards: bool) {
        let index = self
            .players
            .iter()
            .position(|p| p == &self.player)
            .unwrap_or(0);
        let next = self
            .players
            .get(
                (index
                    + if backwards {
                        self.players.len().saturating_sub(1)
                    } else {
                        1
                    })
                    % self.players.len().max(1),
            )
            .cloned()
            .unwrap_or_default();
        let players = std::mem::take(&mut self.players);
        *self = Self {
            players,
            player: next,
            ..Self::default()
        };
    }
    // Return true when the selection changed and needs a metadata request.
    pub fn update(&mut self, body: &Value) -> bool {
        if let Some(players) = body["playerList"].as_array() {
            self.players = players
                .iter()
                .filter_map(|p| p.as_str())
                .filter(|s| !s.is_empty() && s.len() <= 512)
                .take(32)
                .map(str::to_owned)
                .collect();
            if !self.players.contains(&self.player) {
                let players = std::mem::take(&mut self.players);
                let player = players.first().cloned().unwrap_or_default();
                *self = Self {
                    players,
                    player,
                    ..Self::default()
                };
                return !self.player.is_empty();
            }
        }
        if self.player.is_empty() || body["player"].as_str() != Some(&self.player) {
            return false;
        }
        if body.get("title").is_some() {
            self.title = text(&body["title"]);
        }
        if body.get("artist").is_some() {
            self.artist = text(&body["artist"]);
        }
        for (name, field) in [
            ("isPlaying", &mut self.playing),
            ("canPlay", &mut self.can_play),
            ("canPause", &mut self.can_pause),
            ("canGoNext", &mut self.can_next),
            ("canGoPrevious", &mut self.can_previous),
        ] {
            if let Some(value) = body[name].as_bool() {
                *field = value;
            }
        }
        if let Some(volume) = body["volume"].as_i64() {
            let volume = volume.clamp(0, 100);
            // KDE's MPRIS plugin divides by 100.f, then truncates the double
            // property multiplied by 100. E.g. 51 becomes 50 on readback.
            // Reconcile only the precise echo of our last requested integer.
            if let Some(target) = self.requested_volume {
                let kde_echo = ((target as f32 / 100.0) as f64 * 100.0) as i64;
                if volume == target || volume == kde_echo {
                    self.volume = Some(target);
                } else {
                    self.requested_volume = None;
                    self.volume = Some(volume);
                }
            } else {
                self.volume = Some(volume);
            }
        }
        false
    }
    pub fn command(&self, command: &str) -> Option<Value> {
        if self.player.is_empty() {
            return None;
        }
        let mut body = json!({"player":self.player});
        match command {
            "play-pause"
                if if self.playing {
                    self.can_pause
                } else {
                    self.can_play
                } =>
            {
                body["action"] = json!("PlayPause")
            }
            "next" if self.can_next => body["action"] = json!("Next"),
            "previous" if self.can_previous => body["action"] = json!("Previous"),
            "volume-up" | "volume-down" => {
                body["setVolume"] = json!((self.volume?
                    + if command == "volume-up" { 5 } else { -5 })
                .clamp(0, 100))
            }
            c if volume_adjustment(c).is_some() => {
                body["setVolume"] = json!((self.volume? + volume_adjustment(c)?).clamp(0, 100));
            }
            c if volume_target(c).is_some() => {
                let (player, value) = volume_target(c)?;
                if player != self.player || self.volume.is_none() {
                    return None;
                }
                body["setVolume"] = json!(value);
            }
            _ => return None,
        }
        Some(body)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_target_is_bounded_and_bound_to_player() {
        let mut m = Media::default();
        m.update(&json!({"playerList":["Test"]}));
        m.update(&json!({"player":"Test","volume":50}));
        let command = |p: &str, v: i64| format!("volume-set:{}", json!({"player":p,"volume":v}));
        assert_eq!(m.command(&command("Test", 80)).unwrap()["setVolume"], 80);
        for c in [
            command("Other", 80),
            command("Test", 101),
            command("Test", -1),
            "volume-set:bad".into(),
        ] {
            assert!(m.command(&c).is_none());
        }
    }
    #[test]
    fn kde_float_echo_does_not_stall_fine_steps() {
        let mut m = Media::default();
        m.update(&json!({"playerList":["Test","Other"]}));
        m.update(&json!({"player":"Test","volume":50}));
        for target in [51, 52, 51, 50] {
            m.sent_volume(Some(target));
            let echo = ((target as f32 / 100.0) as f64 * 100.0) as i64;
            m.update(&json!({"player":"Test","volume":echo}));
            assert_eq!(m.volume, Some(target));
            assert_eq!(
                m.command("volume-adjust:1").unwrap()["setVolume"],
                target + 1
            );
        }
        m.update(&json!({"player":"Test","volume":70}));
        assert_eq!(m.volume, Some(70));
        assert_eq!(m.requested_volume, None);
        m.sent_volume(Some(51));
        m.select_next();
        assert_eq!(m.requested_volume, None);
    }
    #[test]
    fn fine_volume_is_bounded_and_keeps_touch_steps() {
        let mut m = Media::default();
        m.update(&json!({"playerList":["Test"]}));
        m.update(&json!({"player":"Test","volume":50}));
        assert_eq!(m.command("volume-adjust:1").unwrap()["setVolume"], 51);
        assert_eq!(m.command("volume-adjust:-1").unwrap()["setVolume"], 49);
        assert_eq!(m.command("volume-up").unwrap()["setVolume"], 55);
        assert_eq!(m.command("volume-adjust:100").unwrap()["setVolume"], 100);
        assert_eq!(m.command("volume-adjust:-100").unwrap()["setVolume"], 0);
        for c in [
            "volume-adjust:101",
            "volume-adjust:-101",
            "volume-adjust:0",
            "volume-adjust:1.5",
            "volume-adjust:abc",
        ] {
            assert!(m.command(c).is_none());
        }
        m.volume = None;
        assert!(m.command("volume-adjust:1").is_none());
    }
    #[test]
    fn partial_updates_selection_and_capability_gates() {
        let mut m = Media::default();
        assert!(m.command("play-pause").is_none());
        assert!(m.update(&json!({"playerList":["One","Two"]})));
        m.update(&json!({"player":"One","title":"Track","canPlay":true,"volume":99}));
        m.update(&json!({"player":"Two","title":"Wrong"}));
        m.update(&json!({"player":"One","artist":"Artist"}));
        assert_eq!(m.title, "Track");
        assert_eq!(m.command("volume-up").unwrap()["setVolume"], 100);
        assert_eq!(m.command("play-pause").unwrap()["action"], "PlayPause");
        assert!(m.command("next").is_none());
        assert!(m.command("Quit").is_none());
        m.select_next();
        assert_eq!(m.player, "Two");
        m.select_previous();
        assert_eq!(m.player, "One");
        m.select_previous();
        assert_eq!(m.player, "Two");
        assert!(m.title.is_empty());
        assert!(m.command("volume-down").is_none());
        m.update(&json!({"playerList":[]}));
        assert!(m.player.is_empty());
    }
}
