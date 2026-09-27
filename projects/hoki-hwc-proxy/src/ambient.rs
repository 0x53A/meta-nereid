//! Display ownership transaction. Keep this in the process that owns HWC.
use crate::{hwc::HwcBackend, render::Renderer};
use anyhow::{bail, Result};
use std::os::unix::process::CommandExt;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn helper(operation: &str, face: Option<&str>) -> Result<()> {
    let mut command = Command::new("/usr/lib/hoki-lp-watchface");
    command
        .args(["managed", operation])
        .stdin(Stdio::null())
        .uid(1000)
        .gid(1000)
        .env("XDG_RUNTIME_DIR", "/run/user/1000");
    if let Some(face) = face {
        command.arg(face);
    }
    let mut child = command.spawn()?;
    let until = Instant::now() + Duration::from_secs(9);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("Sidekick {operation}: {status}")
            }
            return Ok(());
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Sidekick {operation} timed out")
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const MARKER: &str = "/run/hoki-hwc-proxy/ambient.pending";
pub fn recover_after_crash() -> Result<()> {
    if std::path::Path::new(MARKER).exists() {
        helper("exit", None)?;
        std::fs::remove_file(MARKER)?;
    }
    Ok(())
}
#[derive(Default)]
pub struct Ambient {
    pub active: bool,
}
impl Ambient {
    pub fn restore(&mut self, hwc: &HwcBackend) -> Result<()> {
        if self.active {
            helper("exit", None)?;
            self.active = false;
            match std::fs::remove_file(MARKER) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        hwc.set_power_mode(2)
    }
    pub fn change(
        &mut self,
        mode: u8,
        face: &str,
        hwc: &mut HwcBackend,
        renderer: &mut Renderer,
    ) -> Result<()> {
        if mode != 3 {
            self.restore(hwc)?;
            return hwc.set_power_mode(if mode == 0 { 0 } else { 2 });
        }
        if self.active {
            self.restore(hwc)?;
        }
        // Mark uncertain ownership before the first vendor call, so every
        // partial upload/entry failure follows the same recovery path.
        std::fs::create_dir_all("/run/hoki-hwc-proxy")?;
        std::fs::write(MARKER, b"ambient ownership may be active\n")?;
        self.active = true;
        let result = (|| {
            helper("prepare", Some(face))?;
            renderer.clear(0., 0., 0., 1.);
            renderer.swap_buffers()?;
            hwc.drain_frame()?;
            hwc.set_power_mode(3)?;
            helper("enter", None)
        })();
        if let Err(error) = result {
            let recovery = self.restore(hwc);
            bail!("ambient handoff: {error}; recovery: {recovery:?}")
        }
        Ok(())
    }
}
