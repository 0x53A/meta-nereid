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
    install_button_callbacks(&app, Arc::new(AtomicI32::new(0)));
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
    app.set_disk_summary("80% used · 3.2/4.0 GiB".into());
    app.set_disk_total("4.0 GiB".into());
    app.set_disk_reserved("234 MiB".into());
    app.set_auto_cores_label("off".into());
    app.set_cpu_cores_active(1);
    app.set_sensor_profile("sleep".into());
    app.set_sleep_reason("Recording is keeping the CPU awake".into());
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
    let tap_checkbox = || {
        draw();
        let position = slint::LogicalPosition::new(360., 216.);
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
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
        app.set_settings_selected_index(health_menu_index(&app));
        gesture(&[200.0]);
        assert!(app.get_show_health_menu());
        draw();
        let position = slint::LogicalPosition::new(344.,158.);
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-recording:on");
        app.set_action_busy(false);
        app.invoke_bottom_pressed();
    }

    // Every added sleep row must keep its touch/action mapping when the
    // optional acoustic slider shifts the list indices.
    for acoustic_on in [false, true] {
        app.set_acoustic_on(acoustic_on);
        for (offset, action) in ["toggle-sleep", "cycle-face-mode", "cycle-ambient-face",
                                "cycle-idle-time"].iter().enumerate() {
            app.set_action_busy(false);
            app.set_settings_selected_index(health_menu_index(&app)+1+offset as i32);
            if offset == 0 { tap_checkbox(); } else { gesture(&[200.0]); }
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), *action);
        }
    }

    for acoustic_on in [false,true] {
        app.set_acoustic_on(acoustic_on);
        app.set_action_busy(false);
        app.set_settings_selected_index(3);
        tap_checkbox();
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),"toggle-auto-cores");
    }

    app.set_action_busy(false);
    app.set_wifi_status("off".into());
    app.set_settings_selected_index(4);
    gesture(&[200.0]); // Wi-Fi text selects the row without changing Wi-Fi.
    assert!(rx.try_recv().is_err());
    tap_checkbox();
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-wifi:on");
    app.set_action_busy(false);
    app.set_wifi_status("off".into());

    app.set_show_health_menu(true);
    app.set_action_busy(false);
    draw();
    let position = slint::LogicalPosition::new(304.,218.);
    app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "cycle-sensor-profile");
    app.invoke_bottom_pressed();

    // Storage opens from the summary row and both entry paths share the subpage.
    app.set_action_busy(false);
    app.set_settings_selected_index(1);
    app.invoke_bottom_pressed();
    assert!(app.get_show_storage_menu());
    app.invoke_top_pressed();
    draw();
    // At this scroll position row 1 is at y=147, not the screen centre.
    for event in [WindowEvent::PointerPressed { position: slint::LogicalPosition::new(208.,147.), button: PointerEventButton::Left },
                  WindowEvent::PointerReleased { position: slint::LogicalPosition::new(208.,147.), button: PointerEventButton::Left }] {
        app.window().dispatch_event(event);
    }
    assert!(app.get_show_storage_menu());
    app.invoke_bottom_pressed();
    assert!(!app.get_show_storage_menu());
                assert!(!app.get_show_health_menu());
    assert_eq!(settings_item_count(&app), health_menu_index(&app) + 6);

    // The final row opens Licenses with either acoustic layout, and crown
    // input scrolls that page without changing the main-list selection.
    app.set_license_entries(ModelRc::new(VecModel::from(vec![
        LicenseEntry { name: "alpha 1.0".into(), license: "MIT".into() },
        LicenseEntry { name: "beta 2.0".into(), license: "GPL-2.0-only".into() },
    ])));
    app.set_sbom_path("Full SBOM at\n/userdata/.hoki/versions/example/sbom.spdx.json".into());
    for acoustic_on in [false, true] {
        app.set_acoustic_on(acoustic_on);
        app.set_settings_selected_index(health_menu_index(&app) + 5);
        app.invoke_bottom_pressed();
        assert!(app.get_show_licenses_menu());
        let selected = app.get_settings_selected_index();
        handle_compositor_message(&app, "scroll:5", &AtomicI32::new(0));
        assert_eq!(app.get_license_selected_index(), 1);
        assert_eq!(app.get_settings_selected_index(), selected);
        app.invoke_bottom_pressed();
        assert!(!app.get_show_licenses_menu());
    }

    // Both side buttons and the shared touch footer return to the same list
    // position, even while a background action is busy.
    for page in 0..6 {
        for busy in [false, true] {
            for input in 0..3 {
                app.set_settings_selected_index(8);
                app.set_action_busy(busy);
                app.set_action_status(if busy { "Working…" } else { "" }.into());
                app.set_show_battery_menu(page == 0);
                app.set_show_power_menu(page == 1);
                app.set_show_usb_menu(page == 2);
                app.set_show_storage_menu(page == 3);
                app.set_show_health_menu(page == 4);
                app.set_show_licenses_menu(page == 5);
                draw();
                match input {
                    0 => app.invoke_top_pressed(),
                    1 => app.invoke_bottom_pressed(),
                    _ => {
                        let position = slint::LogicalPosition::new(208.0, if page == 1 { 302.0 } else { 374.0 });
                        app.window().dispatch_event(WindowEvent::PointerPressed {
                            position, button: PointerEventButton::Left,
                        });
                        app.window().dispatch_event(WindowEvent::PointerReleased {
                            position, button: PointerEventButton::Left,
                        });
                    }
                }
                if page == 1 && input == 0 {
                    assert!(app.get_show_power_menu());
                    if !busy {
                        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "poweroff");
                    } else { assert!(rx.try_recv().is_err()); }
                    app.invoke_bottom_pressed();
                }
                assert!(!app.get_show_battery_menu());
                assert!(!app.get_show_power_menu());
                assert!(!app.get_show_usb_menu());
                assert!(!app.get_show_storage_menu());
                assert!(!app.get_show_health_menu());
                assert!(!app.get_show_licenses_menu());
                assert_eq!(app.get_settings_selected_index(), 8);
            }
        }
    }
    app.set_action_busy(false);
    app.set_action_status("".into());

    // Optional reproducible renderer captures use the actual production view.
    if let Some(directory) = std::env::var_os("HOKI_SETTINGS_TEST_CAPTURES") {
        use std::io::Write;
        std::fs::create_dir_all(&directory).unwrap();
        app.set_battery_details(BatteryDetails {
            power: "0.18 W".into(), current: "45 mA".into(), voltage: "3.98 V".into(),
            ocv: "4.01 V".into(), charge: "220 / 300 mAh".into(), time: "4h 50m".into(),
            temp: "29.5 C".into(), cycles: "83".into(), resistance: "145 mOhm".into(),
        });
        app.set_usb_mode("SSH".into());
        for page in 0..6 {
            app.set_show_battery_menu(page == 0);
            app.set_show_power_menu(page == 1);
            app.set_show_usb_menu(page == 2);
                app.set_show_storage_menu(page == 3);
                app.set_show_health_menu(page == 4);
                app.set_show_licenses_menu(page == 5);
            let pixels = draw();
            let path = std::path::Path::new(&directory).join(format!("subpage-{page}.ppm"));
            let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
            file.write_all(b"P6\n416 416\n255\n").unwrap();
            for pixel in pixels { file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap(); }
        }
        close_subpage(&app);
        app.set_acoustic_available(true);
        for state in ["on", "off", "turning on", "turning off"] {
            app.set_wifi_status(state.into());
            app.set_bt_status(state.into());
            app.set_airplane_status(state.into());
            app.set_acoustic_status(state.into());
            for index in [0, 1, 2, 3, 5, 10] {
                app.set_settings_selected_index(index);
                let pixels = draw();
                let path = std::path::Path::new(&directory)
                    .join(format!("{index}-{}.ppm", state.replace(' ', "-")));
                let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                file.write_all(b"P6\n416 416\n255\n").unwrap();
                for pixel in pixels {
                    file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                }
            }
        }
        app.set_action_busy(false);
        app.set_show_health_menu(true);
        for acoustic_on in [false, true] {
            app.set_acoustic_on(acoustic_on);
            app.set_settings_selected_index(health_menu_index(&app));
            for state in ["on", "off", "starting", "stopping", "error", "unavailable"] {
                app.set_recording_status(state.into());
                app.set_recording_on(state == "on" || state == "stopping");
                let pixels = draw();
                let path = std::path::Path::new(&directory)
                    .join(format!("recording-{acoustic_on}-{state}.ppm"));
                let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                file.write_all(b"P6\n416 416\n255\n").unwrap();
                for pixel in pixels {
                    file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                }
            }
        }
    }
}
