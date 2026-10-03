//! USB-bus uinput devices are routed exclusively to Nereid's desktop seat.
use anyhow::{Result, ensure};
use evdev::{
    AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventType, InputEvent, InputId,
    KeyCode, RelativeAxisCode as Rel, UinputAbsSetup, uinput::VirtualDevice,
};
use pict_protocol::{PointerSample, WheelSample};
use std::collections::BTreeSet;
pub struct Tablet {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    held: BTreeSet<u16>,
    buttons: u16,
    wheel: [f64; 2],
    pub events: u64,
}
impl Tablet {
    pub async fn create(output: &str) -> Result<Self> {
        ensure!(
            output == "hoki-desktop",
            "Input must target the desktop seat"
        );
        let keys = AttributeSet::from_iter((1..256).map(KeyCode::new));
        let keyboard = VirtualDevice::builder()?
            .name("Pict Nereid desktop keyboard")
            .input_id(InputId::new(BusType::BUS_USB, 1, 1, 1))
            .with_keys(&keys)?
            .build()?;
        let buttons = AttributeSet::from_iter((272..277).map(KeyCode::new));
        let rel = AttributeSet::from_iter([Rel::REL_HWHEEL, Rel::REL_WHEEL]);
        let props = AttributeSet::from_iter([evdev::PropType::POINTER]);
        let pointer = VirtualDevice::builder()?
            .name("Pict Nereid desktop mouse")
            .input_id(InputId::new(BusType::BUS_USB, 1, 1, 1))
            .with_keys(&buttons)?
            .with_relative_axes(&rel)?
            .with_properties(&props)?
            .with_absolute_axis(&UinputAbsSetup::new(
                Abs::ABS_X,
                AbsInfo::new(0, 0, 65535, 0, 0, 0),
            ))?
            .with_absolute_axis(&UinputAbsSetup::new(
                Abs::ABS_Y,
                AbsInfo::new(0, 0, 65535, 0, 0, 0),
            ))?
            .build()?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(Self {
            keyboard,
            pointer,
            held: BTreeSet::new(),
            buttons: 0,
            wheel: [0.0; 2],
            events: 0,
        })
    }
    fn position(&mut self, x: f64, y: f64) -> Result<()> {
        self.pointer.emit(&[
            InputEvent::new(
                EventType::ABSOLUTE.0,
                Abs::ABS_X.0,
                (x * 65535.0).round() as i32,
            ),
            InputEvent::new(
                EventType::ABSOLUTE.0,
                Abs::ABS_Y.0,
                (y * 65535.0).round() as i32,
            ),
        ])?;
        Ok(())
    }
    pub fn event(&mut self, s: &PointerSample) -> Result<()> {
        if s.kind != "mouse" {
            return Ok(());
        }
        if matches!(s.phase.as_str(), "cancel" | "leave" | "lostcapture") {
            return self.release();
        }
        if !s.valid() {
            return Ok(());
        }
        self.events += 1;
        self.position(s.x, s.y)?;
        let mut events = Vec::new();
        for (mask, code) in [(1, 272), (2, 273), (4, 274), (8, 275), (16, 276)] {
            if self.buttons & mask != s.buttons & mask {
                events.push(InputEvent::new(
                    EventType::KEY.0,
                    code,
                    i32::from(s.buttons & mask != 0),
                ));
            }
        }
        if !events.is_empty() {
            self.pointer.emit(&events)?;
        }
        self.buttons = s.buttons;
        Ok(())
    }
    pub fn key(&mut self, code: u16, pressed: bool) -> Result<()> {
        if !pict_protocol::valid_key_code(code) || self.held.contains(&code) == pressed {
            return Ok(());
        }
        self.keyboard
            .emit(&[InputEvent::new(EventType::KEY.0, code, i32::from(pressed))])?;
        if pressed {
            self.held.insert(code);
        } else {
            self.held.remove(&code);
        }
        self.events += 1;
        Ok(())
    }
    pub fn wheel(&mut self, s: &WheelSample) -> Result<()> {
        if !s.valid() {
            return Ok(());
        }
        self.position(s.x, s.y)?;
        let unit = match s.delta_mode {
            1 => 1.0,
            2 => 20.0,
            _ => 1.0 / 15.0,
        };
        for (i, axis, delta, sign) in [
            (0, Rel::REL_HWHEEL, s.delta_x, 1.0),
            (1, Rel::REL_WHEEL, s.delta_y, -1.0),
        ] {
            self.wheel[i] += (delta * unit).clamp(-100.0, 100.0) * sign;
            let ticks = self.wheel[i] as i32;
            self.wheel[i] -= ticks as f64;
            if ticks != 0 {
                self.pointer
                    .emit(&[InputEvent::new(EventType::RELATIVE.0, axis.0, ticks)])?;
            }
        }
        self.events += 1;
        Ok(())
    }
    pub fn release(&mut self) -> Result<()> {
        let keys: Vec<_> = self
            .held
            .iter()
            .map(|c| InputEvent::new(EventType::KEY.0, *c, 0))
            .collect();
        if !keys.is_empty() {
            self.keyboard.emit(&keys)?;
        }
        self.held.clear();
        let buttons: Vec<_> = (272..277)
            .map(|c| InputEvent::new(EventType::KEY.0, c, 0))
            .collect();
        self.pointer.emit(&buttons)?;
        self.buttons = 0;
        self.wheel = [0.0; 2];
        Ok(())
    }
}
impl Drop for Tablet {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
