use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use std::rc::Rc;
struct Platform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn uploading_screen_fits_round_display_even_with_long_face_names() {
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Platform(renderer.clone()))).unwrap();
    let window = MainWindow::new().unwrap();
    window.show().unwrap();
    renderer.set_size(slint::PhysicalSize::new(416, 416));
    let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
    for (label, name) in [("digital", "Hoki Digital"), ("long-name", "A very long registered low-power watchface name that must be elided")] {
        window.set_face_name(name.into());
        window.window().request_redraw();
        renderer.draw_if_needed(|r| { r.render(&mut pixels, 416); });
        let mut painted = 0;
        for (i, p) in pixels.iter().enumerate() {
            if p.r == 0 && p.g == 0 && p.b == 0 { continue; }
            painted += 1;
            let x = (i % 416) as i32 - 208;
            let y = (i / 416) as i32 - 208;
            assert!(x*x + y*y < 192*192, "text outside circular safe area");
        }
        assert!(painted > 500, "expected visible text");
        assert!(pixels[185*416..239*416].iter().filter(|p| p.r > 0).count() > 500, "Uploading label must remain visible");
        if let Some(dir) = std::env::var_os("HOKI_PLACEHOLDER_CAPTURES") {
            std::fs::create_dir_all(&dir).unwrap();
            let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r,p.g,p.b]).collect();
            image::save_buffer(std::path::PathBuf::from(dir).join(format!("{label}.png")), &bytes, 416, 416, image::ColorType::Rgb8).unwrap();
        }
    }
}
