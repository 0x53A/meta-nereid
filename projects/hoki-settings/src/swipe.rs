//! Signed, residual-preserving row stepping, independent of pointer event density.
#[derive(Default)]
pub struct Swipe {
    last_y: f32,
    residual: f32,
}

impl Swipe {
    pub fn begin(&mut self, y: f32) {
        self.last_y = y;
        self.residual = 0.0;
    }

    pub fn move_to(&mut self, y: f32, selected: i32, count: i32) -> i32 {
        self.residual += y - self.last_y;
        self.last_y = y;
        let steps = (self.residual / 50.0).trunc() as i32;
        self.residual -= steps as f32 * 50.0;
        let next = (selected - steps).clamp(0, count - 1);
        if next != selected - steps {
            self.residual = 0.0;
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(points: &[f32]) -> i32 {
        let mut swipe = Swipe::default();
        swipe.begin(200.0);
        points
            .iter()
            .fold(4, |index, &y| swipe.move_to(y, index, 10))
    }

    #[test]
    fn small_upward_moves_do_not_run_away() {
        assert_eq!(drag(&[179.0, 178.0, 177.0, 176.0]), 4);
        assert_eq!(drag(&[221.0, 222.0, 223.0, 224.0]), 4);
    }

    #[test]
    fn directions_and_event_density_agree() {
        assert_eq!(drag(&[79.0]), 6);
        assert_eq!(
            drag(&(79..200).rev().map(|y| y as f32).collect::<Vec<_>>()),
            6
        );
        assert_eq!(drag(&[321.0]), 2);
        assert_eq!(drag(&(201..=321).map(|y| y as f32).collect::<Vec<_>>()), 2);
        assert_eq!(drag(&[140.0, 100.0]), 6); // retain the first step's remainder
        assert_eq!(drag(&[140.0, 200.0]), 4); // reversal
    }

    #[test]
    fn a_new_gesture_discards_old_residual() {
        let mut swipe = Swipe::default();
        swipe.begin(200.0);
        assert_eq!(swipe.move_to(160.0, 4, 10), 4);
        swipe.begin(200.0);
        assert_eq!(swipe.move_to(180.0, 4, 10), 4);
        assert_eq!(swipe.move_to(-500.0, 4, 10), 9);
        assert_eq!(swipe.move_to(-450.0, 9, 10), 8);
    }
}
