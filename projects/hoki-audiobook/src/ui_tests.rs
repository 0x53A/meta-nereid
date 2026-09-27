use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::WindowEvent;
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
fn crown_scrolls_books_and_chapters_one_row_per_five_ticks() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(renderer.clone()))).unwrap();
    let app = App::new().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    app.set_books(Rc::new(slint::VecModel::from((0..10).map(|index| BookEntry {
        id: format!("book-{index}").into(), title: format!("Book {index}").into(),
        num_chapters: 10, progress_percent: 0.0, last_position: "".into(),
    }).collect::<Vec<_>>())).into());
    app.show().unwrap();
    app.window().request_redraw();
    renderer.draw_if_needed(|r| {
        r.render(&mut vec![slint::Rgb8Pixel::default(); 416 * 416], 416);
    });
    let position = slint::LogicalPosition::new(208.0, 100.0);
    for _ in 0..5 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert!((app.get_library_viewport_y() + 416.0 * 0.135).abs() < 0.1);

    app.set_chapters(Rc::new(slint::VecModel::from((0..10).map(|index| ChapterEntry {
        index, title: format!("Chapter {index}").into(),
    }).collect::<Vec<_>>())).into());
    app.set_show_player(true);
    app.set_show_chapters(true);
    app.window().request_redraw();
    renderer.draw_if_needed(|r| {
        r.render(&mut vec![slint::Rgb8Pixel::default(); 416 * 416], 416);
    });
    let position = slint::LogicalPosition::new(208.0, 100.0);
    app.window().dispatch_event(WindowEvent::PointerMoved { position });
    for _ in 0..5 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position, delta_x: 0.0, delta_y: -60.0,
        });
    }
    assert!((app.get_chapter_viewport_y() + 416.0 * 0.135).abs() < 0.1,
        "chapter offset: {}", app.get_chapter_viewport_y());
}
