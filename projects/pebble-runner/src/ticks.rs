//! Track calendar changes instead of treating every second as every time unit.
#[derive(Default)]
pub struct TickState(Option<[i32; 6]>);
impl TickState {
    pub const fn new() -> Self {
        Self(None)
    }
    pub fn update(&mut self, tm: &libc::tm, subscribed: u32) -> Option<u32> {
        let now = [
            tm.tm_sec, tm.tm_min, tm.tm_hour, tm.tm_mday, tm.tm_mon, tm.tm_year,
        ];
        let Some(previous) = self.0.replace(now) else {
            return Some(0);
        };
        let changed = now
            .iter()
            .zip(previous)
            .enumerate()
            .fold(0, |mask, (bit, (a, b))| mask | (u32::from(*a != b) << bit));
        (changed & subscribed != 0).then_some(changed)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn minute_face_fires_once_per_minute_and_receives_actual_changes() {
        let mut state = TickState::default();
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        assert_eq!(state.update(&tm, 2), Some(0));
        for second in 1..60 {
            tm.tm_sec = second;
            assert_eq!(state.update(&tm, 2), None);
        }
        tm.tm_sec = 0;
        tm.tm_min = 1;
        assert_eq!(state.update(&tm, 2), Some(3));
        assert_eq!(state.update(&tm, 2), None);
        tm.tm_hour = 2;
        assert_eq!(state.update(&tm, 4), Some(4)); // timezone / clock adjustment
        tm.tm_min = 0;
        tm.tm_hour = 0;
        tm.tm_mday = 1;
        tm.tm_mon = 1;
        tm.tm_year = 1;
        assert_eq!(state.update(&tm, 32), Some(62));
    }
}
