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
fn pale_row_tap_and_crown_lists() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = App::new().unwrap();
    app.set_view("list".into());
    app.set_selected_feed(1);
    app.set_feed_title("PALE".into());
    app.set_episodes(Rc::new(slint::VecModel::from(vec![EpisodeData {
        title: "Pale chapter".into(), detail: "READY".into(), duration: "".into(),
        index: 2, downloaded: true, progress: 0.0,
    }])).into());
    let (sender, receiver) = mpsc::channel();
    app.on_select_episode(move |index| sender.send(index).unwrap());
    app.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    app.window().request_redraw();
    renderer.draw_if_needed(|renderer| {
        renderer.render(&mut vec![slint::Rgb8Pixel::default(); 416 * 416], 416);
    });
    let position = slint::LogicalPosition::new(200.0, 174.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position, button: PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position, button: PointerEventButton::Left,
    });
    assert_eq!(receiver.try_recv().unwrap(), 2);

    app.set_episodes(Rc::new(slint::VecModel::from((0..10).map(|index| EpisodeData {
        title: format!("Episode {index}").into(), detail: "READY".into(), duration: "".into(),
        index, downloaded: true, progress: 0.0,
    }).collect::<Vec<_>>())).into());
    install_crown_scroll(&app);
    for _ in 0..4 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert_eq!(app.get_episode_viewport_y(), 0.0);
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position, delta_x: 0.0, delta_y: -60.0,
    });
    assert_eq!(app.get_episode_viewport_y(), -74.0);
    for _ in 0..5 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: 60.0,
        });
    }
    assert_eq!(app.get_episode_viewport_y(), 0.0);

    app.set_subscriptions(Rc::new(slint::VecModel::from((0..10).map(|index| EpisodeData {
        title: format!("Show {index}").into(), detail: "READY".into(), duration: "".into(),
        index, downloaded: true, progress: 0.0,
    }).collect::<Vec<_>>())).into());
    app.set_view("shows".into());
    for _ in 0..5 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert_eq!(app.get_shows_viewport_y(), -74.0);

    app.set_downloads(Rc::new(slint::VecModel::from((0..10).map(|index| EpisodeData {
        title: format!("Queued {index}").into(), detail: "READY".into(), duration: "".into(),
        index, downloaded: true, progress: 0.0,
    }).collect::<Vec<_>>())).into());
    app.set_view("queue".into());
    for _ in 0..5 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert_eq!(app.get_queue_viewport_y(), -74.0);
}
