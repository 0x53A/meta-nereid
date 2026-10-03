//! Nereid platform adapter. Pict owns authentication, sessions and transport.
#[cfg(not(feature = "dev-ssh"))]
mod input;
mod nereid;
#[cfg(not(feature = "dev-ssh"))]
mod output;
mod venus;

use anyhow::Result;
use pict_host::{
    backend::{Backend, Display, DisplayHandle, Input, Task},
    media::{self, CaptureSource, EncodingSettings, VideoSource},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct Nereid;
struct Output {
    cancelled: Arc<AtomicBool>,
    _lease: nereid::Lease,
}
impl DisplayHandle for Output {
    fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl Backend for Nereid {
    fn create(&self, _name: String, width: u32, height: u32) -> Result<Display> {
        let output = Output {
            _lease: nereid::Lease::create(width, height)?,
            cancelled: Arc::default(),
        };
        Ok(Display {
            name: "hoki-desktop".into(),
            source: CaptureSource::Encoded(Arc::new(Venus)),
            handle: Box::new(output),
        })
    }
    fn input<'a>(&'a self, output: &'a str) -> Task<'a, Box<dyn Input>> {
        Box::pin(async move {
            anyhow::ensure!(
                output == "hoki-desktop",
                "Input must target the desktop seat"
            );
            #[cfg(not(feature = "dev-ssh"))]
            {
                Ok(Box::new(input::Tablet::create(output).await?) as Box<dyn Input>)
            }
            #[cfg(feature = "dev-ssh")]
            {
                Ok(Box::new(RemoteInput {
                    input: nereid::Input::new()?,
                    events: 0,
                }) as Box<dyn Input>)
            }
        })
    }
    fn default_encoding(&self) -> Result<EncodingSettings> {
        let mut settings = EncodingSettings::from_env()?;
        if std::env::var_os("PICT_BITRATE_KBPS").is_none() {
            settings.bitrate_kbps = 8000;
        }
        self.validate_encoding(settings)?;
        Ok(settings)
    }
    fn validate_encoding(&self, settings: EncodingSettings) -> Result<()> {
        validate_encoding(settings)
    }
}
fn validate_encoding(settings: EncodingSettings) -> Result<()> {
    settings.validate()?;
    anyhow::ensure!(
        settings.quality == 7 && settings.buffer_ms == 100 && settings.low_power,
        "Venus supports bitrate changes only"
    );
    Ok(())
}
#[derive(Debug)]
struct Venus;
impl VideoSource for Venus {
    fn run(
        &self,
        control: tokio::sync::watch::Receiver<media::Control>,
        output: tokio::sync::mpsc::Sender<media::Encoded>,
        stop: &AtomicBool,
        stats: &media::Stats,
    ) -> Result<()> {
        venus::run(control, output, stop, stats)
    }
    fn validate_encoding(&self, settings: EncodingSettings) -> Result<()> {
        validate_encoding(settings)
    }
}
#[cfg(not(feature = "dev-ssh"))]
impl Input for input::Tablet {
    fn event(&mut self, sample: &pict_protocol::PointerSample) -> Result<()> {
        self.event(sample)
    }
    fn wheel(&mut self, sample: &pict_protocol::WheelSample) -> Result<()> {
        self.wheel(sample)
    }
    fn key(&mut self, code: u16, pressed: bool) -> Result<()> {
        self.key(code, pressed)
    }
    fn release(&mut self) -> Result<()> {
        self.release()
    }
    fn events(&self) -> u64 {
        self.events
    }
}
#[cfg(feature = "dev-ssh")]
struct RemoteInput {
    input: nereid::Input,
    events: u64,
}
#[cfg(feature = "dev-ssh")]
impl Input for RemoteInput {
    fn event(&mut self, s: &pict_protocol::PointerSample) -> Result<()> {
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
        self.input
            .send(serde_json::json!({"type":"pointer", "x":s.x, "y":s.y, "buttons":s.buttons}))
    }
    fn wheel(&mut self, s: &pict_protocol::WheelSample) -> Result<()> {
        if !s.valid() {
            return Ok(());
        }
        self.events += 1;
        self.input
            .send(serde_json::json!({"type":"wheel", "event":s}))
    }
    fn key(&mut self, code: u16, pressed: bool) -> Result<()> {
        if !pict_protocol::valid_key_code(code) {
            return Ok(());
        }
        self.events += 1;
        self.input
            .send(serde_json::json!({"type":"key", "code":code, "pressed":pressed}))
    }
    fn release(&mut self) -> Result<()> {
        self.input.send(serde_json::json!({"type":"release"}))
    }
    fn events(&self) -> u64 {
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertises_only_supported_operations() {
        let caps = Nereid.capabilities();
        for unsupported in ["raw-zstd-v1", "display-settings", "display-preferences"] {
            assert!(!caps.iter().any(|c| c == unsupported));
        }
    }
    #[test]
    fn validates_before_session_persistence() {
        let settings = EncodingSettings::default();
        assert!(Nereid.validate_encoding(settings).is_ok());
        for unsupported in [
            EncodingSettings {
                quality: 1,
                ..settings
            },
            EncodingSettings {
                buffer_ms: 20,
                ..settings
            },
            EncodingSettings {
                low_power: false,
                ..settings
            },
        ] {
            assert!(Nereid.validate_encoding(unsupported).is_err());
        }
    }
}
