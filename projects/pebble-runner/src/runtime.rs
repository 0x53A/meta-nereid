use crate::gcolor;
use crate::pbw;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU8, Ordering};

/// Pebble Chalk display: 180x180, 8-bit color (GColor8 ARGB2222)
pub const DISPLAY_WIDTH: usize = 180;
pub const DISPLAY_HEIGHT: usize = 180;

// Pebble Chalk's compact 8BitCircular framebuffer row bounds, from the
// Spalding display table in pebble-os/src/fw/board/displays/display_spalding.c.
const CHALK_ROW_MIN: [u8; 90] = [
    76, 71, 66, 63, 60, 57, 55, 52, 50, 48, 46, 45, 43, 41, 40, 38, 37, 36,
    34, 33, 32, 31, 29, 28, 27, 26, 25, 24, 23, 22, 22, 21, 20, 19, 18, 18,
    17, 16, 15, 15, 14, 13, 13, 12, 12, 11, 10, 10, 9, 9, 8, 8, 7, 7,
    7, 6, 6, 5, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

pub fn chalk_row_info(y: usize) -> (u16, u8, u8) {
    let mut start = 76usize;
    for row in 0..y {
        let min = CHALK_ROW_MIN[row.min(179 - row)] as usize;
        start += 180 - 2 * min;
    }
    let min = CHALK_ROW_MIN[y.min(179 - y)];
    ((start - min as usize) as u16, min, 179 - min)
}

#[cfg(test)]
mod chalk_framebuffer_tests {
    use super::chalk_row_info;

    #[test]
    fn compact_row_map_matches_pebble_chalk_boundaries_and_size() {
        assert_eq!(chalk_row_info(0), (0, 76, 103));
        assert_eq!(chalk_row_info(1), (33, 71, 108));
        assert_eq!(chalk_row_info(179), (25764, 76, 103));
        let (offset, _, max) = chalk_row_info(179);
        assert_eq!(offset as usize + max as usize + 1, 25868);
        let visible_pixels: usize = (0..180).map(|y| {
            let (_, min, max) = chalk_row_info(y);
            (max - min + 1) as usize
        }).sum();
        assert_eq!(visible_pixels, 25792);
    }
}

// The host framebuffer is always 180x180; guest window coordinates reflect the
// PBW's target platform. A square app is fitted into the round host display.
static GUEST_PLATFORM: AtomicU8 = AtomicU8::new(0);

pub fn set_guest_platform(platform: &str) {
    let kind = match platform {
        "basalt" | "diorite" | "aplite" => 1,
        _ => 0,
    };
    GUEST_PLATFORM.store(kind, Ordering::Relaxed);
}

pub fn guest_dimensions() -> (usize, usize) {
    match GUEST_PLATFORM.load(Ordering::Relaxed) {
        1 => (144, 168),
        _ => (180, 180),
    }
}

/// The Pebble graphics context state
#[derive(Clone)]
pub struct GContext {
    pub fill_color: u8,
    pub stroke_color: u8,
    pub text_color: u8,
    pub stroke_width: u8,
}

impl Default for GContext {
    fn default() -> Self {
        Self {
            fill_color: gcolor::colors::WHITE,
            stroke_color: gcolor::colors::BLACK,
            text_color: gcolor::colors::BLACK,
            stroke_width: 1,
        }
    }
}

/// Shared state between the host and the running Pebble app
pub struct PebbleState {
    /// 180x180 framebuffer in GColor8 format
    pub framebuffer: Vec<u8>,
    pub gctx: GContext,
    pub app_name: String,
    pub info: Option<pbw::PebbleProcessInfo>,
}

impl PebbleState {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![gcolor::colors::BLACK; DISPLAY_WIDTH * DISPLAY_HEIGHT],
            gctx: GContext::default(),
            app_name: String::new(),
            info: None,
        }
    }

    /// Convert framebuffer to RGBA8888, scaled 2x (360x360)
    pub fn render_rgba(&self, output: &mut [u8]) {
        let scale = 2;
        let out_w = DISPLAY_WIDTH * scale;
        let cx = DISPLAY_WIDTH as f32 / 2.0;
        let cy = DISPLAY_HEIGHT as f32 / 2.0;
        let r = cx; // radius for circular mask

        for out_y in 0..DISPLAY_HEIGHT * scale {
            for out_x in 0..out_w {
                let src_x = out_x / scale;
                let src_y = out_y / scale;

                // Circular mask for Chalk's round display
                let dx = (src_x as f32 + 0.5) - cx;
                let dy = (src_y as f32 + 0.5) - cy;
                let dist = (dx * dx + dy * dy).sqrt();

                let out_idx = (out_y * out_w + out_x) * 4;
                if dist <= r {
                    let gc = self.framebuffer[src_y * DISPLAY_WIDTH + src_x];
                    let rgba = gcolor::gcolor8_to_rgba(gc);
                    output[out_idx] = rgba[0];
                    output[out_idx + 1] = rgba[1];
                    output[out_idx + 2] = rgba[2];
                    output[out_idx + 3] = 255;
                } else {
                    // Outside circle: transparent
                    output[out_idx] = 0;
                    output[out_idx + 1] = 0;
                    output[out_idx + 2] = 0;
                    output[out_idx + 3] = 0;
                }
            }
        }
    }

    /// Draw a filled rectangle
    pub fn fill_rect(&mut self, x: i16, y: i16, w: u16, h: u16) {
        let color = self.gctx.fill_color;
        for py in y.max(0) as usize..(y + h as i16).min(DISPLAY_HEIGHT as i16) as usize {
            for px in x.max(0) as usize..(x + w as i16).min(DISPLAY_WIDTH as i16) as usize {
                self.framebuffer[py * DISPLAY_WIDTH + px] = color;
            }
        }
    }

    /// Draw a line (simple Bresenham)
    pub fn draw_line(&mut self, x0: i16, y0: i16, x1: i16, y1: i16) {
        let color = self.gctx.stroke_color;
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx: i16 = if x0 < x1 { 1 } else { -1 };
        let sy: i16 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = x0;
        let mut y = y0;

        loop {
            if x >= 0 && x < DISPLAY_WIDTH as i16 && y >= 0 && y < DISPLAY_HEIGHT as i16 {
                self.framebuffer[y as usize * DISPLAY_WIDTH + x as usize] = color;
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Draw a filled circle
    pub fn fill_circle(&mut self, cx: i16, cy: i16, radius: u16) {
        let color = self.gctx.fill_color;
        let r = radius as i16;
        for py in (cy - r).max(0)..(cy + r + 1).min(DISPLAY_HEIGHT as i16) {
            for px in (cx - r).max(0)..(cx + r + 1).min(DISPLAY_WIDTH as i16) {
                let dx = px - cx;
                let dy = py - cy;
                if dx * dx + dy * dy <= r * r {
                    self.framebuffer[py as usize * DISPLAY_WIDTH + px as usize] = color;
                }
            }
        }
    }

    /// Clear the framebuffer
    pub fn clear(&mut self, color: u8) {
        self.framebuffer.fill(color);
    }
}

pub type SharedState = Arc<Mutex<PebbleState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(Mutex::new(PebbleState::new()))
}

/// Load a PBW and parse its header. Does NOT execute — just loads metadata.
pub fn load_pbw(
    state: &SharedState,
    pbw_path: &std::path::Path,
    platform: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (bin_data, res_data) = pbw::extract_from_pbw(pbw_path, platform)?;
    let info = pbw::parse_header(&bin_data)?;
    crate::pebble_api::set_piny_companion(info.uuid == crate::piny_companion::UUID);
    set_guest_platform(platform);

    println!("=== Pebble App Loaded ===");
    println!("  Name:     {}", info.name);
    println!("  Company:  {}", info.company);
    println!("  UUID:     {}", info.uuid_str());
    println!("  SDK:      {}.{}", info.sdk_version_major, info.sdk_version_minor);
    println!("  Struct:   0x{:02x}{:02x}", info.struct_version_major, info.struct_version_minor);
    println!("  Size:     {} bytes (load), {} bytes (virtual)", info.load_size, info.virtual_size);
    println!("  Entry:    0x{:04x}", info.entry_point);
    println!("  JumpTbl:  0x{:04x}", info.sym_table_addr);
    println!("  Relocs:   {}", info.num_reloc_entries);
    println!("  Flags:    0x{:04x} (watchface={}, platform={})", info.flags, info.is_watchface(), info.platform());
    println!("  Bin size: {} bytes", bin_data.len());
    println!("  Res size: {} bytes", res_data.len());

    {
        let mut s = state.lock().unwrap();
        s.app_name = info.name.clone();
        s.info = Some(info);
    }

    Ok((bin_data, res_data))
}


/// Declared before guest state so teardown runs after execution has unwound.
pub struct SessionCleanup;
impl Drop for SessionCleanup {
    fn drop(&mut self) { crate::pebble_api::reset_state(); }
}
