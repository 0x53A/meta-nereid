// SPDX-License-Identifier: GPL-3.0-only
use slint::ComponentHandle;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
slint::include_modules!();

unsafe extern "C" {
    fn hap_open() -> i32;
    fn hap_upload(fd: i32, id: *mut i32, preset: i32, strength: i32, ms: i32) -> i32;
    fn hap_play(fd: i32, id: i32, on: i32) -> i32;
    fn hap_close(fd: i32, id: i32) -> i32;
}

fn check(value: i32) -> Result<i32, String> {
    if value < 0 {
        Err(std::io::Error::from_raw_os_error(-value).to_string())
    } else {
        Ok(value)
    }
}

struct Device {
    fd: i32,
    id: i32,
}
impl Drop for Device {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // Fallback cleanup on early return; ordinary completion reports errors.
            unsafe {
                hap_close(self.fd, self.id);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Request {
    preset: i32,
    strength: i32,
    rate: i32,
}
impl Request {
    fn validate(self) -> Result<Self, String> {
        if (-1..=5).contains(&self.preset)
            && [35, 65, 100].contains(&self.strength)
            && [1, 2, 4].contains(&self.rate)
        {
            Ok(self)
        } else {
            Err("Unsupported pattern".into())
        }
    }
    fn count(self) -> i32 {
        if self.preset < 0 {
            self.rate
        } else {
            1
        }
    }
}

fn wait_until(deadline: Instant, cancel: &AtomicBool) -> bool {
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    !cancel.load(Ordering::Relaxed)
}

fn play(request: Request, cancel: &AtomicBool, preview: bool) -> Result<(), String> {
    let request = request.validate()?;
    if preview {
        wait_until(Instant::now() + Duration::from_millis(500), cancel);
        return Ok(());
    }
    let mut dev = Device {
        fd: check(unsafe { hap_open() })?,
        id: -1,
    };
    let result = (|| {
        for pulse in 0..request.count() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            // One slot, freshly uploaded before every play. No effect cache or FF_GAIN.
            let ms = check(unsafe {
                hap_upload(dev.fd, &mut dev.id, request.preset, request.strength, 80)
            })?;
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let start = Instant::now();
            check(unsafe { hap_play(dev.fd, dev.id, 1) })?;
            let deadline = start + Duration::from_millis(ms as u64 + 15);
            let finished = wait_until(deadline, cancel);
            let stopped = check(unsafe { hap_play(dev.fd, dev.id, 0) });
            // This driver does not cancel its constant stop timer on early stop.
            // Drain the original interval before erase or another upload.
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
            stopped?;
            if !finished {
                break;
            }
            if pulse + 1 < request.count()
                && !wait_until(
                    start + Duration::from_millis(1000 / request.rate as u64),
                    cancel,
                )
            {
                break;
            }
        }
        Ok(())
    })();
    let cleanup = check(unsafe { hap_close(dev.fd, dev.id) }).map(|_| ());
    dev.fd = -1;
    // Let the driver's delayed module-disable complete before another session.
    thread::sleep(Duration::from_millis(30));
    result.and(cleanup)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let preview = args.iter().any(|a| a == "--preview" || a == "--capture");
    let window = MainWindow::new()?;
    if preview {
        window.set_status("Preview · vibration disabled".into());
    }
    if args.iter().any(|a| a == "--effects") {
        window.set_presets(true);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<Request>(1);
    let (done_tx, done_rx) = mpsc::channel();
    let worker_cancel = cancel.clone();
    let worker = thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            let result = play(request, &worker_cancel, preview);
            let status = match result {
                Err(error) => {
                    eprintln!("Haptics: {error}");
                    format!("Unavailable: {error}")
                }
                Ok(()) if preview => "Preview · vibration disabled".into(),
                Ok(()) if worker_cancel.load(Ordering::Relaxed) => "Stopped".into(),
                Ok(()) => "Pattern sent · did you feel it?".into(),
            };
            if done_tx.send(status).is_err() {
                break;
            }
        }
    });
    let weak = window.as_weak();
    let send = tx.clone();
    let start_cancel = cancel.clone();
    window.on_play(move |preset, strength, rate| {
        if let Some(w) = weak.upgrade() {
            if w.get_busy() {
                return;
            }
            start_cancel.store(false, Ordering::Relaxed);
            if send
                .try_send(Request {
                    preset,
                    strength,
                    rate,
                })
                .is_ok()
            {
                w.set_busy(true);
                w.set_status(
                    if preset < 0 {
                        format!("{rate} pulses/s · {strength}% · 80 ms")
                    } else {
                        format!("Effect {preset} · {strength}%")
                    }
                    .into(),
                );
            }
        }
    });
    let stop_cancel = cancel.clone();
    window.on_stop(move || stop_cancel.store(true, Ordering::Relaxed));
    window.on_close_app(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = window.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(30),
        move || {
            if let Ok(status) = done_rx.try_recv() {
                if let Some(w) = weak.upgrade() {
                    w.set_busy(false);
                    w.set_status(status.into());
                }
            }
        },
    );
    #[cfg(feature = "capture")]
    let capture_timer = slint::Timer::default();
    #[cfg(feature = "capture")]
    if let Some(path) = args
        .iter()
        .position(|a| a == "--capture")
        .and_then(|i| args.get(i + 1))
        .cloned()
    {
        let weak = window.as_weak();
        capture_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(1),
            move || {
                let w = weak.upgrade().unwrap();
                let pixels = w.window().take_snapshot().expect("capture renderer");
                image::save_buffer(
                    &path,
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .expect("save capture");
                let _ = slint::quit_event_loop();
            },
        );
    }
    let result = window.run();
    cancel.store(true, Ordering::Relaxed);
    // Release the callback's sender before joining the worker.
    window.on_play(|_, _, _| {});
    drop(tx);
    let _ = worker.join();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_has_bounded_duty_cycle_and_time_for_timer_to_expire() {
        for rate in [1, 2, 4] {
            let r = Request {
                preset: -1,
                strength: 65,
                rate,
            }
            .validate()
            .unwrap();
            assert!(80 + 15 < 1000 / rate);
            assert!(r.count() * 80 <= 320);
        }
    }
    #[test]
    fn invalid_requests_never_reach_hardware() {
        for r in [
            Request {
                preset: 6,
                strength: 65,
                rate: 1,
            },
            Request {
                preset: -1,
                strength: 0,
                rate: 1,
            },
            Request {
                preset: -1,
                strength: 100,
                rate: 100,
            },
        ] {
            assert!(r.validate().is_err());
        }
    }
    #[test]
    fn cancellation_interrupts_wait() {
        assert!(!wait_until(
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(true)
        ));
    }
}
