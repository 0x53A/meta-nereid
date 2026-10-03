//! One independent, capture-paced desktop. No physical display power ownership.
use crate::{capture::CaptureState, AppSurface, Compositor};
use smithay::{
    input::{Seat, SeatState},
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    utils::{Transform, SERIAL_COUNTER},
};
use std::time::{Duration, Instant};
use wayland_server::{
    backend::GlobalId,
    protocol::{wl_callback::WlCallback, wl_surface::WlSurface},
    DisplayHandle, Resource,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub enabled: bool,
    pub width: i32,
    pub height: i32,
    pub refresh: i32,
    pub scale: f64,
    pub position: (i32, i32),
    pub transform: i32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            width: 1280,
            height: 720,
            refresh: 30000,
            scale: 1.0,
            position: (0, 0),
            transform: 0,
        }
    }
}
impl Config {
    pub fn valid(self) -> bool {
        (64..=4096).contains(&self.width)
            && (64..=4096).contains(&self.height)
            && self.width * self.height <= 8_388_608 // at most 32 MiB of RGBA; encoder limits belong to the host
            && (1000..=120000).contains(&self.refresh)
            && [1.0, 2.0, 3.0, 4.0].contains(&self.scale)
            && self.width % self.scale as i32 == 0
            && self.height % self.scale as i32 == 0
            && self.position == (0, 0)
            && self.transform == 0
    }
    pub fn logical_size(self) -> (i32, i32) {
        (
            self.width / self.scale as i32,
            self.height / self.scale as i32,
        )
    }
    pub fn mode(self) -> Mode {
        Mode {
            size: (self.width, self.height).into(),
            refresh: self.refresh,
        }
    }
}

