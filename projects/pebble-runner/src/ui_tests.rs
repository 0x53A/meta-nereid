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
fn store_pages_use_supported_rebble_collections() {
    assert_eq!(view_to_collection(2), ("all", "watchfaces"));
    assert_eq!(view_to_collection(3), ("all", "watchapps-and-companions"));
    assert_eq!(view_to_collection(4), ("most-loved", "watchfaces"));
}

#[test]
fn watch_screens_render_and_touch_targets_work() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));

    let (tx, rx) = std::sync::mpsc::channel();
    let navigation = tx.clone();
    window.on_navigate(move |view| navigation.send(format!("nav:{view}")).unwrap());
    let selected = tx.clone();
    window.on_app_selected(move |index, fullscreen| selected.send(format!("app:{index}:{fullscreen}")).unwrap());
    window.on_back_pressed(move || tx.send("back".to_string()).unwrap());
    let draw = |name: &str| {
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
        window.window().request_redraw();
        renderer.draw_if_needed(|r| { r.render(&mut pixels, 416); });
        if let Some(dir) = std::env::var_os("PEBBLE_UI_CAPTURES") {
            std::fs::create_dir_all(&dir).unwrap();
            let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                PathBuf::from(dir).join(format!("{name}.png")),
                &bytes, 416, 416, image::ColorType::Rgb8,
            ).unwrap();
        }
    };
    let tap = |x: f32, y: f32| {
        let position = slint::LogicalPosition::new(x, y);
        window.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        window.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    };

    window.set_installed_count(3);
    draw("home");
    tap(208.0, 377.0);
    assert_eq!(rx.try_recv().unwrap(), "nav:6");
    window.set_view(6);
    draw("emu-settings");
    tap(208.0, 175.0);
    assert!(window.get_select_overlay());
    tap(208.0, 377.0);
    assert_eq!(rx.try_recv().unwrap(), "nav:0");
    window.set_view(0);
    tap(208.0, 108.0);
    assert_eq!(rx.try_recv().unwrap(), "nav:1");
    window.set_view(1);
    draw("empty-library");
    window.set_apps(ModelRc::new(VecModel::from(vec![
        PbwEntry { name: "A very long Pebble watchface title".into(), company: "Example Studio".into(), filename: "first.pbw".into(), is_watchface: true },
        PbwEntry { name: "Weather".into(), company: "Another author".into(), filename: "second.pbw".into(), is_watchface: false },
    ])));
    draw("library");
    tap(130.0, 180.0);
    assert_eq!(rx.try_recv().unwrap(), "app:0:false");
    tap(330.0, 180.0);
    assert_eq!(rx.try_recv().unwrap(), "app:0:false");
    tap(130.0, 240.0);
    assert_eq!(rx.try_recv().unwrap(), "app:1:false");
    window.set_apps(ModelRc::new(VecModel::from((0..10).map(|index| PbwEntry {
        name: format!("Pebble app {index}").into(), company: "Author".into(),
        filename: format!("app-{index}.pbw").into(), is_watchface: false,
    }).collect::<Vec<_>>())));
    let scroll_position = slint::LogicalPosition::new(208.0, 120.0);
    for _ in 0..5 {
        window.window().dispatch_event(WindowEvent::PointerScrolled {
            position: scroll_position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert_eq!(window.get_library_viewport_y(), -62.0);
    window.set_view(2);
    window.set_status("Network unavailable".into());
    draw("store-error");
    tap(208.0, 310.0);
    assert_eq!(rx.try_recv().unwrap(), "nav:2");
    window.set_status("".into());
    window.set_store_items(ModelRc::new(VecModel::from(vec![
        StoreEntry { title: "A really long community watchface title".into(), author: "Pebble maker".into(), hearts: 42, status: "installed".into(), has_screenshot: false },
    ])));
    draw("store-list");
    window.set_store_items(ModelRc::new(VecModel::from((0..10).map(|index| StoreEntry {
        title: format!("Store app {index}").into(), author: "Author".into(),
        hearts: 0, status: "available".into(), has_screenshot: false,
    }).collect::<Vec<_>>())));
    for _ in 0..5 {
        window.window().dispatch_event(WindowEvent::PointerScrolled {
            position: scroll_position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert_eq!(window.get_store_viewport_y(), -62.0);
    window.set_view(5);
    window.set_preview_title("A really long community watchface title".into());
    window.set_preview_author("Pebble maker".into());
    let (install_tx, install_rx) = std::sync::mpsc::channel();
    window.on_preview_install(move || install_tx.send(()).unwrap());
    let (launch_tx, launch_rx) = std::sync::mpsc::channel();
    window.on_preview_launch(move || launch_tx.send(()).unwrap());
    let (use_tx, use_rx) = std::sync::mpsc::channel();
    window.on_preview_use(move || use_tx.send(()).unwrap());
    window.set_preview_is_watchface(false);
    window.set_preview_status("".into());
    draw("preview-install");
    tap(269.0, 377.0);
    install_rx.try_recv().unwrap();
    window.set_preview_status("failed".into());
    draw("preview-failed");
    tap(147.0, 377.0);
    assert_eq!(rx.try_recv().unwrap(), "back");
    tap(269.0, 377.0);
    install_rx.try_recv().unwrap();
    window.set_preview_status("installed".into());
    draw("preview-installed");
    tap(269.0, 377.0);
    assert!(install_rx.try_recv().is_err());
    launch_rx.try_recv().unwrap();
    window.set_preview_is_watchface(true);
    draw("preview-installed-watchface");
    tap(269.0, 377.0);
    use_rx.try_recv().unwrap();
    let (button_tx, button_rx) = std::sync::mpsc::channel();
    window.on_button_pressed(move |button| button_tx.send(button).unwrap());
    window.set_current_app("Sample game".into());
    window.set_running(true);
    draw("running-app");
    tap(373.0, 208.0);
    assert_eq!(button_rx.try_recv().unwrap(), 2);
    for (key, expected) in [(slint::platform::Key::F13, 1), (slint::platform::Key::F14, 3)] {
        window.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        assert_eq!(button_rx.try_recv().unwrap(), expected);
    }
    tap(269.0, 377.0);
    assert!(window.get_fullscreen_launch());
    draw("running-fullscreen");
    for (key, expected) in [(slint::platform::Key::F13, 1), (slint::platform::Key::F14, 3)] {
        window.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        assert_eq!(button_rx.try_recv().unwrap(), expected);
    }
    tap(208.0, 371.0);
    assert_eq!(button_rx.try_recv().unwrap(), 2);
    window.set_back_override(true);
    draw("running-fullscreen-back-override");
    tap(156.0, 371.0);
    assert_eq!(button_rx.try_recv().unwrap(), 0);
    tap(260.0, 371.0);
    assert_eq!(button_rx.try_recv().unwrap(), 2);
    window.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(208.0, 208.0), delta_x: 0.0, delta_y: -60.0,
    });
    assert_eq!(button_rx.try_recv().unwrap(), 3);
    let (face_tx, face_rx) = std::sync::mpsc::channel();
    window.on_set_as_watchface(move || face_tx.send(()).unwrap());
    window.set_fullscreen_launch(false);
    window.set_current_app_is_watchface(true);
    draw("running-watchface");
    tap(373.0, 208.0);
    assert!(button_rx.try_recv().is_err());
    window.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::F13.into() });
    assert!(button_rx.try_recv().is_err());
    window.set_fullscreen_launch(true);
    draw("fullscreen-watchface");
    tap(208.0, 371.0);
    assert!(button_rx.try_recv().is_err());
    window.set_fullscreen_launch(false);
    tap(147.0, 377.0);
    assert_eq!(rx.try_recv().unwrap(), "back");
    tap(269.0, 377.0);
    face_rx.try_recv().unwrap();
    assert_eq!(rx.try_recv().unwrap(), "back");
}
