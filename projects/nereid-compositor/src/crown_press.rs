//! Gesture state uses monotonic time; repeat events never restart the timer.
use std::time::{Duration, Instant};
const HOLD: Duration = Duration::from_millis(650);
#[derive(Default)]
pub struct CrownPress {
    down: Option<Instant>,
    fired: bool,
    wake_only: bool,
}
impl CrownPress {
    pub fn press(&mut self, now: Instant, wake_only: bool) {
        if self.down.is_none() {
            self.down = Some(now);
            self.fired = false;
            self.wake_only = wake_only;
        }
    }
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.fired
            && self
                .down
                .is_some_and(|t| now.saturating_duration_since(t) >= HOLD)
        {
            self.fired = true;
            true
        } else {
            false
        }
    }
    pub fn remaining_ms(&self, now: Instant) -> Option<u16> {
        self.down.filter(|_| !self.fired).map(|t| {
            HOLD.saturating_sub(now.saturating_duration_since(t))
                .as_millis()
                .max(1) as u16
        })
    }
    pub fn release(&mut self) -> bool {
        let short = self.down.take().is_some() && !self.fired && !self.wake_only;
        self.fired = false;
        self.wake_only = false;
        short
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_press_only_on_release() {
        let mut c = CrownPress::default();
        let t = Instant::now();
        c.press(t, false);
        assert!(!c.tick(t + Duration::from_millis(649)));
        assert!(c.release());
        assert!(!c.release());
    }
    #[test]
    fn hold_consumes_release_and_repeats() {
        let mut c = CrownPress::default();
        let t = Instant::now();
        c.press(t, false);
        c.press(t + Duration::from_millis(500), false);
        assert!(c.tick(t + HOLD));
        assert!(!c.tick(t + HOLD));
        assert!(!c.release());
    }
    #[test]
    fn wake_tap_is_consumed_but_hold_activates() {
        let mut c = CrownPress::default();
        let t = Instant::now();
        c.press(t, true);
        assert!(!c.release());
        c.press(t, true);
        assert!(c.tick(t + HOLD));
        assert!(!c.release());
    }
}
