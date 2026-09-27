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
#[test]
fn companion_navigation_renders_and_works_while_offline() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    let (tx, rx) = mpsc::channel();
    window.on_action(move |c| tx.send(c.to_string()).unwrap());
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let draw = |name: &str| {
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
        window.window().request_redraw();
        renderer.draw_if_needed(|r| {
            r.render(&mut pixels, 416);
        });
        if let Some(dir) = std::env::var_os("HOKI_CONNECT_TEST_CAPTURES") {
            std::fs::create_dir_all(&dir).unwrap();
            let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                PathBuf::from(dir).join(format!("{name}.png")),
                &bytes,
                416,
                416,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
    };
    let tap = |x: f32, y: f32| {
        let position = slint::LogicalPosition::new(x, y);
        window.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window
            .window()
            .dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
    };
    for state in ["devices", "offline", "pair", "pairing", "long"] {
        let mut snapshot = preview(state);
        snapshot["peers"] = json!([{"peer_id":"laptop"},{"peer_id":"phone"}]);
        apply(&window, &snapshot);
        draw(state);
        tap(388.0, 208.0);
        assert_eq!(rx.try_recv().unwrap(), "next-device");
        tap(28.0, 208.0);
        assert_eq!(rx.try_recv().unwrap(), "previous-device");
    }
    window.set_busy(true);
    draw("busy");
    tap(388.0, 208.0);
    assert!(rx.try_recv().is_err());
    window.set_busy(false);
    apply(&window, &preview("device"));
    draw("single-device");
    tap(388.0, 208.0);
    assert!(rx.try_recv().is_err());
    window.set_music_page(true);
    for state in ["devices", "offline", "pair", "pairing", "empty", "long"] {
        apply(&window, &preview(state));
        draw(&format!("music-{state}"));
    }
    let command: Value = serde_json::from_str(&addressed("phone", "ping")).unwrap();
    assert_eq!(command, json!({"peer_id":"phone","command":"ping"}));
}
