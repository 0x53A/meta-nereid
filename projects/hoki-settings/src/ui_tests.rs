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
fn real_pointer_swipes_and_busy_toggles() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    install_swipe_callbacks(&app);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = action_worker::ActionWorker::new(
        move |action| tx.send(action.to_string()).unwrap(),
        |_| {},
    )
    .unwrap();
    install_action_callback(&app, worker, Arc::new(controls::PollVersion::default()));
    app.set_wifi_status("off".into());
    app.set_bt_status("off".into());
    app.set_airplane_status("off".into());
    app.set_battery_level(75);
    app.set_disk_used("3.2 GiB".into());
    app.set_disk_free("585 MiB".into());
    app.set_settings_selected_index(3);
    app.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let draw = || {
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
        app.window().request_redraw();
        renderer.draw_if_needed(|r| {
            r.render(&mut pixels, 416);
        });
        pixels
    };
    let gesture = |points: &[f32]| {
        draw();
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(208.0, 200.0),
            button: PointerEventButton::Left,
        });
        for &y in points {
            app.window().dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(208.0, y),
            });
            draw();
        }
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(208.0, *points.last().unwrap()),
            button: PointerEventButton::Left,
        });
    };
    gesture(&[179.0, 178.0, 177.0, 176.0]);
    assert_eq!(app.get_settings_selected_index(), 3);
    assert!(rx.try_recv().is_err()); // a drag must not toggle the row
    gesture(&(79..200).rev().map(|y| y as f32).collect::<Vec<_>>());
    assert_eq!(app.get_settings_selected_index(), 5);
    gesture(&(201..=321).map(|y| y as f32).collect::<Vec<_>>());
    assert_eq!(app.get_settings_selected_index(), 3);
    assert!(rx.try_recv().is_err());
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(208.0, 0.0),
    });
    assert_eq!(app.get_settings_selected_index(), 3); // hover after release

    app.invoke_settings_action("toggle-wifi".into());
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
        "set-wifi:on"
    );
    assert!(app.get_action_busy());
    assert_eq!(app.get_wifi_status(), "turning on");
    app.invoke_settings_action("toggle-wifi".into());
    app.invoke_settings_action("toggle-bt".into());
    assert!(rx.try_recv().is_err());
    // Emulate confirmed completion, then verify the reverse request.
    app.set_wifi_status("on".into());
    app.set_action_busy(false);
    app.invoke_settings_action("toggle-wifi".into());
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
        "set-wifi:off"
    );
    assert_eq!(app.get_wifi_status(), "turning off");

    // The health row remains tappable at the right index with and without
    // the acoustic volume overlay occupying a row above it.
    app.set_recording_available(true);
    for acoustic_on in [false, true] {
        app.set_action_busy(false);
        app.set_acoustic_available(true);
        app.set_acoustic_on(acoustic_on);
        app.set_recording_on(false);
        app.set_recording_status("off".into());
        app.set_settings_selected_index(health_recording_index(&app));
        gesture(&[200.0]);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-recording:on");
    }

    // Optional reproducible renderer captures use the actual production view.
    if let Some(directory) = std::env::var_os("HOKI_SETTINGS_TEST_CAPTURES") {
        use std::io::Write;
        std::fs::create_dir_all(&directory).unwrap();
        app.set_acoustic_available(true);
        for state in ["on", "off", "turning on", "turning off"] {
            app.set_wifi_status(state.into());
            app.set_bt_status(state.into());
            app.set_airplane_status(state.into());
            app.set_acoustic_status(state.into());
            for index in [0, 2, 5, 10] {
                app.set_settings_selected_index(index);
                let pixels = draw();
                let path = std::path::Path::new(&directory)
                    .join(format!("{index}-{}.ppm", state.replace(' ', "-")));
                let mut file = std::fs::File::create(path).unwrap();
                file.write_all(b"P6\n416 416\n255\n").unwrap();
                for pixel in pixels {
                    file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                }
            }
        }
        app.set_action_busy(false);
        for acoustic_on in [false, true] {
            app.set_acoustic_on(acoustic_on);
            app.set_settings_selected_index(health_recording_index(&app));
            for state in ["on", "off", "starting", "stopping", "error", "unavailable"] {
                app.set_recording_status(state.into());
                let pixels = draw();
                let path = std::path::Path::new(&directory)
                    .join(format!("recording-{acoustic_on}-{state}.ppm"));
                let mut file = std::fs::File::create(path).unwrap();
                file.write_all(b"P6\n416 416\n255\n").unwrap();
                for pixel in pixels {
                    file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                }
            }
        }
    }
}
