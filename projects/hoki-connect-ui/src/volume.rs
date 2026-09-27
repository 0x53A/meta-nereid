//! One replaceable target, independent of the action busy gate.
use std::time::{Duration, Instant};
#[derive(Default)]
pub struct Volume {
    target: Option<(String, String, i32, Instant)>,
    pending: bool,
}
impl Volume {
    pub fn clear(&mut self) {
        self.target = None;
        self.pending = false;
    }
    pub fn set(&mut self, peer: String, player: String, value: i32) {
        self.target = Some((peer, player, value.clamp(0, 100), Instant::now()));
        self.pending = true;
    }
    pub fn take(&mut self) -> Option<(String, String, i32)> {
        if !self.pending {
            return None;
        }
        self.pending = false;
        self.target
            .as_ref()
            .map(|(peer, p, v, _)| (peer.clone(), p.clone(), *v))
    }
    pub fn displayed(&mut self, peer: &str, player: &str, reported: i32, online: bool) -> i32 {
        if let Some((device, p, v, t)) = &self.target {
            if online
                && device == peer
                && p == player
                && (self.pending || t.elapsed() < Duration::from_secs(2))
            {
                return *v;
            }
        }
        self.clear();
        reported
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalesces_and_protects_newer_intent_from_old_echoes() {
        let mut v = Volume::default();
        for n in 51..=80 {
            v.set("laptop".into(), "Player".into(), n);
        }
        assert_eq!(v.take(), Some(("laptop".into(), "Player".into(), 80)));
        assert_eq!(v.take(), None);
        assert_eq!(v.displayed("laptop", "Player", 51, true), 80);
        v.set("laptop".into(), "Player".into(), 79);
        assert_eq!(v.displayed("laptop", "Player", 80, true), 79);
        assert_eq!(v.take(), Some(("laptop".into(), "Player".into(), 79)));
        assert_eq!(v.displayed("laptop", "Other", 20, true), 20);
        assert_eq!(v.take(), None);
        v.set("laptop".into(), "Other".into(), 40);
        assert_eq!(v.displayed("laptop", "Other", 20, false), 20);
        assert_eq!(v.take(), None);
    }
    #[test]
    fn same_player_on_another_companion_clears_pending_volume() {
        let mut v = Volume::default();
        v.set("laptop".into(), "Spotify".into(), 80);
        assert_eq!(v.displayed("phone", "Spotify", 25, true), 25);
        assert_eq!(v.take(), None);
    }
    #[test]
    fn stops_hiding_peer_state_after_settling() {
        let mut v = Volume {
            target: Some((
                "laptop".into(),
                "Player".into(),
                60,
                Instant::now() - Duration::from_secs(3),
            )),
            pending: false,
        };
        assert_eq!(v.displayed("laptop", "Player", 30, true), 30);
    }
}
