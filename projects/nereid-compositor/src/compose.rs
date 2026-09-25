//! Software compositor — blits Wayland surface buffers into a memfd framebuffer.
//!
//! Replaces the GLES2 renderer. The proxy handles GPU upload and presentation.

use std::ffi::c_void;
use std::os::unix::io::RawFd;

use anyhow::Result;
use smithay::wayland::shell::wlr_layer::Layer;

use crate::ShellMode;
use crate::wayland::{LayerEntry, SurfaceBuffer};

/// Memfd-backed framebuffer for compositing.
pub struct MemfdBuffer {
    pub fd: RawFd,
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for MemfdBuffer {}

impl MemfdBuffer {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let len = (width * height * 4) as usize;

        let fd = unsafe {
            libc::memfd_create(
                b"frame\0".as_ptr() as *const libc::c_char,
                libc::MFD_CLOEXEC,
            )
        };
        if fd < 0 {
            anyhow::bail!("memfd_create: {}", std::io::Error::last_os_error());
        }

        if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
            unsafe { libc::close(fd) };
            anyhow::bail!("ftruncate: {}", std::io::Error::last_os_error());
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            anyhow::bail!("mmap: {}", std::io::Error::last_os_error());
        }

        Ok(Self {
            fd,
            ptr: ptr as *mut u8,
            len,
        })
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for MemfdBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// Composite all visible surfaces into the framebuffer (RGBA8888).
pub fn composite_frame(
    dest: &mut [u8],
    width: u32,
    height: u32,
    watchface_buf: Option<&SurfaceBuffer>,
    launcher_buf: Option<&SurfaceBuffer>,
    settings_buf: Option<&SurfaceBuffer>,
    toplevel_buf: Option<&SurfaceBuffer>,
    layer_surfaces: &[LayerEntry],
    shell_mode: ShellMode,
) {
    // Clear to black
    dest.fill(0);

    // 1. Background/Bottom layers
    for entry in layer_surfaces {
        if entry.visible
            && entry.has_content
            && matches!(entry.layer, Layer::Background | Layer::Bottom)
        {
            if let Some(ref buf) = entry.pending_buffer {
                blit_opaque(dest, width, height, buf);
            }
        }
    }

    // 2. Main content based on active mode
    match shell_mode {
        ShellMode::Watchface => {
            if let Some(buf) = watchface_buf {
                blit_opaque(dest, width, height, buf);
            }
        }
        ShellMode::Launcher => {
            if let Some(buf) = launcher_buf {
                blit_opaque(dest, width, height, buf);
            }
        }
        ShellMode::Settings => {
            if let Some(buf) = settings_buf {
                blit_opaque(dest, width, height, buf);
            }
        }
        ShellMode::App => {
            if let Some(buf) = toplevel_buf {
                blit_opaque(dest, width, height, buf);
            }
        }
    }

    // 3. Top/Overlay layers (with alpha blending)
    for entry in layer_surfaces {
        if entry.visible && entry.has_content && matches!(entry.layer, Layer::Top | Layer::Overlay)
        {
            if let Some(ref buf) = entry.pending_buffer {
                blit_alpha(dest, width, height, buf);
            }
        }
    }
}

/// Blit a surface buffer opaquely (no alpha blending). Handles swap_rb.
fn blit_opaque(dest: &mut [u8], dest_w: u32, dest_h: u32, src: &SurfaceBuffer) {
    let w = src.width.min(dest_w);
    let h = src.height.min(dest_h);
    let dest_stride = dest_w * 4;

    for row in 0..h {
        let src_start = (row * src.stride) as usize;
        let dest_start = (row * dest_stride) as usize;

        if src.swap_rb {
            // XRGB8888/ARGB8888: memory is B,G,R,X → output R,G,B,0xFF
            for col in 0..w {
                let si = src_start + (col * 4) as usize;
                let di = dest_start + (col * 4) as usize;
                if si + 3 < src.data.len() && di + 3 < dest.len() {
                    dest[di] = src.data[si + 2]; // R
                    dest[di + 1] = src.data[si + 1]; // G
                    dest[di + 2] = src.data[si]; // B
                    dest[di + 3] = 0xFF; // A
                }
            }
        } else {
            // RGBA8888: direct copy
            let row_bytes = (w * 4) as usize;
            let src_end = src_start + row_bytes;
            let dest_end = dest_start + row_bytes;
            if src_end <= src.data.len() && dest_end <= dest.len() {
                dest[dest_start..dest_end].copy_from_slice(&src.data[src_start..src_end]);
            }
        }
    }
}

/// Blit a surface buffer with per-pixel alpha blending. Handles swap_rb.
fn blit_alpha(dest: &mut [u8], dest_w: u32, dest_h: u32, src: &SurfaceBuffer) {
    let w = src.width.min(dest_w);
    let h = src.height.min(dest_h);
    let dest_stride = dest_w * 4;

    for row in 0..h {
        let src_start = (row * src.stride) as usize;
        let dest_start = (row * dest_stride) as usize;

        for col in 0..w {
            let si = src_start + (col * 4) as usize;
            let di = dest_start + (col * 4) as usize;
            if si + 3 >= src.data.len() || di + 3 >= dest.len() {
                continue;
            }

            let (sr, sg, sb, mut sa) = if src.swap_rb {
                // ARGB8888: memory is B,G,R,A
                (
                    src.data[si + 2],
                    src.data[si + 1],
                    src.data[si],
                    src.data[si + 3],
                )
            } else {
                (
                    src.data[si],
                    src.data[si + 1],
                    src.data[si + 2],
                    src.data[si + 3],
                )
            };

            if !src.has_alpha {
                sa = 255;
            }
            if sa == 0 {
                continue;
            } else if sa == 255 {
                dest[di] = sr;
                dest[di + 1] = sg;
                dest[di + 2] = sb;
                dest[di + 3] = 0xFF;
            } else {
                let inv_a = 255 - sa as u16;
                dest[di] = (sr as u16 + dest[di] as u16 * inv_a / 255).min(255) as u8;
                dest[di + 1] = (sg as u16 + dest[di + 1] as u16 * inv_a / 255).min(255) as u8;
                dest[di + 2] = (sb as u16 + dest[di + 2] as u16 * inv_a / 255).min(255) as u8;
                dest[di + 3] = 0xFF;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn premultiplied_and_opaque_overlay_pixels() {
        let mut src = SurfaceBuffer {
            data: vec![0, 0, 128, 128],
            width: 1,
            height: 1,
            stride: 4,
            swap_rb: true,
            has_alpha: true,
        };
        let mut dest = [0, 0, 0, 255];
        blit_alpha(&mut dest, 1, 1, &src);
        assert_eq!(dest, [128, 0, 0, 255]);
        src.data = vec![0, 0, 255, 0];
        src.has_alpha = false;
        blit_alpha(&mut dest, 1, 1, &src);
        assert_eq!(dest, [255, 0, 0, 255]);
    }
}
