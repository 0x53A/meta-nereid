// Slint maps one Wayland wheel tick to 60 logical pixels. The compositor sends
// positive physical ticks as negative Slint deltas; Settings and Launcher use
// five physical ticks to move one row.
const UNITS_PER_ROW: f32 = 5.0 * 60.0;

#[derive(Default)]
pub struct CrownScroll {
    view: String,
    remainder: f32,
}

impl CrownScroll {
    pub fn step(&mut self, delta: f32, view: &str) -> i32 {
        if self.view != view {
            self.view.clear();
            self.view.push_str(view);
            self.remainder = 0.0;
        }
        if !matches!(view, "shows" | "list" | "queue") || !delta.is_finite() {
            self.remainder = 0.0;
            return 0;
        }
        self.remainder += delta.clamp(-6000.0, 6000.0);
        let rows = -(self.remainder / UNITS_PER_ROW).trunc() as i32;
        self.remainder += rows as f32 * UNITS_PER_ROW;
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_settings_and_launcher_direction_and_rate() {
        let mut crown = CrownScroll::default();
        for _ in 0..4 { assert_eq!(crown.step(-60.0, "list"), 0); }
        assert_eq!(crown.step(-60.0, "list"), 1);
        assert_eq!(crown.step(300.0, "list"), -1);
        assert_eq!(crown.step(-600.0, "list"), 2);
    }

    #[test]
    fn view_change_discards_partial_rotation() {
        let mut crown = CrownScroll::default();
        assert_eq!(crown.step(-240.0, "list"), 0);
        assert_eq!(crown.step(-60.0, "queue"), 0);
        assert_eq!(crown.step(-240.0, "queue"), 1);
        assert_eq!(crown.step(-60.0, "player"), 0);
        assert_eq!(crown.step(-60.0, "queue"), 0);
    }
}
