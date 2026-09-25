use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;
use tracing::info;

use input::event::EventTrait;
use input::event::keyboard::KeyboardEventTrait;
use input::event::touch::{TouchEventPosition, TouchEventSlot};

/// Touch event data.
#[derive(Debug, Clone)]
pub struct TouchEvent {
    pub slot: u32,
    pub x: f64,
    pub y: f64,
    pub state: TouchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchState {
    Down,
    Motion,
    Up,
    Cancel,
}

/// Button event data.
#[derive(Debug, Clone)]
pub struct ButtonEvent {
    pub code: u32,
    pub pressed: bool,
}

/// Scroll (crown wheel) event data.
#[derive(Debug, Clone)]
pub struct ScrollEvent {
    /// Scroll value in v120 units (120 = one discrete tick).
    /// Positive = scroll down/clockwise, negative = scroll up/counter-clockwise.
    pub v120: i32,
}

/// Input event from any device.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Touch(TouchEvent),
    Button(ButtonEvent),
    Scroll(ScrollEvent),
}

/// Manages evdev input devices for the watch.
///
/// On hoki:
/// - /dev/input/event3 = touchscreen
/// - /dev/input/event4 = side button
/// - /dev/input/event0 = additional buttons
pub struct InputManager {
    context: Option<input::Libinput>,
    simulated: Option<std::os::unix::net::UnixDatagram>,
    screen_width: u32,
    screen_height: u32,
}

struct WatchInterface;

impl input::LibinputInterface for WatchInterface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        use std::os::unix::io::IntoRawFd;
        let file = OpenOptions::new()
            .read(true)
            .write((flags & libc::O_WRONLY != 0) || (flags & libc::O_RDWR != 0))
            .custom_flags(flags & !libc::O_RDWR & !libc::O_WRONLY & !libc::O_RDONLY)
            .open(path)
            .map_err(|e| e.raw_os_error().unwrap_or(-1))?;

        Ok(OwnedFd::from(file))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd);
    }
}

