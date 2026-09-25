//! The only native audio boundary. All decoding and PCM gain happens in Rust.
use anyhow::{bail, Context as _, Result};
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::{
    context::{Context, FlagSet as ContextFlags, State as ContextState},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    operation,
    sample::{Format, Spec},
    stream::{FlagSet, SeekMode, State, Stream},
};
use std::time::{Duration, Instant};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub struct Output {
    stream: Stream,
    context: Context,
    mainloop: Mainloop,
    sink: Rc<RefCell<Option<String>>>,
    changed: Rc<Cell<bool>>,
    lookup: Rc<Cell<bool>>,
    route_failed: Rc<Cell<bool>>,
    routed_to: String,
    pub rate: u32,
    pub channels: u8,
}
impl Output {
    pub fn new(rate: u32, channels: u8) -> Result<Self> {
        let mut mainloop = Mainloop::new().context("Cannot create PulseAudio loop")?;
        let mut context =
            Context::new(&mainloop, "hoki-music").context("Cannot create PulseAudio client")?;
        context
            .connect(None, ContextFlags::NOAUTOSPAWN, None)
            .map_err(|e| anyhow::anyhow!("PulseAudio connection: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump(&mut mainloop)?;
            match context.get_state() {
                ContextState::Ready => break,
                ContextState::Failed | ContextState::Terminated => {
                    bail!("PulseAudio is unavailable")
                }
                _ if Instant::now() > deadline => bail!("PulseAudio connection timed out"),
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let sink = Rc::new(RefCell::new(None::<String>));
        let got = sink.clone();
        context.introspect().get_server_info(move |info| {
            *got.borrow_mut() = info.default_sink_name.as_ref().map(|s| s.to_string());
        });
        while sink.borrow().is_none() {
            pump(&mut mainloop)?;
            if Instant::now() > deadline {
                bail!("No default audio output available");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let routed_to = sink.borrow_mut().take().unwrap();
        let changed = Rc::new(Cell::new(false));
        let events = changed.clone();
        context.set_subscribe_callback(Some(Box::new(move |_, _, _| events.set(true))));
        context.subscribe(InterestMaskSet::SERVER, |_| {});
        let spec = Spec {
            format: Format::FLOAT32NE,
            rate,
            channels,
        };
        if !spec.is_valid() {
            bail!("Invalid audio format");
        }
        let mut stream = Stream::new(&mut context, "Music", &spec, None)
            .context("Cannot create music output")?;
        let bytes_per_second = rate * channels as u32 * 4;
        let attr = BufferAttr {
            maxlength: bytes_per_second,
            tlength: bytes_per_second / 4,
            prebuf: u32::MAX,
            minreq: bytes_per_second / 20,
            fragsize: u32::MAX,
        };
        stream
            .connect_playback(
                Some(&routed_to),
                Some(&attr),
                FlagSet::ADJUST_LATENCY | FlagSet::AUTO_TIMING_UPDATE | FlagSet::INTERPOLATE_TIMING,
                None,
                None,
            )
            .map_err(|e| anyhow::anyhow!("Cannot connect music output: {e}"))?;
        let mut output = Self {
            stream,
            context,
            mainloop,
            rate,
            channels,
            sink,
            changed,
            lookup: Rc::new(Cell::new(false)),
            route_failed: Rc::new(Cell::new(false)),
            routed_to,
        };
        while output.stream.get_state() != State::Ready {
            output.pump()?;
            if Instant::now() > deadline {
                bail!("Audio output timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(output)
    }
    pub fn pump(&mut self) -> Result<()> {
        pump(&mut self.mainloop)?;
        if matches!(self.stream.get_state(), State::Failed | State::Terminated)
            || matches!(
                self.context.get_state(),
                ContextState::Failed | ContextState::Terminated
            )
        {
            bail!("Audio output disconnected");
        }
        if self.route_failed.replace(false) {
            bail!("Could not switch to the selected audio output");
        }
        if self.changed.get() && !self.lookup.get() {
            self.changed.set(false);
            self.lookup.set(true);
            let sink = self.sink.clone();
            let lookup = self.lookup.clone();
            self.context.introspect().get_server_info(move |info| {
                *sink.borrow_mut() = info.default_sink_name.as_ref().map(|s| s.to_string());
                lookup.set(false);
            });
        }
        if self.stream.get_state() == State::Ready {
            if let Some(sink) = self.sink.borrow_mut().take() {
                if sink != self.routed_to {
                    if let Some(index) = self.stream.get_index() {
                        let failed = self.route_failed.clone();
                        self.context.introspect().move_sink_input_by_name(
                            index,
                            &sink,
                            Some(Box::new(move |ok| failed.set(!ok))),
                        );
                        self.routed_to = sink;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn write(&mut self, samples: &[f32], volume: u8) -> Result<usize> {
        self.pump()?;
        let capacity = self.stream.writable_size().context("Output unavailable")? / 4;
        let count = samples.len().min(capacity) / self.channels as usize * self.channels as usize;
        if count == 0 {
            return Ok(0);
        }
        let gain = (volume.min(100) as f32 / 100.0).powi(2);
        let mut bytes = Vec::with_capacity(count * 4);
        for sample in &samples[..count] {
            bytes.extend_from_slice(
                &(if sample.is_finite() {
                    (sample * gain).clamp(-1.0, 1.0)
                } else {
                    0.0
                })
                .to_ne_bytes(),
            );
        }
        self.stream
            .write_copy(&bytes, 0, SeekMode::Relative)
            .map_err(|e| anyhow::anyhow!("Audio write failed: {e}"))?;
        Ok(count)
    }
    pub fn pause(&mut self, pause: bool) -> Result<()> {
        let operation = if pause {
            self.stream.cork(None)
        } else {
            self.stream.uncork(None)
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while operation.get_state() == operation::State::Running {
            self.pump()?;
            if Instant::now() > deadline {
                bail!("Audio pause timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    pub fn latency(&self) -> f64 {
        match self.stream.get_latency() {
            Ok(libpulse_binding::stream::Latency::Positive(t)) => t.0 as f64 / 1e6,
            _ => 0.0,
        }
    }
    pub fn start_drain(&mut self) -> operation::Operation<dyn FnMut(bool)> {
        self.stream.drain(None)
    }
}
fn pump(mainloop: &mut Mainloop) -> Result<()> {
    match mainloop.iterate(false) {
        IterateResult::Err(_) | IterateResult::Quit(_) => bail!("PulseAudio loop stopped"),
        _ => Ok(()),
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
        self.context.disconnect();
    }
}