pub struct Desktop {
    pub config: Config,
    pub output: Output,
    pub global: Option<GlobalId>,
    retired_globals: Vec<(Instant, GlobalId)>,
    pub capture: CaptureState,
    pub seat: Seat<Compositor>,
    pub surfaces: Vec<AppSurface>,
    pub focus: Option<WlSurface>,
    pub callbacks: Vec<(WlSurface, WlCallback)>,
    pixels: Vec<u8>,
    last_render: Option<Instant>,
    pub pointer: (f64, f64),
    pressed_buttons: Vec<u32>,
}
impl Desktop {
    pub fn new(dh: &DisplayHandle, seats: &mut SeatState<Compositor>) -> Self {
        let mut seat = seats.new_wl_seat(dh, "desktop");
        seat.add_pointer();
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("desktop keyboard");
        let output = Self::new_output();
        let config = Config::default();
        let mut capture =
            CaptureState::for_output(output.clone(), config.width as u32, config.height as u32);
        capture.set_active(false);
        Self {
            config,
            output,
            global: None,
            retired_globals: vec![],
            capture,
            seat,
            surfaces: vec![],
            focus: None,
            callbacks: vec![],
            pixels: vec![],
            last_render: None,
            pointer: (0., 0.),
            pressed_buttons: vec![],
        }
    }
    fn new_output() -> Output {
        Output::new(
            "hoki-desktop".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Nereid".into(),
                model: "Virtual desktop".into(),
            },
        )
    }
    /// Leave a grace period for registry bind requests already in flight.
    /// Cleanup is opportunistic; no timer needs to wake a sleeping watch for it.
    pub fn reap_globals(&mut self, dh: &DisplayHandle, now: Instant) {
        self.retired_globals.retain(|(retired, id)| {
            if now.saturating_duration_since(*retired) >= Duration::from_secs(5) {
                dh.remove_global::<Compositor>(id.clone());
                false
            } else {
                true
            }
        });
    }
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        if !self.config.enabled || !self.capture.has_demand() {
            return None;
        }
        let period = Duration::from_nanos(1_000_000_000_000 / self.config.refresh as u64);
        Some(
            self.last_render
                .map(|t| period.saturating_sub(now.saturating_duration_since(t)))
                .unwrap_or_default(),
        )
    }
    pub fn render(&mut self, now: Instant, time_ms: u32) -> bool {
        if self.deadline(now) != Some(Duration::ZERO) {
            return false;
        }
        self.pixels.fill(0);
        let w = self.config.width as usize;
        let h = self.config.height as usize;
        if let Some(buf) = self
            .surfaces
            .iter()
            .find(|s| Some(&s.surface) == self.focus.as_ref())
            .and_then(|s| s.buffer.as_ref())
        {
            // Most desktop clients render at output size. Avoid per-pixel integer
            // division on ARM (where generic builds may use a software helper).
            let source_x: Vec<usize> = if buf.width as usize == w {
                Vec::new()
            } else {
                (0..w).map(|x| x * buf.width as usize / w * 4).collect()
            };
            for (y, row) in self.pixels.chunks_exact_mut(w * 4).enumerate() {
                let from = (y * buf.height as usize / h) * buf.stride as usize;
                if source_x.is_empty() && !buf.swap_rb {
                    row.copy_from_slice(&buf.data[from..from + w * 4]);
                    for pixel in row.chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                } else {
                    for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                        let offset = from
                            + if source_x.is_empty() {
                                x * 4
                            } else {
                                source_x[x]
                            };
                        let p = &buf.data[offset..offset + 4];
                        pixel.copy_from_slice(&if buf.swap_rb {
                            [p[2], p[1], p[0], 255]
                        } else {
                            [p[0], p[1], p[2], 255]
                        });
                    }
                }
            }
        }

        self.capture.cursor = Some((
            (self.pointer.0 * self.config.scale) as u32,
            (self.pointer.1 * self.config.scale) as u32,
        ));
        self.capture.presented();
        self.capture.copy_pending(&self.pixels);
        self.callbacks.retain(|(surface, cb)| {
            if !surface.is_alive() || !cb.is_alive() {
                return false;
            }
            if Some(surface) == self.focus.as_ref() {
                cb.done(time_ms);
                false
            } else {
                true
            }
        });
        self.last_render = Some(now);
        true
    }
}
impl Compositor {
    pub fn apply_desktop_config(&mut self, config: Config) {
        let capture_allowed = config.enabled && !self.is_locked();
        if !config.enabled && self.wayland.desktop.config.enabled {
            if let Some(kb) = self.wayland.desktop.seat.get_keyboard() {
                for code in kb.pressed_keys() {
                    self.handle_desktop_input(
                        crate::input::DesktopInput::Key(crate::input::ButtonEvent {
                            code: u32::from(code) - 8,
                            pressed: false,
                        }),
                        0,
                    );
                }
            }
            for code in self.wayland.desktop.pressed_buttons.clone() {
                self.handle_desktop_input(
                    crate::input::DesktopInput::Button(crate::input::ButtonEvent {
                        code,
                        pressed: false,
                    }),
                    0,
                );
            }
        }
        let dh = self.display_handle.clone();
        let d = &mut self.wayland.desktop;
        if d.config == config {
            return;
        }
        // Constraints and output lifetime change atomically. Consumers recreate
        // their source/session after stopped, including on a resize.
        d.capture.set_active(false);
        d.last_render = None;
        d.config = config;
        let (lw, lh) = config.logical_size();
        d.pointer.0 = d.pointer.0.clamp(0., (lw - 1) as f64);
        d.pointer.1 = d.pointer.1.clamp(0., (lh - 1) as f64);
        if config.enabled {
            if d.global.is_none() {
                d.output = Desktop::new_output();
            }
            for mode in d.output.modes() {
                d.output.delete_mode(mode);
            }
            d.output.change_current_state(
                Some(config.mode()),
                Some(Transform::Normal),
                Some(Scale::Integer(config.scale as i32)),
                Some((0, 0).into()),
            );
            d.output.set_preferred(config.mode());
            if d.global.is_none() {
                d.global = Some(d.output.create_global::<Compositor>(&dh));
            }
            d.pixels
                .resize(config.width as usize * config.height as usize * 4, 0);
            d.capture.resize(config.width as u32, config.height as u32);
            d.capture.set_active(capture_allowed);
            for s in &d.surfaces {
                d.output.enter(&s.surface);
            }
        } else {
            d.capture.set_active(false);
            for s in &d.surfaces {
                d.output.leave(&s.surface);
            }
            if let Some(id) = d.global.take() {
                dh.disable_global::<Compositor>(id.clone());
                d.retired_globals.push((Instant::now(), id));
            }
            d.pixels = Vec::new();
        }
        let tops = self.wayland.xdg_shell_state.toplevel_surfaces().to_vec();
        for top in tops {
            if self.is_desktop_surface(top.wl_surface()) {
                self.configure_toplevel_fullscreen(&top);
            }
        }
        self.refresh_desktop_focus();
    }
    pub fn is_desktop_surface(&self, surface: &WlSurface) -> bool {
        // wl_surface can be destroyed before its xdg role on disconnect; Smithay
        // clears the surface data map in that order. Retained role ownership wins.
        if self
            .wayland
            .desktop
            .surfaces
            .iter()
            .any(|s| &s.surface == surface)
        {
            return true;
        }
        smithay::wayland::compositor::with_states(surface, |states| {
            states
                .data_map
                .get::<crate::wayland::SurfaceDomain>()
                .is_some_and(|d| d.0)
        })
    }
    pub fn refresh_desktop_focus(&mut self) {
        let d = &mut self.wayland.desktop;
        if !d.surfaces.iter().any(|s| {
            Some(&s.surface) == d.focus.as_ref() && s.surface.is_alive() && s.buffer.is_some()
        }) {
            d.focus = d
                .surfaces
                .iter()
                .rev()
                .find(|s| s.surface.is_alive() && s.buffer.is_some())
                .map(|s| s.surface.clone());
        }
        let focus = d.config.enabled.then(|| d.focus.clone()).flatten();
        if let Some(kb) = d.seat.get_keyboard() {
            kb.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        }
        self.desktop_pointer_motion(0., 0., 0);
        for top in self.wayland.xdg_shell_state.toplevel_surfaces() {
            if !self.is_desktop_surface(top.wl_surface()) {
                continue;
            }
            let active = self.wayland.desktop.config.enabled
                && self.wayland.desktop.focus.as_ref() == Some(top.wl_surface());
            let changed = top.with_pending_state(|s| {
                use wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                let changed = s.states.contains(State::Activated) != active;
                if active {
                    s.states.set(State::Activated);
                } else {
                    s.states.unset(State::Activated);
                }
                changed
            });
            if changed {
                top.send_configure();
            }
        }
    }
}

