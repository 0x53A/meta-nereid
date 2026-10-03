mod brightness;
#[allow(dead_code)]
#[path = "../../shared/brightness_config.rs"]
mod brightness_config;
#[path = "../../shared/sleep_client.rs"]
mod sleep_client;
mod hwc;
mod ambient;
#[allow(dead_code)]
mod protocol;
mod render;
mod shutdown;

use anyhow::{Context, Result};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use tracing::{error, info, warn};

use hwc::{HWC2_POWER_MODE_OFF, HWC2_POWER_MODE_ON};
use protocol::*;

fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    PathBuf::from(dir).join("hwc-proxy.sock")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("hoki-hwc-proxy starting");

    let shutdown = shutdown::Shutdown::new().context("Initialize termination signal FD")?;

    ambient::recover_after_crash().context("Recover previous Sidekick ownership")?;
    // Initialize HWC backend
    let mut hwc = hwc::HwcBackend::new().context("Failed to init HWC backend")?;
    info!(
        width = hwc.info.width,
        height = hwc.info.height,
        "HWC backend initialized"
    );

    // Initialize renderer
    let mut renderer = render::Renderer::new(&hwc).context("Failed to init renderer")?;
    info!("Renderer initialized");

    // Clear to black initially
    renderer.clear(0.0, 0.0, 0.0, 1.0);
    renderer.swap_buffers().context("Initial swap_buffers")?;
    hwc.drain_frame()?;

    // Bind unix socket
    let sock_path = socket_path();
    // Remove stale socket
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("Failed to bind {}", sock_path.display()))?;
    info!(path = %sock_path.display(), "Listening for connections");

    // Set socket permissions (world-accessible for ceres user)
    let _ = std::fs::set_permissions(
        &sock_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    );

    let brightness = brightness::Controller::start();

    // Main accept loop
    listener.set_nonblocking(true).context("set_nonblocking")?;
    while !shutdown.requested() {
        if let Err(e) = wait_ready(listener.as_raw_fd(), libc::POLLIN, Some(shutdown.fd())) {
            if shutdown.requested() {
                break;
            }
            return Err(e).context("Wait for compositor connection");
        }

        info!("Waiting for compositor connection...");

        let (stream, _addr) = match listener.accept() {
            Ok(s) => s,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(e) => {
                error!(?e, "accept() failed");
                continue;
            }
        };

        info!("Compositor connected");

        let client_fd = stream.as_raw_fd();

        // Send INFO message with display dimensions
        let mut info_payload = Vec::with_capacity(8);
        info_payload.extend_from_slice(&hwc.info.width.to_le_bytes());
        info_payload.extend_from_slice(&hwc.info.height.to_le_bytes());
        if let Err(e) = send_raw_cancellable(client_fd, MSG_INFO, &info_payload, shutdown.fd()) {
            error!(?e, "Failed to send INFO");
            continue;
        }

        // Message loop
        handle_client(client_fd, &mut hwc, &mut renderer, &shutdown, &brightness)?;
        if shutdown.requested() {
            break;
        }

        // Client disconnected — clear screen
        info!("Compositor disconnected, clearing screen");
        renderer.clear(0.0, 0.0, 0.0, 1.0);
        renderer.swap_buffers().context("swap_buffers after disconnect")?;
        hwc.drain_frame()?;
    }

    // Clean shutdown
    info!("Shutting down");
    brightness.transition(false, || hwc.set_power_mode(HWC2_POWER_MODE_OFF))?;
    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}

fn handle_client(
    client_fd: i32,
    hwc: &mut hwc::HwcBackend,
    renderer: &mut render::Renderer,
    shutdown: &shutdown::Shutdown,
    brightness: &brightness::Controller,
) -> Result<()> {
    let mut ambient=ambient::Ambient::default();
    // Recover an ambient face left by a crashed prior proxy before accepting frames.
    // Normal per-client disconnect recovery below handles compositor restarts.
    handle_messages(client_fd,hwc,renderer,shutdown,&mut ambient,brightness);
    brightness.transition(true, || ambient.restore(hwc)).context("display recovery failed")
}