impl InputManager {
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            context: Some(input::Libinput::new_with_udev(WatchInterface)),
            simulated: None,
            screen_width: 4,
            screen_height: 4,
        }
    }

    /// Create input manager using libinput's udev backend.
    pub fn new_from_udev(seat: &str, screen_width: u32, screen_height: u32) -> Result<Self> {
        let interface = WatchInterface;
        let mut context = input::Libinput::new_with_udev(interface);
        context
            .udev_assign_seat(seat)
            .map_err(|_| anyhow::anyhow!("Failed to assign udev seat '{}'", seat))?;
        info!(seat, "Input manager initialized");
        Ok(Self {
            context: Some(context),
            simulated: None,
            screen_width,
            screen_height,
        })
    }

    /// Explicit opt-in: never opens the host's physical input devices.
    pub fn simulated(path: &Path, screen_width: u32, screen_height: u32) -> Result<Self> {
        let socket = std::os::unix::net::UnixDatagram::bind(path)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            context: None,
            simulated: Some(socket),
            screen_width,
            screen_height,
        })
    }

    /// Get the input fd for polling.
    pub fn fd(&self) -> RawFd {
        self.simulated.as_ref().map_or_else(
            || self.context.as_ref().unwrap().as_raw_fd(),
            |s| s.as_raw_fd(),
        )
    }

    /// Get a borrowed fd for epoll registration.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe {
            BorrowedFd::borrow_raw(self.simulated.as_ref().map_or_else(
                || self.context.as_ref().unwrap().as_raw_fd(),
                |s| s.as_raw_fd(),
            ))
        }
    }

    /// Dispatch pending events and return them.
    pub fn dispatch(&mut self) -> Result<Vec<InputEvent>> {
        if let Some(socket) = &self.simulated {
            let mut events = Vec::new();
            let mut buf = [0u8; 512];
            // Bound each dispatch so a producer cannot starve Wayland clients.
            for _ in 0..256 {
                match socket.recv(&mut buf) {
                    Ok(n) => {
                        match parse_simulated(&buf[..n], self.screen_width, self.screen_height) {
                            Some(event) => events.push(event),
                            None => tracing::warn!("Ignoring malformed simulated input"),
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            return Ok(events);
        }
        let context = self.context.as_mut().unwrap();
        context.dispatch().context("libinput dispatch")?;

        let mut events = Vec::new();
        while let Some(event) = context.next() {
            match event {
                input::Event::Touch(te) => {
                    if let Some(ev) = Self::handle_touch(te, self.screen_width, self.screen_height)
                    {
                        events.push(InputEvent::Touch(ev));
                    }
                }
                input::Event::Keyboard(ke) => {
                    if let Some(ev) = Self::handle_keyboard(ke) {
                        events.push(InputEvent::Button(ev));
                    }
                }
                input::Event::Pointer(pe) => {
                    if let Some(ev) = Self::handle_pointer(pe) {
                        events.push(InputEvent::Scroll(ev));
                    }
                }
                _ => {}
            }
        }
        Ok(events)
    }

    fn handle_touch(event: input::event::TouchEvent, sw: u32, sh: u32) -> Option<TouchEvent> {
        use input::event::TouchEvent::*;
        use input::event::touch::TouchEventPosition;

        match event {
            Down(e) => Some(TouchEvent {
                slot: e.slot().unwrap_or(0),
                x: e.x_transformed(sw),
                y: e.y_transformed(sh),
                state: TouchState::Down,
            }),
            Motion(e) => Some(TouchEvent {
                slot: e.slot().unwrap_or(0),
                x: e.x_transformed(sw),
                y: e.y_transformed(sh),
                state: TouchState::Motion,
            }),
            Up(e) => Some(TouchEvent {
                slot: e.slot().unwrap_or(0),
                x: 0.0,
                y: 0.0,
                state: TouchState::Up,
            }),
            Cancel(_) => Some(TouchEvent {
                slot: 0,
                x: 0.0,
                y: 0.0,
                state: TouchState::Cancel,
            }),
            _ => None,
        }
    }

    fn handle_keyboard(event: input::event::KeyboardEvent) -> Option<ButtonEvent> {
        use input::event::KeyboardEvent::*;

        match event {
            Key(e) => Some(ButtonEvent {
                code: e.key(),
                pressed: e.key_state() == input::event::keyboard::KeyState::Pressed,
            }),
            _ => None,
        }
    }

    fn handle_pointer(event: input::event::PointerEvent) -> Option<ScrollEvent> {
        use input::event::PointerEvent::*;

        match event {
            ScrollWheel(e) => {
                use input::event::pointer::PointerScrollEvent;
                let axis = input::event::pointer::Axis::Vertical;
                if e.has_axis(axis) {
                    Some(ScrollEvent {
                        v120: e.scroll_value_v120(axis) as i32,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Small datagram protocol; one event per message, coordinates in display pixels.
fn parse_simulated(bytes: &[u8], width: u32, height: u32) -> Option<InputEvent> {
    let text = std::str::from_utf8(bytes).ok()?;
    let parts: Vec<_> = text.split_whitespace().collect();
    match parts.as_slice() {
        ["touch", state, x, y] => {
            let x: f64 = x.parse().ok()?;
            let y: f64 = y.parse().ok()?;
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            let state = match *state {
                "down" => TouchState::Down,
                "motion" => TouchState::Motion,
                "up" => TouchState::Up,
                "cancel" => TouchState::Cancel,
                _ => return None,
            };
            Some(InputEvent::Touch(TouchEvent {
                slot: 0,
                state,
                x: x.clamp(0.0, width.saturating_sub(1) as f64),
                y: y.clamp(0.0, height.saturating_sub(1) as f64),
            }))
        }
        ["button", code, state] => {
            let code = code.parse().ok()?;
            if !matches!(code, 114 | 115 | 116) {
                return None;
            }
            let pressed = match *state {
                "down" => true,
                "up" => false,
                _ => return None,
            };
            Some(InputEvent::Button(ButtonEvent { code, pressed }))
        }
        ["scroll", amount] => {
            let v120: i32 = amount.parse().ok()?;
            if v120.abs_diff(0) > 12000 {
                return None;
            }
            Some(InputEvent::Scroll(ScrollEvent { v120 }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod simulator_tests {
    use super::*;
    #[test]
    fn validates_injected_events() {
        assert!(parse_simulated(b"touch down NaN 20", 416, 416).is_none());
        assert!(parse_simulated(b"button 1 down", 416, 416).is_none());
        assert!(parse_simulated(b"scroll -2147483648", 416, 416).is_none());
        assert!(parse_simulated(b"button 116 down junk", 416, 416).is_none());
        match parse_simulated(b"touch down -2 999", 416, 416).unwrap() {
            InputEvent::Touch(t) => assert_eq!((t.x, t.y, t.state), (0.0, 415.0, TouchState::Down)),
            _ => panic!("expected touch"),
        }
    }
}
