//! Completed frames retain identity until pixels change, independently of drawing.
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Frames(Option<Arc<[u8]>>);
impl Frames {
    fn clear(&mut self) {
        self.0 = None;
    }

    fn publish(&mut self, pixels: Vec<u8>) {
        if self.0.as_deref() != Some(pixels.as_slice()) {
            self.0 = Some(pixels.into());
        }
    }
}
static DISPLAY: Mutex<Frames> = Mutex::new(Frames(None));
pub fn publish(pixels: Vec<u8>) {
    DISPLAY.lock().unwrap().publish(pixels);
}
pub fn clear() {
    DISPLAY.lock().unwrap().clear();
}
pub fn snapshot() -> Option<Arc<[u8]>> {
    DISPLAY.lock().unwrap().0.clone()
}

#[derive(Default)]
pub struct ChangedFrame(Option<Arc<[u8]>>);
impl ChangedFrame {
    pub fn take(&mut self, frame: Option<Arc<[u8]>>) -> Option<Arc<[u8]>> {
        let frame = frame?;
        if self
            .0
            .as_ref()
            .is_some_and(|last| Arc::ptr_eq(last, &frame))
        {
            return None;
        }
        self.0 = Some(frame.clone());
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_frames_skip_rendering_and_changes_resume_it() {
        let mut frames = Frames::default();
        let mut ui = ChangedFrame::default();
        frames.publish(vec![1; 32]);
        let first = ui.take(frames.0.clone()).unwrap();
        for _ in 0..600 {
            frames.publish(vec![1; 32]);
            assert!(ui.take(frames.0.clone()).is_none());
        }
        frames.publish(vec![2; 32]);
        assert_eq!(&*ui.take(frames.0.clone()).unwrap(), &[2; 32]);
        assert_eq!(&*first, &[1; 32]);
        assert!(ui.take(frames.0.clone()).is_none());
    }
    #[test]
    fn published_frames_remain_immutable_across_threads() {
        publish(vec![1; 32]);
        let first = snapshot().unwrap();
        std::thread::spawn(|| publish(vec![2; 32])).join().unwrap();
        assert_eq!(&*first, &[1; 32]);
        assert_eq!(&*snapshot().unwrap(), &[2; 32]);
    }
    #[test]
    fn new_session_clears_old_frame_even_when_pixels_repeat() {
        let mut frames = Frames::default();
        let mut ui = ChangedFrame::default();
        frames.publish(vec![7; 32]);
        let old = ui.take(frames.0.clone()).unwrap();
        frames.clear();
        assert!(ui.take(frames.0.clone()).is_none());
        frames.publish(vec![7; 32]);
        let new = ui.take(frames.0.clone()).unwrap();
        assert_eq!(&*new, &*old);
        assert!(!Arc::ptr_eq(&new, &old));
    }
}
