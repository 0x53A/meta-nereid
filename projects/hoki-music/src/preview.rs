use crate::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use std::io::Write;
struct Platform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for Platform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
pub fn render(state: &str, path: &str) -> anyhow::Result<()> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Platform(window.clone())))?;
    let app = App::new()?;
    app.set_track_title(
        if state == "long" {
            "A very long track title that wraps…"
        } else {
            "Night Swimming"
        }
        .into(),
    );
    app.set_artist("The Violet Hours".into());
    app.set_source_label("LOCAL MUSIC".into());
    app.set_has_track(true);
    app.set_playing(true);
    app.set_time_label("1:24 / 4:08".into());
    app.set_progress(0.34);
    if state == "library" {
        app.set_page(1);
        app.set_tracks(ModelRc::new(VecModel::from(vec![
            TrackRow {
                key: "".into(),
                title: "Night Swimming".into(),
                subtitle: "The Violet Hours · Local".into(),
            },
            TrackRow {
                key: "".into(),
                title: "Into the morning light".into(),
                subtitle: "Northern Skies · Navidrome".into(),
            },
            TrackRow {
                key: "".into(),
                title: "Long journey home".into(),
                subtitle: "An artist with a long name".into(),
            },
        ])));
    }
    if state == "sources" {
        app.set_page(2);
    }
    if state == "seek" {
        app.set_page(3);
    }
    if state == "empty" {
        app.set_page(1);
    }
    if state == "error" {
        app.set_notice("Server unreachable. Local music is still available.".into());
    }
    if state == "paused" {
        app.set_playing(false);
    }
    app.show()?;
    window.set_size(slint::PhysicalSize::new(416, 416));
    let mut buffer = vec![slint::Rgb8Pixel::default(); 416 * 416];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, 416);
    });
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"P6\n416 416\n255\n")?;
    for (i, p) in buffer.iter().enumerate() {
        let x = i % 416;
        let y = i / 416;
        let inside = (x as i32 - 208).pow(2) + (y as i32 - 208).pow(2) <= 208 * 208;
        let pixel = if inside { [p.r, p.g, p.b] } else { [0, 0, 0] };
        file.write_all(&pixel)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::{PointerEventButton, WindowEvent};
    use std::cell::RefCell;
    #[test]
    fn player_touch_controls_and_navigation() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Platform(window.clone()))).unwrap();
        let app = App::new().unwrap();
        app.set_has_track(true);
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let c = calls.clone();
        app.on_action(move |a| c.borrow_mut().push(a.into()));
        let c = calls.clone();
        app.on_volume_change(move |v| c.borrow_mut().push(format!("volume:{v}")));
        app.show().unwrap();
        window.set_size(slint::PhysicalSize::new(416, 416));
        let tap = |x, y| {
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|r| {
                r.render(&mut vec![slint::Rgb8Pixel::default(); 416 * 416], 416);
            });
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };
        tap(105., 102.);
        tap(208., 102.);
        tap(310., 102.);
        tap(100., 310.);
        tap(314., 310.);
        assert_eq!(
            *calls.borrow(),
            ["previous", "toggle", "next", "volume:55", "volume:65"]
        );
        app.set_busy(true);
        tap(208., 102.);
        assert_eq!(calls.borrow().len(), 5);
        tap(208., 210.);
        assert_eq!(app.get_page(), 3);
        tap(148., 374.);
        assert_eq!(app.get_page(), 0);
        tap(268., 374.);
        assert_eq!(app.get_page(), 1);
    }
}
