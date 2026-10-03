use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use std::rc::Rc;
struct TestPlatform(Rc<MinimalSoftwareWindow>);

#[test]
fn companion_battery_is_visible_only_while_paired_and_connected() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    let mut snapshot = preview("connected");
    snapshot["battery"] = json!({"currentCharge":67,"isCharging":true});
    apply(&window, &snapshot);
    assert_eq!(window.get_peer_battery(), "67% · Charging");
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
    renderer.draw_if_needed(|r| {
        r.render(&mut pixels, 416);
    });
    if let Some(dir) = std::env::var_os("HOKI_CONNECT_TEST_CAPTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            PathBuf::from(dir).join("companion-battery.png"),
            &bytes,
            416,
            416,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
    snapshot["status"]["state"] = json!("disconnected");
    apply(&window, &snapshot);
    assert!(window.get_peer_battery().is_empty());
    snapshot["status"]["state"] = json!("connected");
    snapshot["battery"]["currentCharge"] = json!(-1);
    apply(&window, &snapshot);
    assert!(window.get_peer_battery().is_empty());
    snapshot["battery"]["currentCharge"] = json!(50);
    snapshot["status"]["paired"] = json!(false);
    apply(&window, &snapshot);
    assert!(window.get_peer_battery().is_empty());
}

