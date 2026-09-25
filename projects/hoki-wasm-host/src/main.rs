mod frame;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use wasmi::{Engine, Linker, Module, Store};

slint::include_modules!();

const WASM_BYTES: &[u8] = include_bytes!("../guest.wasm");

fn stop_renderer(
    failed: &Cell<bool>,
    timer: &std::rc::Weak<slint::Timer>,
    app: &slint::Weak<App>,
    reason: impl std::fmt::Display,
) {
    if failed.replace(true) {
        return;
    }
    eprintln!("WASM renderer stopped: {reason}");
    if let Some(timer) = timer.upgrade() {
        timer.stop();
    }
    if let Some(app) = app.upgrade() {
        app.set_status_text("Renderer stopped".into());
    }
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    // --- Load WASM module ---
    let engine = Engine::default();
    let module = Module::new(&engine, WASM_BYTES).expect("failed to parse WASM module");

    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("failed to instantiate")
        .start(&mut store)
        .expect("failed to start");

    // Get exported functions
    let init_fn = instance
        .get_typed_func::<(u32, u32), ()>(&store, "init")
        .expect("missing export: init");
    let render_fn = instance
        .get_typed_func::<(), u32>(&store, "render")
        .expect("missing export: render");
    let on_touch_fn = instance
        .get_typed_func::<(u32, u32, u32), ()>(&store, "on_touch")
        .expect("missing export: on_touch");
    let buffer_len_fn = instance
        .get_typed_func::<(), u32>(&store, "buffer_len")
        .expect("missing export: buffer_len");

    // Render at half res, Slint upscales via Image smooth rendering
    let render_width: u32 = 208;
    let render_height: u32 = 208;

    // Initialize the guest
    init_fn
        .call(&mut store, (render_width, render_height))
        .expect("init failed");

    app.set_status_text("WASM loaded".into());

    // Shared state for callbacks
    let store = Rc::new(RefCell::new(store));
    let failed = Rc::new(Cell::new(false));
    let timer = Rc::new(slint::Timer::default());

    // Touch handler — scale coordinates from display to render resolution
    let store_touch = store.clone();
    let touch_failed = failed.clone();
    let touch_timer = Rc::downgrade(&timer);
    let touch_app = app.as_weak();
    app.on_touch_event(move |x, y, pressed| {
        if touch_failed.get() {
            return;
        }
        let mut s = store_touch.borrow_mut();
        let sx = (x * render_width as f32 / 416.0) as u32;
        let sy = (y * render_height as f32 / 416.0) as u32;
        if let Err(error) = on_touch_fn.call(&mut *s, (sx, sy, if pressed { 1 } else { 0 })) {
            stop_renderer(&touch_failed, &touch_timer, &touch_app, error);
        }
    });

    // Render loop via Slint timer
    let weak = app.as_weak();
    let store_render = store.clone();
    let render_failed = failed.clone();
    let render_timer = Rc::downgrade(&timer);
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(33), // ~30fps
        move || {
            if render_failed.get() {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            let mut s = store_render.borrow_mut();

            // Call render, get pointer to framebuffer in WASM memory
            let ptr = match render_fn.call(&mut *s, ()) {
                Ok(ptr) => ptr,
                Err(error) => {
                    stop_renderer(&render_failed, &render_timer, &weak, error);
                    return;
                }
            };
            let len = match buffer_len_fn.call(&mut *s, ()) {
                Ok(len) => len,
                Err(error) => {
                    stop_renderer(&render_failed, &render_timer, &weak, error);
                    return;
                }
            };

            // Read pixels from WASM linear memory
            let Some(memory) = instance.get_memory(&*s, "memory") else {
                stop_renderer(
                    &render_failed,
                    &render_timer,
                    &weak,
                    "missing memory export",
                );
                return;
            };
            let mem_data = memory.data(&*s);

            let expected = render_width as usize * render_height as usize * 4;
            let Some(pixels) = frame::pixels(mem_data, ptr as usize, len as usize, expected) else {
                stop_renderer(
                    &render_failed,
                    &render_timer,
                    &weak,
                    "invalid framebuffer range or size",
                );
                return;
            };

            // Create Slint image from RGBA buffer (half res, upscaled by Slint)
            let mut pixel_buf = SharedPixelBuffer::<Rgba8Pixel>::new(render_width, render_height);
            pixel_buf.make_mut_bytes().copy_from_slice(pixels);
            app.set_frame_image(Image::from_rgba8(pixel_buf));
        },
    );

    // Clear status after a moment
    let weak2 = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
        if failed.get() {
            return;
        }
        if let Some(app) = weak2.upgrade() {
            app.set_status_text("".into());
        }
    });

    app.run().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HeadlessPlatform;
    impl slint::platform::Platform for HeadlessPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    #[test]
    fn renderer_failure_stops_timer_and_keeps_visible_status() {
        slint::platform::set_platform(Box::new(HeadlessPlatform)).unwrap();
        let app = App::new().unwrap();
        let timer = Rc::new(slint::Timer::default());
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(33),
            || {},
        );
        assert!(timer.running());
        let failed = Cell::new(false);
        stop_renderer(
            &failed,
            &Rc::downgrade(&timer),
            &app.as_weak(),
            "test failure",
        );
        assert!(failed.get());
        assert!(!timer.running());
        assert_eq!(app.get_status_text().as_str(), "Renderer stopped");
        stop_renderer(
            &failed,
            &Rc::downgrade(&timer),
            &app.as_weak(),
            "duplicate failure",
        );
        assert!(!timer.running());
        assert_eq!(app.get_status_text().as_str(), "Renderer stopped");
    }
}