fn handle_messages(client_fd:i32,hwc:&mut hwc::HwcBackend,renderer:&mut render::Renderer,
    shutdown:&shutdown::Shutdown,ambient:&mut ambient::Ambient,brightness:&brightness::Controller) {
    while !shutdown.requested() {
        let (msg_type, payload, fd) = match recv_fd_with_cancel(client_fd, Some(shutdown.fd())) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if shutdown.requested() {
                    return;
                }
                continue;
            }
            Err(e) => {
                error!(?e, "recv_fd failed");
                return;
            }
        };

        match msg_type {
            MSG_DISPLAY => {
                let result=(|| -> Result<()> {
                    let mode=*payload.first().ok_or_else(||anyhow::anyhow!("missing display mode"))?;
                    if !matches!(mode,0|2|3) {anyhow::bail!("invalid display mode")}
                    let face=std::str::from_utf8(&payload[1..])?;
                    brightness.transition(mode == 2, || ambient.change(mode,face,hwc,renderer))
                })();
                let response=match result {Ok(())=>vec![0],Err(e)=>{let mut b=vec![1];b.extend_from_slice(e.to_string().as_bytes());b}};
                if send_raw_cancellable(client_fd,MSG_DISPLAY_RESULT,&response,shutdown.fd()).is_err(){return;}
            }
            MSG_FRAME => {
                if ambient.active {error!("frame while Sidekick owns display");return;}
                if let Err(e) = handle_frame(&payload, fd, renderer, hwc, client_fd, shutdown.fd()) {
                    error!(?e, "FRAME handling failed");
                    return;
                }
            }
            MSG_POWER => {
                if payload.is_empty() {
                    warn!("POWER message with empty payload");
                    continue;
                }
                let mode = match payload[0] {
                    0 => HWC2_POWER_MODE_OFF,
                    _ => HWC2_POWER_MODE_ON,
                };
                info!(mode, "Setting display power mode");
                if brightness.transition(mode == HWC2_POWER_MODE_ON, || ambient.restore(hwc).and_then(|_| hwc.set_power_mode(mode))).is_err(){return;}
            }
            MSG_PING => {
                if let Err(e) = send_raw_cancellable(client_fd, MSG_PONG, &[], shutdown.fd()) {
                    error!(?e, "Failed to send PONG");
                    return;
                }
            }
            other => {
                warn!(msg_type = other, "Unknown message type");
            }
        }
    }
}

fn handle_frame(
    payload: &[u8],
    fd: Option<OwnedFd>,
    renderer: &mut render::Renderer,
    hwc: &mut hwc::HwcBackend,
    client_fd: i32,
    cancel_fd: i32,
) -> Result<()> {
    // Parse payload: u32 width, u32 height, u32 stride
    if payload.len() < 12 {
        anyhow::bail!("FRAME payload too short: {} bytes", payload.len());
    }
    let width = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    let height = u32::from_le_bytes(payload[4..8].try_into().unwrap());
    let stride = u32::from_le_bytes(payload[8..12].try_into().unwrap());

    let memfd = fd.ok_or_else(|| anyhow::anyhow!("FRAME without memfd"))?;

    // mmap the memfd
    let data_len = stride
        .checked_mul(height)
        .map(|n| n as usize)
        .filter(|_| width == renderer.width && height == renderer.height && stride == width * 4)
        .ok_or_else(|| anyhow::anyhow!("invalid frame dimensions or stride"))?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(memfd.as_raw_fd(), &mut stat) } != 0
        || (stat.st_size as i128) < data_len as i128
    {
        anyhow::bail!("frame descriptor is shorter than its dimensions");
    }
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            data_len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            memfd.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        anyhow::bail!("mmap failed: {}", std::io::Error::last_os_error());
    }

    // Upload + draw + swap
    let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, data_len) };
    renderer.upload(width, height, stride, data);
    renderer.draw();
    let swap_result = renderer.swap_buffers();

    // Cleanup: munmap + close fd
    unsafe {
        libc::munmap(ptr, data_len);
    }

    swap_result.context("swap_buffers in FRAME handler")?;
    hwc.drain_frame()?;

    // Send SYNC back (direct write, no dup/drop dance)
    send_raw_cancellable(client_fd, MSG_SYNC, &[], cancel_fd)?;

    Ok(())
}
