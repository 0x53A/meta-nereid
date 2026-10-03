//! Native capture/convert/Venus worker, local on watch or remote over SSH.
//! One request in flight locally; two over SSH to overlap RTT. Both are bounded
//! by transport demand, and stale dependent chains wait for a new IDR.
use crate::nereid::{command, nonblocking, read, write};
use anyhow::{Result, ensure};
use pict_host::media::{Control, Encoded, Packet, Stats};
use std::{
    collections::VecDeque,
    io::Read,
    os::fd::AsRawFd,
    process::{Child, ChildStdin, ChildStdout},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

struct Remote {
    child: Child,
    input: Option<ChildStdin>,
    stream: ChildStdout,
}
impl Drop for Remote {
    fn drop(&mut self) {
        // EOF first, then drain pending compressed frames while the remote worker
        // flushes Venus. Keep stdout open so SSH cannot die on a premature EPIPE.
        self.input.take();
        if nonblocking(self.stream.as_raw_fd()).is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut discard = [0u8; 16384];
        while Instant::now() < deadline {
            while self.stream.read(&mut discard).is_ok_and(|n| n > 0) {}
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(crate) fn run(
    mut control: watch::Receiver<Control>,
    output: mpsc::Sender<Encoded>,
    stop: &AtomicBool,
    stats: &Stats,
) -> Result<()> {
    let mut child = command("venus")?.spawn()?;
    let input = child.stdin.take();
    let stream = child.stdout.take().unwrap();
    let mut process = Remote {
        child,
        input,
        stream,
    };
    nonblocking(process.input.as_ref().unwrap().as_raw_fd())?;
    nonblocking(process.stream.as_raw_fd())?;
    *stats.encoder.lock().unwrap() = "h264_venus (watch)".into();
    *stats.capture_path.lock().unwrap() = if cfg!(feature = "dev-ssh") {
        "Nereid SHM -> NV12 -> Venus; H.264 over SSH"
    } else {
        "Nereid SHM -> NV12 -> Venus; local WebRTC"
    }
    .into();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut requested = u64::MAX;
            let mut previous = None;
            let mut pending: VecDeque<(Instant, u32)> = VecDeque::new();
            let mut recover = true;
            while !stop.load(Ordering::Acquire) {
                let settings = *control.borrow_and_update();
                if !settings.connected {
                    recover = true;
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                let slot = tokio::select! {
                    s=output.reserve()=>match s { Ok(s)=>s, Err(_)=>break },
                    _=tokio::time::sleep(Duration::from_millis(20))=>continue,
                };
                let settings = *control.borrow_and_update();
                if !settings.connected || stop.load(Ordering::Acquire) {
                    continue;
                }
                // One read-ahead request overlaps SSH RTT/encoding with the current
                // frame. Fill only while a transport slot is available, never unbounded.
                while pending.len() < if cfg!(feature = "dev-ssh") { 2 } else { 1 } {
                    let force = recover || requested != settings.keyframe;
                    let bitrate = (settings.bitrate as u32).clamp(500_000, 100_000_000);
                    let mut request = [0u8; 8];
                    request[..4].copy_from_slice(&(force as u32).to_le_bytes());
                    request[4..].copy_from_slice(&bitrate.to_le_bytes());
                    let submitted = Instant::now();
                    write(process.input.as_mut().unwrap(), &request, stop)?;
                    pending.push_back((submitted, bitrate));
                    requested = settings.keyframe;
                }
                let (submitted, bitrate) = pending.pop_front().unwrap();
                let mut header = [0u8; 40];
                read(&mut process.stream, &mut header, stop)?;
                let h: Vec<u32> = header
                    .chunks_exact(4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                ensure!(
                    (1..=4 * 1024 * 1024).contains(&h[0])
                        && h[1] <= 1
                        && (96..=1920).contains(&h[2])
                        && (64..=1088).contains(&h[3]),
                    "Invalid Venus packet header"
                );
                let mut bytes = vec![0; h[0] as usize];
                read(&mut process.stream, &mut bytes, stop)?;
                let finished = Instant::now();
                stats.captured.fetch_add(1, Ordering::Relaxed);
                stats.encoded.fetch_add(1, Ordering::Relaxed);
                stats.conversion_us.store(h[5] as u64, Ordering::Relaxed);
                stats.encode_us.store(h[6] as u64, Ordering::Relaxed);
                stats.bitrate.store(bitrate as u64, Ordering::Relaxed);
                if let Some(last) = previous {
                    stats.capture_interval_us.store(
                        finished.duration_since(last).as_micros() as u64,
                        Ordering::Relaxed,
                    );
                }
                previous = Some(finished);
                if submitted.elapsed() > Duration::from_millis(150) {
                    recover = true;
                    continue;
                }
                if recover && h[1] == 0 {
                    continue;
                }
                recover = false;
                slot.send(Encoded {
                    packet: Packet {
                        data: bytes.into(),
                        keyframe: h[1] != 0,
                    },
                    pts: ((h[8] as u64 | ((h[9] as u64) << 32)) * 90 / 1000),
                    captured: submitted,
                    submitted,
                    finished,
                });
            }
            Ok(())
        })
}
