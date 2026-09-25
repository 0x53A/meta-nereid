//! HWC proxy client — connects to hoki-hwc-proxy via unix socket.
//!
//! The proxy owns the HWC2/EGL/GLES2 stack. The compositor sends pre-composed
//! RGBA framebuffers via memfd and receives SYNC when presentation completes.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::info;

#[path = "../../shared/hwc_protocol.rs"]
mod protocol;
use protocol::*;

pub struct ProxyClient {
    stream: UnixStream,
    pub display_width: u32,
    pub display_height: u32,
}

impl ProxyClient {
    #[cfg(test)]
    pub fn for_test(stream: UnixStream) -> Self {
        Self {
            stream,
            display_width: 4,
            display_height: 4,
        }
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    pub fn connect() -> Result<Self> {
        let sock_path = PathBuf::from(
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into()),
        )
        .join("hwc-proxy.sock");

        info!(path = %sock_path.display(), "Connecting to HWC proxy");
        let stream = UnixStream::connect(&sock_path)
            .with_context(|| format!("Failed to connect to {}", sock_path.display()))?;

        // Set a read timeout so a lost SYNC can't deadlock the compositor forever
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;

        // Receive INFO message with display dimensions
        let (msg_type, payload, _fd) = recv_fd(stream.as_raw_fd())?;
        if msg_type != MSG_INFO {
            anyhow::bail!("Expected INFO (0x83), got 0x{:02x}", msg_type);
        }
        if payload.len() < 8 {
            anyhow::bail!("INFO payload too short: {} bytes", payload.len());
        }

        let width = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(payload[4..8].try_into().unwrap());
        info!(width, height, "Connected to HWC proxy");

        Ok(Self {
            stream,
            display_width: width,
            display_height: height,
        })
    }

    /// Send a composited frame to the proxy and wait for presentation.
    /// The kernel duplicates the sent descriptor; caller retains ownership.
    pub fn send_frame(&self, memfd_fd: RawFd, width: u32, height: u32, stride: u32) -> Result<()> {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&stride.to_le_bytes());

        send_fd(self.stream.as_raw_fd(), MSG_FRAME, &payload, memfd_fd)?;

        // Wait for SYNC (blocks until frame is presented, 5s timeout)
        let (msg_type, _, _) =
            recv_fd(self.stream.as_raw_fd()).context("waiting for SYNC from proxy (timeout?)")?;
        if msg_type != MSG_SYNC {
            anyhow::bail!("Expected SYNC (0x81), got 0x{:02x}", msg_type);
        }

        Ok(())
    }

    /// Set display power state via proxy.
    pub fn set_power(&mut self, on: bool) -> Result<()> {
        let mode: u8 = if on { 2 } else { 0 };
        send_raw(self.stream.as_raw_fd(), MSG_POWER, &[mode])
            .map_err(|e| anyhow::anyhow!("send POWER: {}", e))
    }
}