fn pairing_notice_preserves_code(incoming: bool) {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    let mut snapshot = preview("pairing");
    if incoming {
        snapshot["status"]["pairing_token"] = json!("0123456789abcdef0123456789abcdef");
    }
    apply(&window, &snapshot);
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
    renderer.draw_if_needed(|r| {
        r.render(&mut pixels, 416);
    });
    let region = |pixels: &[slint::Rgb8Pixel]| -> Vec<(u8, u8, u8)> {
        (166..274)
            .flat_map(|y| {
                (58..358).map(move |x| {
                    let p = pixels[y * 416 + x];
                    (p.r, p.g, p.b)
                })
            })
            .collect()
    };
    let before = region(&pixels);
    assert!(
        before
            .iter()
            .filter(|&&(r, g, b)| r > 100 && g > 100 && b > 100)
            .count()
            > 200
    );
    window.set_feedback("A notification while comparing codes".into());
    window.window().request_redraw();
    renderer.draw_if_needed(|r| {
        r.render(&mut pixels, 416);
    });
    assert_eq!(
        before,
        region(&pixels),
        "notice must not obscure device, code or guidance"
    );
    if let Some(dir) = std::env::var_os("HOKI_CONNECT_TEST_CAPTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            PathBuf::from(dir).join(if incoming {
                "incoming-notice.png"
            } else {
                "outgoing-notice.png"
            }),
            &bytes,
            416,
            416,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
}
#[test]
fn outgoing_pairing_notice_keeps_code_visible() {
    pairing_notice_preserves_code(false);
}
#[test]
fn incoming_pairing_notice_keeps_code_visible() {
    pairing_notice_preserves_code(true);
}
impl slint::platform::Platform for TestPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
#[test]
fn first_launch_shows_only_add_device() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    apply(&window, &json!({"peers":[]}));
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
    renderer.draw_if_needed(|r| {
        r.render(&mut pixels, 416);
    });
    assert!(window.get_add_entry());
    assert_eq!(window.get_peer_count(), 0);
    if let Some(dir) = std::env::var_os("HOKI_CONNECT_TEST_CAPTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            PathBuf::from(dir).join("first-launch.png"),
            &bytes,
            416,
            416,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
}
#[test]
fn companion_navigation_renders_and_works_while_offline() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    let (tx, rx) = mpsc::channel();
    window.on_action(move |c| tx.send(c.to_string()).unwrap());
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let pixels = std::cell::RefCell::new(vec![slint::Rgb8Pixel::default(); 416 * 416]);
    let draw = |name: &str| {
        slint::platform::update_timers_and_animations();
        let mut pixels = pixels.borrow_mut();
        window.window().request_redraw();
        renderer.draw_if_needed(|r| {
            // Captures must contain a complete frame, including unchanged items.
            // Switching modes also invalidates the partial-rendering cache.
            r.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
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
        snapshot["peers"] =
            json!([{"peer_id":"laptop","trusted":true},{"peer_id":"phone","trusted":true}]);
        apply(&window, &snapshot);
        draw(state);
    }
    apply(&window, &preview("devices"));
    tap(388.0, 208.0);
    assert!(window.get_add_entry());
    assert!(rx.try_recv().is_err());
    apply(&window, &preview("devices"));
    assert!(
        window.get_add_entry(),
        "polling must preserve the dummy entry"
    );
    draw("add-entry");
    tap(28.0, 208.0);
    assert!(!window.get_add_entry());
    assert_eq!(rx.try_recv().unwrap(), "select:phone");
    tap(28.0, 208.0);
    assert_eq!(rx.try_recv().unwrap(), "select:laptop");
    let mut laptop = preview("device");
    laptop["peers"] = preview("devices")["peers"].clone();
    apply(&window, &laptop);
    tap(28.0, 208.0);
    assert!(window.get_add_entry());
    tap(388.0, 208.0);
    assert_eq!(rx.try_recv().unwrap(), "select:laptop");
    window.set_busy(true);
    draw("busy");
    tap(388.0, 208.0);
    assert!(rx.try_recv().is_err());
    window.set_busy(false);
    apply(&window, &json!({"peers":[]}));
    assert!(window.get_add_entry());
    assert_eq!(window.get_peer_count(), 0);
    draw("no-paired-devices");
    tap(388.0, 208.0);
    assert!(rx.try_recv().is_err());
    apply(
        &window,
        &json!({"selected_peer":"unpaired", "peers":[{"peer_id":"unpaired","trusted":false}]}),
    );
    assert!(!window.get_add_entry());
    assert_eq!(window.get_peer_count(), 1);
    // New enrollment must still reach Pair device, including the first device.
    let mut enrolled = json!({"selected_peer":"unpaired", "peers":[{"peer_id":"unpaired","trusted":false}], "discovery":{"enrolled_peer":"unpaired"}});
    window.set_setup_page(true);
    apply(&window, &enrolled);
    assert!(!window.get_add_entry());
    assert!(!window.get_setup_page());
    window.set_music_page(true);
    apply(&window, &enrolled);
    assert!(
        window.get_music_page(),
        "an enrollment result is consumed only once"
    );
    enrolled["peers"][0]["trusted"] = json!(true);
    apply(&window, &enrolled);
    window.set_music_page(false);
    apply(&window, &preview("device"));
    draw("single-device");
    tap(388.0, 208.0);
    assert!(window.get_add_entry());
    assert!(rx.try_recv().is_err());
    window.set_busy(true);
    tap(208.0, 205.0);
    assert!(!window.get_setup_page());
    assert!(rx.try_recv().is_err());
    window.set_busy(false);
    for (x, y) in [
        (208.0, 130.0),
        (208.0, 205.0),
        (80.0, 290.0),
        (208.0, 310.0),
    ] {
        tap(x, y);
        assert!(window.get_setup_page());
        assert_eq!(rx.try_recv().unwrap(), "discover");
        window.set_setup_page(false);
    }
    tap(208.0, 205.0);
    assert!(window.get_setup_page());
    assert_eq!(rx.try_recv().unwrap(), "discover");
    let mut searching = preview("device");
    searching["discovery"] = json!({"scanning":true,"candidates":[]});
    apply(&window, &searching);
    draw("discovery-searching");
    tap(208.0, 310.0);
    assert!(rx.try_recv().is_err());
    searching["discovery"]["candidates"] = json!([
        {"peer_id":"pixel","name":"Pixel 10 Pro","token":"session-one","verification_key":"A1B2C3D4"},
        {"peer_id":"tablet","name":"A tablet with an unusually long device name","token":"","verification_key":""}
    ]);
    apply(&window, &searching);
    draw("discovery-incoming-pair");
    tap(208.0, 310.0);
    assert_eq!(rx.try_recv().unwrap(), "approve:session-one");
    tap(148.0, 371.0);
    assert_eq!(rx.try_recv().unwrap(), "reject:session-one");
    tap(388.0, 208.0);
    assert_eq!(window.get_candidate_index(), 1);
    draw("discovery-long-name");
    tap(208.0, 310.0);
    assert_eq!(rx.try_recv().unwrap(), "enroll:tablet");
    searching["discovery"]["enrolling"] = json!(true);
    apply(&window, &searching);
    draw("discovery-enrolling");
    tap(208.0, 310.0);
    assert!(rx.try_recv().is_err());
    searching["discovery"] =
        json!({"scanning":false,"candidates":[],"error":"Pairing request expired"});
    apply(&window, &searching);
    draw("discovery-expired");
    tap(208.0, 310.0);
    assert!(rx.try_recv().is_err());
    tap(268.0, 371.0);
    assert_eq!(rx.try_recv().unwrap(), "discover");
    tap(148.0, 371.0);
    assert!(!window.get_setup_page());
    window.set_music_page(true);
    for state in ["devices", "offline", "pair", "pairing", "empty", "long"] {
        apply(&window, &preview(state));
        draw(&format!("music-{state}"));
    }
    let command: Value = serde_json::from_str(&addressed("phone", "ping")).unwrap();
    assert_eq!(command, json!({"peer_id":"phone","command":"ping"}));
}
