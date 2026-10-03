use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use std::rc::Rc;
struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for TestPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
fn snapshot(phase: &str, ready: bool) -> Value {
    json!({"ok":true,"activity":{"state":phase,"active_seconds":1752,"elapsed_seconds":1860,"distance_m":5240,
        "pace_seconds_km":332,"average_pace_seconds_km":334,"gps_lock":false,
        "track":[{"lat":52.0,"lon":13.0,"segment":1},{"lat":52.0005,"lon":13.0002,"segment":1},
        {"lat":52.0005,"lon":13.0004,"segment":2},{"lat":52.0,"lon":13.0005,"segment":2}]},
        "sensors":{"ready":ready,"heart_rate":{"bpm":147}},"error":""})
}
#[test]
fn watch_actions_follow_acknowledged_state_and_preserve_pause_gating() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let w = MainWindow::new().unwrap();
    let (tx, rx) = mpsc::channel();
    w.on_action(move |c| tx.send(c.to_string()).unwrap());
    w.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let draw = |name: &str| {
        slint::platform::update_timers_and_animations();
        w.window().request_redraw();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
        renderer.draw_if_needed(|r| {
            r.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            r.render(&mut pixels, 416);
        });
        if let Some(dir) = std::env::var_os("HOKI_ACTIVITY_TEST_CAPTURES") {
            std::fs::create_dir_all(&dir).unwrap();
            let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::PathBuf::from(dir).join(format!("{name}.png")),
                &bytes,
                416,
                416,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
    };
    let tap = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        w.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        w.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    apply(&w, &snapshot("idle", false));
    draw("idle");
    tap(208., 205.);
    assert_eq!(rx.try_recv().unwrap(), "prepare");
    assert_eq!(w.get_phase(), "idle"); // Never infer success from a tap.
    apply(&w, &snapshot("prepared", false));
    draw("preparing");
    tap(136., 330.);
    assert!(rx.try_recv().is_err());
    apply(&w, &snapshot("prepared", true));
    draw("prepared");
    tap(136., 330.);
    assert_eq!(rx.try_recv().unwrap(), "start");
    assert_eq!(w.get_phase(), "prepared");
    apply(&w, &snapshot("running", true));
    draw("running");
    tap(136., 330.);
    assert_eq!(rx.try_recv().unwrap(), "pause");
    apply(&w, &snapshot("paused", false));
    draw("paused");
    tap(136., 330.);
    assert!(rx.try_recv().is_err());
    apply(&w, &snapshot("paused", true));
    draw("paused-ready");
    tap(136., 330.);
    assert_eq!(rx.try_recv().unwrap(), "resume");
    apply(&w, &snapshot("running", true));
    draw("running-again");
    tap(208., 380.);
    assert!(w.get_track_page());
    draw("track");
    tap(280., 330.);
    assert_eq!(rx.try_recv().unwrap(), "stop");
    apply(&w, &snapshot("stopped", false));
    draw("stopped");
    assert!(w.get_track_page());
    assert_eq!(w.get_active_time(), "29:12");
    assert_eq!(w.get_distance(), "5.24 km");
    tap(208., 380.);
    draw("summary-metrics");
    assert!(!w.get_track_page());
    tap(136., 330.);
    assert_eq!(rx.try_recv().unwrap(), "prepare");
    w.set_connected(false);
    draw("disconnected");
    tap(136., 330.);
    assert!(rx.try_recv().is_err());
}
#[test]
fn route_does_not_join_isolated_segments_or_wrap_across_the_world() {
    let (_, connected) = route::render(
        &json!([{"lat":52.,"lon":13.,"segment":1},{"lat":53.,"lon":14.,"segment":2}]),
    );
    assert!(!connected);
    let (image, connected) = route::render(
        &json!([{"lat":0.,"lon":179.999,"segment":1},{"lat":0.,"lon":-179.999,"segment":1}]),
    );
    assert!(connected);
    assert_eq!((image.size().width, image.size().height), (276, 157));
}