impl Compositor {
    pub fn desktop_pointer_motion(&mut self, dx: f64, dy: f64, time: u32) {
        let d = &mut self.wayland.desktop;
        let (w, h) = d.config.logical_size();
        if dx.is_finite() && dy.is_finite() {
            d.pointer.0 = (d.pointer.0 + dx).clamp(0., (w - 1) as f64);
            d.pointer.1 = (d.pointer.1 + dy).clamp(0., (h - 1) as f64);
        }
        let location = d.pointer.into();
        let focus = if d.config.enabled {
            d.focus.clone().map(|s| (s, (0., 0.).into()))
        } else {
            None
        };
        if let Some(pointer) = d.seat.get_pointer() {
            pointer.motion(
                self,
                focus,
                &smithay::input::pointer::MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.frame(self);
        }
    }
    pub fn handle_desktop_input(&mut self, event: crate::input::DesktopInput, time: u32) {
        use crate::input::DesktopInput;
        use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
        use smithay::input::{
            keyboard::{FilterResult, Keycode},
            pointer::{AxisFrame, ButtonEvent},
        };
        if !self.wayland.desktop.config.enabled {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        match event {
            DesktopInput::Key(key) => {
                if let Some(kb) = self.wayland.desktop.seat.get_keyboard() {
                    kb.input::<(), _>(
                        self,
                        Keycode::new(key.code + 8),
                        if key.pressed {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        },
                        serial,
                        time,
                        |_, _, _| FilterResult::Forward,
                    );
                }
            }
            DesktopInput::Motion { dx, dy } => self.desktop_pointer_motion(dx, dy, time),
            DesktopInput::Absolute { x, y } => {
                if x.is_finite() && y.is_finite() {
                    let (w, h) = self.wayland.desktop.config.logical_size();
                    let old = self.wayland.desktop.pointer;
                    self.desktop_pointer_motion(
                        x.clamp(0., 1.) * (w - 1) as f64 - old.0,
                        y.clamp(0., 1.) * (h - 1) as f64 - old.1,
                        time,
                    );
                }
            }
            DesktopInput::Button(button) => {
                self.wayland
                    .desktop
                    .pressed_buttons
                    .retain(|b| *b != button.code);
                if button.pressed {
                    self.wayland.desktop.pressed_buttons.push(button.code);
                }
                if let Some(pointer) = self.wayland.desktop.seat.get_pointer() {
                    pointer.button(
                        self,
                        &ButtonEvent {
                            serial,
                            time,
                            button: button.code,
                            state: if button.pressed {
                                ButtonState::Pressed
                            } else {
                                ButtonState::Released
                            },
                        },
                    );
                    pointer.frame(self);
                }
            }
            DesktopInput::Scroll {
                horizontal,
                vertical,
            } => {
                if let Some(pointer) = self.wayland.desktop.seat.get_pointer() {
                    let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
                    for (axis, value) in
                        [(Axis::Horizontal, horizontal), (Axis::Vertical, vertical)]
                    {
                        if value != 0 {
                            frame = frame
                                .v120(axis, value)
                                .value(axis, value as f64 / 120. * 15.);
                        }
                    }
                    pointer.axis(self, frame);
                    pointer.frame(self);
                }
            }
        }
    }
}
