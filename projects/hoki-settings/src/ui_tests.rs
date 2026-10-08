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
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    install_swipe_callbacks(&app);
    install_network_callback(&app);
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
    let framebuffer = std::cell::RefCell::new(vec![slint::Rgb8Pixel::default(); 416 * 416]);
    let draw = || {
        slint::platform::update_timers_and_animations();
        let mut pixels = framebuffer.borrow_mut();
        app.window().request_redraw();
        renderer.draw_if_needed(|r| {
            r.render(&mut pixels, 416);
        });
        pixels.clone()
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
        let position = slint::LogicalPosition::new(208.,120.);
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        assert!(rx.try_recv().is_err()); // retired manual recording control
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
            if offset == 0 {
                assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), *action);
            } else if offset != 2 { // ambient faces depend on installed assets
                assert!(app.get_show_choice_menu());
                assert!(rx.try_recv().is_err()); // opening never changes a setting
                app.invoke_top_pressed();
                assert!(!app.get_show_choice_menu());
            }
            app.set_action_status("".into());
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
    assert!(app.get_show_choice_menu());
    assert!(rx.try_recv().is_err());
    app.invoke_top_pressed();
    assert!(app.get_show_health_menu());
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
    assert_eq!(settings_item_count(&app), health_menu_index(&app) + 10);

    // The final row opens Licenses with either acoustic layout, and crown
    // input scrolls that page without changing the main-list selection.
    app.set_license_entries(ModelRc::new(VecModel::from(vec![
        LicenseEntry { name: "alpha 1.0".into(), license: "MIT".into() },
        LicenseEntry { name: "beta 2.0".into(), license: "GPL-2.0-only".into() },
    ])));
    app.set_sbom_path("Full SBOM at\n/userdata/.hoki/versions/example/sbom.spdx.json".into());
    for acoustic_on in [false, true] {
        app.set_acoustic_on(acoustic_on);
        app.set_settings_selected_index(health_menu_index(&app) + 9);
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
    for page in 0..5 {
        for busy in [false, true] {
            for input in 0..3 {
                app.set_settings_selected_index(8);
                app.set_action_busy(busy);
                app.set_action_status(if busy { "Working…" } else { "" }.into());
                app.set_show_battery_menu(page == 0);
                app.set_show_power_menu(page == 1);
                app.set_show_storage_menu(page == 2);
                app.set_show_health_menu(page == 3);
                app.set_show_licenses_menu(page == 4);
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
                assert!(!app.get_show_storage_menu());
                assert!(!app.get_show_health_menu());
                assert!(!app.get_show_licenses_menu());
                assert_eq!(app.get_settings_selected_index(), 8);
            }
        }
    }
    app.set_action_busy(false);
    app.set_action_status("".into());

    // Brightness navigation, real dragging, disabled auto/manual interaction.
    app.set_settings_selected_index(health_menu_index(&app) + 5);
    app.invoke_bottom_pressed();
    assert!(app.get_show_brightness_menu());
    app.set_brightness_available(true);
    app.set_brightness_level(50);
    let touch = |kind: u8, x: f32, y: f32| {
        let position = slint::LogicalPosition::new(x, y);
        app.window().dispatch_event(match kind {
            0 => WindowEvent::PointerPressed { position, button:PointerEventButton::Left },
            1 => WindowEvent::PointerMoved { position },
            _ => WindowEvent::PointerReleased { position, button:PointerEventButton::Left },
        });
    };
    draw();
    touch(0, 70., 144.);
    touch(1, 346., 144.);
    assert_eq!(app.get_brightness_level(), 100);
    assert!(app.get_brightness_dragging());
    touch(2, 346., 144.);
    assert!(!app.get_brightness_dragging());
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "brightness:100");
    app.set_action_busy(false);
    app.set_action_status("".into());
    draw();
    touch(0, 330., 208.); touch(2, 330., 208.);
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-auto-brightness:on");
    app.set_action_busy(false);
    app.set_auto_brightness(true);
    app.set_action_status("".into());
    draw();
    touch(0, 70.,144.); touch(2,70.,144.);
    assert_eq!(app.get_brightness_level(),100);
    assert!(rx.try_recv().is_err());
    // Independent low-power checkbox; labels do not toggle either preference.
    touch(0,100.,266.); touch(2,100.,266.);
    assert!(rx.try_recv().is_err());
    touch(0,330.,266.); touch(2,330.,266.);
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-ambient-auto-brightness:on");
    assert!(app.get_auto_brightness());
    app.set_action_busy(false);
    app.set_action_status("".into());
    app.set_ambient_auto_brightness(true);
    draw();
    touch(0,330.,266.); touch(2,330.,266.);
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-ambient-auto-brightness:off");
    app.set_action_busy(false);
    app.set_action_status("".into());
    app.set_brightness_available(false);
    draw();
    touch(0,330.,208.); touch(2,330.,208.);
    touch(0,330.,266.); touch(2,330.,266.);
    assert!(rx.try_recv().is_err());
    app.invoke_bottom_pressed();
    assert!(!app.get_show_brightness_menu());

    apply_network(&app, &network::Snapshot {
        available:true, wifi_powered:true, status:"ready".into(),
        networks:vec![network::Network {
            path:"/net/connman/service/saved_one".into(), name:"A long saved Wi-Fi network name".into(),
            state:"ready".into(), connected:true, strength:Some(74),
            details:vec![("State".into(),"ready".into()),("IPv4".into(),"192.0.2.10".into())],
        }],
        diagnostics:vec![("ConnMan".into(),"ready".into()),("DNS".into(),"192.0.2.1".into()),("Interface".into(),"wlan0".into())],
    });
    app.set_settings_selected_index(6);
    app.invoke_bottom_pressed();
    assert!(app.get_show_network_menu());
    draw();
    touch(0,120.,128.); touch(2,120.,128.);
    assert_eq!(app.get_network_page(),1);
    assert_eq!(app.get_network_selected_path(),"/net/connman/service/saved_one");
    draw();
    touch(0,208.,320.); touch(2,208.,320.);
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),"network-disconnect:/net/connman/service/saved_one");
    app.set_action_busy(false); app.set_action_status("".into());
    app.invoke_bottom_pressed();
    assert!(app.get_show_network_menu()); assert_eq!(app.get_network_page(),0);
    draw();
    touch(0,282.,316.); touch(2,282.,316.);
    assert_eq!(app.get_network_page(),2);
    handle_compositor_message(&app,"scroll:5",&AtomicI32::new(0));
    assert_eq!(app.get_network_diagnostic_index(),1);
    app.invoke_bottom_pressed(); app.invoke_bottom_pressed();
    assert!(!app.get_show_network_menu());

    // Bluetooth uses three explicit values. Top and touch Back cancel; crown
    // changes only selection; bottom commits. Underlying list stays selected.
    app.set_bt_status("BLE + Classic".into());
    app.set_settings_selected_index(5);
    gesture(&[200.]);
    assert!(app.get_show_choice_menu());
    assert_eq!(app.get_choice_index(), 2);
    assert_eq!(app.get_choices().row_count(), 3);
    assert!(rx.try_recv().is_err());
    handle_compositor_message(&app, "scroll:-5", &AtomicI32::new(0));
    assert_eq!(app.get_choice_index(), 1);
    assert_eq!(app.get_settings_selected_index(), 5);
    app.invoke_top_pressed();
    assert!(!app.get_show_choice_menu());
    assert!(rx.try_recv().is_err());
    app.invoke_bottom_pressed();
    draw(); touch(0,208.,374.); touch(2,208.,374.);
    assert!(!app.get_show_choice_menu());
    assert!(rx.try_recv().is_err());
    for (y, action) in [(113., "set-bt-mode:off"), (162., "set-bt-mode:le"), (211., "set-bt-mode:dual")] {
        app.set_action_busy(false);
        app.invoke_bottom_pressed();
        draw(); touch(0,208.,y); touch(2,208.,y);
        assert!(!app.get_show_choice_menu());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), action);
    }
    app.set_action_busy(false);
    app.invoke_bottom_pressed();
    handle_compositor_message(&app, "scroll:-5", &AtomicI32::new(0));
    app.invoke_bottom_pressed();
    assert!(!app.get_show_choice_menu());
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-bt-mode:le");
    app.set_action_busy(false);
    show_choices(&app, "Long choice list", "one", &[
        ("one", "one"), ("two", "two"), ("three", "three"), ("four", "four"),
        ("five", "five"), ("six", "six"), ("seven", "seven")]);
    gesture(&(79..200).rev().map(|y| y as f32).collect::<Vec<_>>());
    assert!(app.get_choice_index() > 0);
    assert!(rx.try_recv().is_err());
    app.invoke_top_pressed();
    assert!(!app.get_show_choice_menu());
    app.set_action_busy(false);
    app.set_settings_selected_index(8);
    app.invoke_bottom_pressed();
    assert_eq!(app.get_choice_title(), "USB");
    assert_eq!(app.get_choices().row_data(1).unwrap().label, "Network + ADB");
    app.invoke_top_pressed();

    // Network's Turn on is absolute even if the main-list radio snapshot is stale.
    app.set_show_network_menu(true);
    app.set_network_wifi_powered(false);
    app.set_wifi_status("on".into());
    draw(); touch(0,130.,316.); touch(2,130.,316.);
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), "set-wifi:on");
    app.set_action_busy(false); app.invoke_top_pressed();

    // PIN Management launches through the existing
    // compositor role message, and is reachable through touch and crown.
    app.set_acoustic_available(false);
    app.set_acoustic_on(false);
    let pin_setup_index = health_menu_index(&app) + 6;
    assert_eq!(settings_item_count(&app), pin_setup_index + 4);
    let setup_actions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_setup_actions = setup_actions.clone();
    app.on_settings_action(move |action| {
        seen_setup_actions.lock().unwrap().push(action.to_string());
    });
    app.set_settings_selected_index(pin_setup_index);
    gesture(&[200.0]);
    assert_eq!(*setup_actions.lock().unwrap(), vec!["manage-pin"]);
    assert_eq!(
        pin_management_launch_message(),
        r#"launch-argv:["/usr/lib/hoki-lockscreen","--manage-pin"]"#
    );
    app.invoke_bottom_pressed();
    assert_eq!(setup_actions.lock().unwrap().last().unwrap(), "manage-pin");

    // Lock now follows PIN Management and supports both touch and crown action.
    app.set_settings_selected_index(pin_setup_index + 1);
    gesture(&[200.0]);
    assert_eq!(setup_actions.lock().unwrap().last().unwrap(), "lock-now");
    app.invoke_bottom_pressed();
    assert_eq!(setup_actions.lock().unwrap().iter().filter(|s| *s == "lock-now").count(), 2);

    // Optional reproducible renderer captures use the actual production view.
    if let Some(directory) = std::env::var_os("HOKI_SETTINGS_TEST_CAPTURES") {
        use std::io::Write;
        std::fs::create_dir_all(&directory).unwrap();
        app.set_battery_details(BatteryDetails {
            power: "0.18 W".into(), current: "45 mA".into(), voltage: "3.98 V".into(),
            ocv: "4.01 V".into(), charge: "220 / 300 mAh".into(), time: "4h 50m".into(),
            temp: "29.5 C".into(), cycles: "83".into(), resistance: "145 mOhm".into(),
        });
        app.set_usb_mode("Network".into());
        app.set_network_wifi_powered(true);
        for page in 0..7 {
            app.set_show_network_menu(page >= 5);
            app.set_network_page(if page == 6 { 1 } else { 0 });
            app.set_show_battery_menu(page == 0);
            app.set_show_power_menu(page == 1);
                app.set_show_storage_menu(page == 2);
                app.set_show_health_menu(page == 3);
                app.set_show_licenses_menu(page == 4);
            let pixels = draw();
            let path = std::path::Path::new(&directory).join(format!("subpage-{page}.ppm"));
            let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
            file.write_all(b"P6\n416 416\n255\n").unwrap();
            for pixel in pixels { file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap(); }
        }
        close_subpage(&app);
        close_subpage(&app);
        app.set_acoustic_available(true);
        for state in ["on", "off", "turning on", "turning off"] {
            app.set_wifi_status(state.into());
            app.set_bt_status("BLE + Classic".into());
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

/// Run each preview in a fresh process to avoid glyph-cache artifacts from
/// cycling many unrelated pages through the test platform's single window.
#[test]
#[ignore = "set HOKI_BRIGHTNESS_PREVIEW_STATE and HOKI_SETTINGS_TEST_CAPTURES"]
fn brightness_preview() {
    use std::io::Write;
    let state = std::env::var("HOKI_BRIGHTNESS_PREVIEW_STATE").unwrap();
    let directory = std::env::var_os("HOKI_SETTINGS_TEST_CAPTURES").unwrap();
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    app.set_show_brightness_menu(true);
    app.set_brightness_available(state != "unavailable");
    app.set_brightness_level(50);
    app.set_auto_brightness(state == "auto" || state == "sensor-unavailable");
    app.set_ambient_auto_brightness(state == "ambient-auto");
    app.set_brightness_status(if state == "sensor-unavailable" {
        "Light sensor unavailable; using manual level"
    } else { "" }.into());
    app.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416,416));
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![slint::Rgb8Pixel::default();416*416];
    for _ in 0..3 {
        app.window().request_redraw();
        slint::platform::update_timers_and_animations();
        renderer.draw_if_needed(|r| { r.render(&mut pixels,416); });
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    std::fs::create_dir_all(&directory).unwrap();
    let path = std::path::Path::new(&directory).join(format!("brightness-{state}.ppm"));
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"P6\n416 416\n255\n").unwrap();
    for pixel in pixels { file.write_all(&[pixel.r,pixel.g,pixel.b]).unwrap(); }
}

