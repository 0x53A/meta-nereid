//! Normalize crown/desktop wheel movement without accumulating delayed actions.
// One raw tick changes volume by 1%, chosen after on-watch use.
const RAW_TICKS_PER_PERCENT: f32 = 1.0;
const UNITS_PER_PERCENT: f32 = 60.0 * RAW_TICKS_PER_PERCENT;
#[derive(Default)]
pub struct Crown {
    remainder: f32,
}
impl Crown {
    pub fn reset(&mut self) {
        self.remainder = 0.0;
    }
    pub fn step(&mut self, delta: f32, enabled: bool, volume: i32) -> Option<i32> {
        if !enabled || !delta.is_finite() || !(0..=100).contains(&volume) {
            self.reset();
            return None;
        }
        // Slint maps one raw wheel tick to 60 logical pixels. Preserve batched
        // ticks in a single command; each raw tick changes volume by 1%.
        self.remainder += delta.clamp(-6000.0, 6000.0);
        let steps = -(self.remainder / UNITS_PER_PERCENT).trunc() as i32;
        self.remainder %= UNITS_PER_PERCENT;
        let change = (volume + steps).clamp(0, 100) - volume;
        if change != 0 {
            Some(change)
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_percent_per_tick_including_batches_and_partial_steps() {
        let mut c = Crown::default();
        assert_eq!(c.step(-60.0, true, 50), Some(1));
        assert_eq!(c.step(60.0, true, 50), Some(-1));
        assert_eq!(c.step(-300.0, true, 50), Some(5));
        assert_eq!(c.step(-30.0, true, 50), None);
        assert_eq!(c.step(-30.0, true, 50), Some(1));
    }
    #[test]
    fn bounds_disabled_and_reset_discard_remainders() {
        let mut c = Crown::default();
        assert_eq!(c.step(-60.0, true, 100), None);
        assert_eq!(c.step(60.0, true, 0), None);
        assert_eq!(c.step(-300.0, true, 99), Some(1));
        c.step(-30.0, true, 50);
        c.step(-30.0, false, 50);
        assert_eq!(c.step(-30.0, true, 50), None);
        c.reset();
        assert_eq!(c.step(-30.0, true, 50), None);
        assert_eq!(c.step(f32::NAN, true, 50), None);
        assert_eq!(c.step(-60.0, true, -1), None);
    }
}
