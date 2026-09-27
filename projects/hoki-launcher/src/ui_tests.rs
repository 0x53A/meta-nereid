use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for TestPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn folders_touch_crown_back_and_rendering() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    let apps = [
        ("hoki-audiobook", "Audiobooks"),
        ("hoki-music", "Music with a deliberately long title"),
        ("asteroid-weather", "Weather"),
        ("imu-test-app", "IMU Test"),
        ("hoki-nfc", "NFC"),
        ("bt-pair", "Bluetooth pairing"),
    ]
    .iter()
    .map(|(id, name)| {
        desktop::parse(
            &format!("[Desktop Entry]\nName={name}\nExec=example \"two words\"\n"),
            &std::path::PathBuf::from(format!("{id}.desktop")),
        )
        .unwrap()
        .unwrap()
    })
    .collect::<Vec<_>>();
    let mut images = vec![slint::Image::default(); apps.len()];
    images[0] = slint::Image::load_from_path(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/folder.svg"),
    )
    .unwrap();
    let browser = Rc::new(RefCell::new(Browser {
        apps,
        images,
        config: serde_json::Value::Null,
        current: String::new(),
        parent_index: 0,
    }));
    browser.borrow().render(&window, 0);
    let weak = window.as_weak();
    let state = browser.clone();
    window.on_row_activated(move |index| state.borrow_mut().activate(&weak.unwrap(), index));
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let draw = |name: &str| {
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
        window.window().request_redraw();
        renderer.draw_if_needed(|r| {
            r.render(&mut pixels, 416);
        });
        if let Some(dir) = std::env::var_os("HOKI_LAUNCHER_TEST_CAPTURES") {
            use std::io::Write;
            std::fs::create_dir_all(&dir).unwrap();
            let mut file =
                std::fs::File::create(std::path::PathBuf::from(dir).join(format!("{name}.ppm")))
                    .unwrap();
            file.write_all(b"P6\n416 416\n255\n").unwrap();
            for pixel in pixels {
                file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
            }
        }
    };
    let tap = |y| {
        for pressed in [true, false] {
            let position = slint::LogicalPosition::new(208.0, y);
            window.window().dispatch_event(if pressed {
                WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                }
            } else {
                WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                }
            });
        }
    };
    assert_eq!(window.get_apps().row_count(), 5);
    draw("main");
    let accum = AtomicI32::new(0);
    handle_compositor_message(&window, "scroll:10", &accum);
    assert_eq!(window.get_selected_index(), 2);
    draw("folders");
    tap(215.0); // selected Tools folder, after crown scroll
    assert_eq!(window.get_page_title(), "Tools");
    assert_eq!(window.get_apps().row_count(), 3);
    assert_eq!(window.get_selected_index(), 1);
    draw("tools");
    tap(98.0); // visible All apps back row
    assert_eq!(window.get_page_title(), "Apps");
    assert_eq!(window.get_selected_index(), 2);
    draw("restored");
    window.invoke_row_activated(0);
    assert_eq!(window.get_page_title(), "AsteroidOS");
    draw("asteroid");
    browser.borrow_mut().back(&window);
    assert_eq!(window.get_selected_index(), 0);
    // A small drag must neither launch nor jump a row; hover is inert too.
    draw("before-drag");
    window.window().dispatch_event(WindowEvent::PointerPressed {
        position: slint::LogicalPosition::new(208.0, 210.0),
        button: PointerEventButton::Left,
    });
    window.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(208.0, 185.0),
    });
    window
        .window()
        .dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(208.0, 185.0),
            button: PointerEventButton::Left,
        });
    assert_eq!(window.get_selected_index(), 0);
    assert_eq!(window.get_page_title(), "Apps");
    window.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(208.0, 0.0),
    });
    assert_eq!(window.get_selected_index(), 0);
    // Plain-text mode retains folder affordances and layout.
    browser.borrow_mut().images.fill(slint::Image::default());
    browser.borrow().render(&window, 0);
    draw("text-only");
    browser.borrow_mut().apps.clear();
    browser.borrow().render(&window, 0);
    draw("empty");
    window.invoke_row_activated(-1);
    window.invoke_row_activated(99);
    assert_eq!(window.get_apps().row_count(), 0);
}