/// Each image starts with a fresh renderer, avoiding glyph cache artifacts
/// across the long interaction suite. Also checks that key labels have ink.
#[test]
#[ignore = "set HOKI_SETTINGS_PREVIEW and HOKI_SETTINGS_TEST_CAPTURES"]
fn settings_preview() {
    use std::io::Write;
    let state = std::env::var("HOKI_SETTINGS_PREVIEW").unwrap();
    let directory = std::env::var_os("HOKI_SETTINGS_TEST_CAPTURES").unwrap();
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    app.set_battery_level(75);
    app.set_disk_summary("80% used · 3.2/4.0 GiB".into());
    app.set_bt_status("BLE + Classic".into());
    app.set_usb_mode("Network + ADB".into());
    app.set_wifi_status("on".into());
    app.set_airplane_status("off".into());
    app.set_cpu_cores_active(1);
    app.set_auto_cores_label("on".into());
    app.set_sensor_profile("full".into());
    app.set_sleep_reason("Recording is keeping the CPU awake".into());
    let region = match state.as_str() {
        "storage" => { app.set_settings_selected_index(1); (45,125,180,150) }
        "bluetooth-row" => { app.set_settings_selected_index(5); (40,194,200,222) }
        "bluetooth" => { activate_row(&app,5,true); (90,90,325,230) }
        "usb" => { activate_row(&app,8,true); (85,90,330,230) }
        "health" => { app.set_show_health_menu(true); (60,150,300,215) }
        "bottom" => { app.set_settings_selected_index(settings_item_count(&app)-1); (80,196,240,236) }
        "network" => {
            apply_network(&app, &network::Snapshot {
                available:true, wifi_powered:true, status:"online".into(),
                networks:vec![network::Network {
                    path:"/net/connman/service/saved".into(),
                    name:"A very long saved Wi-Fi network name".into(),
                    state:"online".into(), connected:true, strength:Some(74), ..Default::default()
                }], ..Default::default()
            });
            app.set_show_network_menu(true);
            (70,110,330,135)
        }
        _ => panic!("unknown preview"),
    };
    app.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416,416));
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![slint::Rgb8Pixel::default();416*416];
    renderer.draw_if_needed(|r| { r.render(&mut pixels,416); });
    let (left,top,right,bottom) = region;
    let ink = (top..bottom).flat_map(|y|(left..right).map(move |x|y*416+x))
        .filter(|i| { let p=pixels[*i]; p.g > 100 && p.b > 100 }).count();
    assert!(ink > 80, "{state}: required label did not render ({ink} pixels)");
    std::fs::create_dir_all(&directory).unwrap();
    let mut file = std::fs::File::create(std::path::Path::new(&directory).join(format!("preview-{state}.ppm"))).unwrap();
    file.write_all(b"P6\n416 416\n255\n").unwrap();
    for p in pixels { file.write_all(&[p.r,p.g,p.b]).unwrap(); }
}
