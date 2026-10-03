// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
//! Persistent status ring and clock alerts.
mod clock;
use slint::{
    ComponentHandle,
    platform::software_renderer::{
        MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
    },
};
use std::{
    io::Write,
    os::fd::{AsFd, AsRawFd, FromRawFd},
    rc::Rc,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
        wl_touch,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as layer_shell, zwlr_layer_surface_v1 as layer_surface,
};
slint::include_modules!();

struct Platform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for Platform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layers: Option<layer_shell::ZwlrLayerShellV1>,
    surface: Option<wl_surface::WlSurface>,
    size: (u32, u32),
    redraw: bool,
    running: bool,
    renderer: Rc<MinimalSoftwareWindow>,
    ui: OverlayWindow,
    touch: Option<(i32, slint::LogicalPosition)>,
    seat: Option<wl_seat::WlSeat>,
    touch_device: Option<wl_touch::WlTouch>,
}
impl State {
    fn pixels(&self) -> Vec<PremultipliedRgbaColor> {
        self.renderer
            .set_size(slint::PhysicalSize::new(self.size.0, self.size.1));
        self.ui.window().request_redraw();
        slint::platform::update_timers_and_animations();
        let mut pixels =
            vec![PremultipliedRgbaColor::default(); (self.size.0 * self.size.1) as usize];
        self.renderer.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, self.size.0 as usize);
        });
        pixels
    }
    fn draw(&self, qh: &QueueHandle<Self>) {
        let bytes: Vec<u8> = self
            .pixels()
            .iter()
            .flat_map(|p| [p.blue, p.green, p.red, p.alpha])
            .collect();
        let fd = unsafe { libc::memfd_create(c"hoki-overlay".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&bytes).expect("write ring buffer");
        let pool = self
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), bytes.len() as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            self.size.0 as i32,
            self.size.1 as i32,
            self.size.0 as i32 * 4,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, self.size.0 as i32, self.size.1 as i32);
        surface.commit();
        pool.destroy();
        // No animation/frame callback loop: a static ring submits only on configure.
    }
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Platform(renderer.clone()))).unwrap();
    let ui = OverlayWindow::new().unwrap();
    if let Some(i) = args.iter().position(|arg| arg == "--width") {
        let width: f32 = args
            .get(i + 1)
            .expect("--width needs pixels")
            .parse()
            .expect("invalid width");
        assert!(
            width.is_finite() && (1.0..=8.0).contains(&width),
            "width must be 1..8 pixels"
        );
        ui.set_ring_width(width);
    }
    ui.show().unwrap();
    let mut state = State {
        compositor: None,
        shm: None,
        layers: None,
        surface: None,
        size: (416, 416),
        redraw: false,
        running: true,
        renderer,
        ui,
        touch: None,
        seat: None,
        touch_device: None,
    };
    if args.iter().any(|arg| arg == "--preview-alert") {
        state.ui.set_alert(true);
        state.ui.set_alarm(true);
        state.ui.set_label("Morning alarm".into());
    }
    if args.iter().any(|arg| arg == "--preview-timers") {
        let view = clock::View {
            snapshot: serde_json::json!({"timers":[{"total":100000,"remaining":60000,"running":true},{"total":100000,"remaining":40000,"running":false}]}),
            received: clock::boottime_ms(),
        };
        state.ui.set_arcs(
            std::rc::Rc::new(slint::VecModel::from(view.arcs(state.ui.get_ring_width()))).into(),
        );
    }
    if let Some(i) = args.iter().position(|arg| arg == "--preview") {
        let pixels = state.pixels();
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|p| {
                let straight = |v: u8| {
                    if p.alpha == 0 {
                        0
                    } else {
                        ((v as u32 * 255) / p.alpha as u32).min(255) as u8
                    }
                };
                [
                    straight(p.red),
                    straight(p.green),
                    straight(p.blue),
                    p.alpha,
                ]
            })
            .collect();
        image::save_buffer(
            args.get(i + 1).expect("--preview needs a PNG path"),
            &bytes,
            416,
            416,
            image::ColorType::Rgba8,
        )
        .unwrap();
        return;
    }
    let updates = clock::start();
    let mut view = clock::View::default();
    let mut visible = true;
    let selected = std::rc::Rc::new(std::cell::Cell::new(None::<(bool, u64)>));
    let (action_tx, action_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let tx = action_tx.clone();
    state.ui.on_dismiss(move || {
        let _ = tx.try_send(false);
    });
    state.ui.on_snooze(move || {
        let _ = action_tx.try_send(true);
    });
    let conn = Connection::connect_to_env().expect("connect to Wayland");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut state).unwrap();
    let compositor = state.compositor.as_ref().expect("wl_compositor");
    let surface = compositor.create_surface(&qh, ());
    let empty = compositor.create_region(&qh, ());
    surface.set_input_region(Some(&empty));
    empty.destroy();
    let layer = state
        .layers
        .as_ref()
        .expect("layer-shell")
        .get_layer_surface(
            &surface,
            None,
            layer_shell::Layer::Overlay,
            "hoki-overlay".into(),
            &qh,
            (),
        );
    layer.set_size(0, 0);
    layer.set_anchor(
        layer_surface::Anchor::Top
            | layer_surface::Anchor::Bottom
            | layer_surface::Anchor::Left
            | layer_surface::Anchor::Right,
    );
    layer.set_exclusive_zone(-1); // Cover the display without reserving app space.
    layer.set_keyboard_interactivity(layer_surface::KeyboardInteractivity::None);
    surface.commit();
    state.surface = Some(surface);
    let mut last_activity_draw = std::time::Instant::now();
    while state.running {
        queue
            .dispatch_pending(&mut state)
            .expect("Wayland pending events");
        for event in updates.rx.try_iter() {
            match event {
                clock::Event::Snapshot(snapshot) => {
                    state
                        .ui
                        .set_error(snapshot["delivery_error"].as_str().unwrap_or("").into());
                    view.snapshot = snapshot;
                    view.received = clock::boottime_ms();
                    state.redraw = true;
                }
                clock::Event::Visibility(on) => {
                    visible = on;
                    if on {
                        state.redraw = true;
                    }
                }
                clock::Event::ActionError(error) => {
                    state.ui.set_error(error.into());
                    state.redraw = true;
                }
                clock::Event::Error(error) => {
                    eprintln!("Clock: {error}");
                    view.snapshot = serde_json::Value::Null;
                    state.redraw = true;
                }
            }
        }
        for snooze in action_rx.try_iter() {
            if let Some((alarm, id)) = selected.get() {
                // Calls run away from Wayland dispatch; the Changed signal refreshes the UI.
                let events = updates.tx.clone();
                let fd = updates.fd.clone();
                std::thread::spawn(move || {
                    if let Err(error) = clock::action(alarm, id, snooze) {
                        clock::send(&events, &fd, clock::Event::ActionError(error));
                    }
                });
            }
        }
        if visible
            && view.active()
            && last_activity_draw.elapsed() >= std::time::Duration::from_secs(1)
        {
            state.redraw = true;
        }
        if state.redraw && visible && state.running {
            let alert = view.alert();
            selected.set(alert.as_ref().map(|(a, id, _)| (*a, *id)));
            state.ui.set_alert(alert.is_some());
            state.ui.set_alarm(alert.as_ref().is_some_and(|a| a.0));
            state
                .ui
                .set_label(alert.map(|a| a.2).unwrap_or_default().into());
            state.ui.set_arcs(
                std::rc::Rc::new(slint::VecModel::from(view.arcs(state.ui.get_ring_width())))
                    .into(),
            );
            let region = state.compositor.as_ref().unwrap().create_region(&qh, ());
            if state.ui.get_alert() {
                region.add(0, 0, state.size.0 as i32, state.size.1 as i32);
            }
            state
                .surface
                .as_ref()
                .unwrap()
                .set_input_region(Some(&region));
            region.destroy();
            state.draw(&qh);
            state.redraw = false;
            last_activity_draw = std::time::Instant::now();
        }
        if !state.running {
            break;
        }
        conn.flush().expect("Wayland flush");
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut fds = [conn.as_fd().as_raw_fd(), updates.fd.as_raw_fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        let timeout = if visible && view.active() { 1000 } else { -1 };
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if result < 0 {
            drop(guard);
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("overlay poll failed");
        }
        if fds[0].revents != 0 {
            guard.read().expect("Wayland read");
        } else {
            drop(guard);
        }
        if fds[1].revents != 0 {
            let mut n = 0u64;
            unsafe {
                libc::read(updates.fd.as_raw_fd(), (&mut n as *mut u64).cast(), 8);
            }
        }
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    state.layers = Some(registry.bind(name, version.min(4), qh, ()))
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<layer_surface::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &layer_surface::ZwlrLayerSurfaceV1,
        event: layer_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            layer_surface::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                if width > 0 {
                    state.size.0 = width;
                }
                if height > 0 {
                    state.size.1 = height;
                }
                assert!(
                    state.size.0 <= 4096 && state.size.1 <= 4096,
                    "unexpected output size"
                );
                state.redraw = true;
            }
            layer_surface::Event::Closed => state.running = false,
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore layer_shell::ZwlrLayerShellV1);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ring_leaves_center_transparent_and_uses_premultiplied_alpha() {
        let renderer = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Platform(renderer.clone()))).unwrap();
        let ui = OverlayWindow::new().unwrap();
        ui.show().unwrap();
        let state = State {
            compositor: None,
            shm: None,
            layers: None,
            surface: None,
            size: (416, 416),
            redraw: false,
            running: true,
            renderer,
            ui,
            touch: None,
            seat: None,
            touch_device: None,
        };
        for width in [1., 2., 3.] {
            state.ui.set_ring_width(width);
            let pixels = state.pixels();
            let mut painted = 0;
            for (i, p) in pixels.iter().enumerate() {
                assert!(p.red <= p.alpha && p.green <= p.alpha && p.blue <= p.alpha);
                if p.alpha == 0 {
                    continue;
                }
                painted += 1;
                let x = (i % 416) as f32 + 0.5 - 208.;
                let y = (i / 416) as f32 + 0.5 - 208.;
                assert!(
                    x * x + y * y >= (207. - width).powi(2),
                    "ring painted over app interior"
                );
            }
            assert!(
                painted > 800 && painted < 6000,
                "expected thin visible ring: {painted}"
            );
        }
        let dismiss = Rc::new(std::cell::Cell::new(0));
        let snooze = Rc::new(std::cell::Cell::new(0));
        let d = dismiss.clone();
        state.ui.on_dismiss(move || d.set(d.get() + 1));
        let s = snooze.clone();
        state.ui.on_snooze(move || s.set(s.get() + 1));
        state.ui.set_alert(true);
        state.ui.set_alarm(true);
        let _ = state.pixels();
        use slint::platform::{PointerEventButton, WindowEvent};
        for y in [270., 335.] {
            let position = slint::LogicalPosition::new(208., y);
            state
                .ui
                .window()
                .dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
            state
                .ui
                .window()
                .dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                });
        }
        assert_eq!(dismiss.get(), 1);
        assert_eq!(snooze.get(), 1);
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use slint::platform::{PointerEventButton, WindowEvent};
        let event = match event {
            wl_touch::Event::Down { id, x, y, .. } if state.touch.is_none() => {
                let p = slint::LogicalPosition::new(x as f32, y as f32);
                state.touch = Some((id, p));
                Some(WindowEvent::PointerPressed {
                    position: p,
                    button: PointerEventButton::Left,
                })
            }
            wl_touch::Event::Motion { id, x, y, .. } if state.touch.is_some_and(|t| t.0 == id) => {
                let p = slint::LogicalPosition::new(x as f32, y as f32);
                state.touch = Some((id, p));
                Some(WindowEvent::PointerMoved { position: p })
            }
            wl_touch::Event::Up { id, .. } if state.touch.is_some_and(|t| t.0 == id) => {
                let (_, p) = state.touch.take().unwrap();
                Some(WindowEvent::PointerReleased {
                    position: p,
                    button: PointerEventButton::Left,
                })
            }
            wl_touch::Event::Cancel => {
                state.touch = None;
                Some(WindowEvent::PointerExited)
            }
            _ => None,
        };
        if let Some(event) = event {
            state.ui.window().dispatch_event(event);
            state.redraw = true;
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: wayland_client::WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Touch) && state.touch_device.is_none() {
                state.touch_device = Some(seat.get_touch(qh, ()));
            } else if !caps.contains(wl_seat::Capability::Touch) {
                if let Some(touch) = state.touch_device.take() { if touch.version() >= 3 { touch.release(); } }
                state.touch = None;
                state.ui.window().dispatch_event(slint::platform::WindowEvent::PointerExited);
            }
        }
    }
}
