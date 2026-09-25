//! Pebble .pfo font parser and text renderer.
//!
//! Parses real Pebble v3 bitmap fonts extracted from the system resource pack.
//! Each system font is embedded as a static byte slice via `include_bytes!`.

use crate::pebble_api::GRect;
use crate::runtime::{DISPLAY_HEIGHT, DISPLAY_WIDTH};

/// Pebble text alignment values
pub const TEXT_ALIGN_LEFT: u8 = 0;
pub const TEXT_ALIGN_CENTER: u8 = 1;
pub const TEXT_ALIGN_RIGHT: u8 = 2;

// ---------------------------------------------------------------------------
// Embedded system fonts (extracted from system_resources_robert.pbpack)
// ---------------------------------------------------------------------------

struct SystemFont {
    key_suffix: &'static str,  // e.g. "GOTHIC_14" — matched against end of font key
    data: &'static [u8],
}

static SYSTEM_FONTS: &[SystemFont] = &[
    SystemFont { key_suffix: "GOTHIC_09",               data: include_bytes!("../fonts/gothic_09.pfo") },
    SystemFont { key_suffix: "GOTHIC_14_BOLD",          data: include_bytes!("../fonts/gothic_14_bold.pfo") },
    SystemFont { key_suffix: "GOTHIC_14",               data: include_bytes!("../fonts/gothic_14.pfo") },
    SystemFont { key_suffix: "GOTHIC_18_BOLD",          data: include_bytes!("../fonts/gothic_18.pfo") },  // no bold .pfo — alias to regular
    SystemFont { key_suffix: "GOTHIC_18",               data: include_bytes!("../fonts/gothic_18.pfo") },
    SystemFont { key_suffix: "GOTHIC_24_BOLD",          data: include_bytes!("../fonts/gothic_24_bold.pfo") },
    SystemFont { key_suffix: "GOTHIC_24",               data: include_bytes!("../fonts/gothic_24.pfo") },
    SystemFont { key_suffix: "GOTHIC_28_BOLD",          data: include_bytes!("../fonts/gothic_28_bold.pfo") },
    SystemFont { key_suffix: "GOTHIC_28",               data: include_bytes!("../fonts/gothic_28.pfo") },
    SystemFont { key_suffix: "GOTHIC_36",               data: include_bytes!("../fonts/gothic_36.pfo") },
    SystemFont { key_suffix: "BITHAM_30_BLACK",         data: include_bytes!("../fonts/bitham_30_black.pfo") },
    SystemFont { key_suffix: "BITHAM_42_BOLD",          data: include_bytes!("../fonts/bitham_42_bold.pfo") },
    SystemFont { key_suffix: "BITHAM_42_LIGHT",         data: include_bytes!("../fonts/bitham_42_light.pfo") },
    SystemFont { key_suffix: "BITHAM_42_MEDIUM_NUMBERS",data: include_bytes!("../fonts/bitham_42_medium_numbers.pfo") },
    SystemFont { key_suffix: "BITHAM_34_MEDIUM_NUMBERS",data: include_bytes!("../fonts/bitham_34_medium_numbers.pfo") },
    SystemFont { key_suffix: "ROBOTO_CONDENSED_21",     data: include_bytes!("../fonts/roboto_condensed_21.pfo") },
    SystemFont { key_suffix: "ROBOTO_BOLD_SUBSET_49",   data: include_bytes!("../fonts/roboto_bold_subset_49.pfo") },
    SystemFont { key_suffix: "DROID_SERIF_28_BOLD",     data: include_bytes!("../fonts/droid_serif_28_bold.pfo") },
];

// Fallback font index (GOTHIC_14 at index 2)
pub const FALLBACK_FONT_INDEX: usize = 2;
pub const GOTHIC_14_INDEX: usize = 2;
pub const GOTHIC_18_INDEX: usize = 4;
pub const GOTHIC_24_BOLD_INDEX: usize = 5;

// ---------------------------------------------------------------------------
// .pfo format parsing
// ---------------------------------------------------------------------------

/// Parsed font header (v3, 10 bytes).
struct PfoHeader {
    max_height: u8,
    number_of_glyphs: u16,
    wildcard_codepoint: u16,
    hash_table_size: u8,
    codepoint_bytes: u8,
    struct_size: u8,
    offset_16: bool, // FEATURE_OFFSET_16
}

fn parse_header(data: &[u8]) -> Option<PfoHeader> {
    if data.len() < 10 { return None; }
    let version = data[0] & 0x3F;
    if version < 2 { return None; } // only v2/v3 supported
    let struct_size = if version == 3 && data.len() >= 10 { data[8] } else { 8 };
    let features = if version == 3 && data.len() >= 10 { data[9] } else { 0 };
    Some(PfoHeader {
        max_height: data[1],
        number_of_glyphs: u16::from_le_bytes([data[2], data[3]]),
        wildcard_codepoint: u16::from_le_bytes([data[4], data[5]]),
        hash_table_size: data[6],
        codepoint_bytes: data[7],
        struct_size,
        offset_16: (features & 0x01) != 0,
    })
}

/// Glyph metrics + bitmap location within the font data.
struct GlyphInfo {
    width: u8,
    height: u8,
    left_offset: i8,
    top_offset: i8,
    advance: i8,
    bitmap_start: usize, // byte offset into font data
    bitmap_bytes: usize,  // number of bytes of bitmap data
}

/// Look up a glyph by codepoint in a .pfo font.
fn lookup_glyph(data: &[u8], hdr: &PfoHeader, codepoint: u32) -> Option<GlyphInfo> {
    let hash_table_start = hdr.struct_size as usize;
    let offset_table_start = hash_table_start + (hdr.hash_table_size as usize) * 4;

    let offset_bytes: usize = if hdr.offset_16 { 2 } else { 4 };
    let entry_size = hdr.codepoint_bytes as usize + offset_bytes;

    let glyph_table_start = offset_table_start + (hdr.number_of_glyphs as usize) * entry_size;

    // Hash lookup
    let bucket_idx = (codepoint % hdr.hash_table_size as u32) as usize;
    let ht_off = hash_table_start + bucket_idx * 4;
    if ht_off + 4 > data.len() { return None; }

    let count = data[ht_off + 1] as usize;
    let bucket_offset = u16::from_le_bytes([data[ht_off + 2], data[ht_off + 3]]) as usize;
    let bucket_start = offset_table_start + bucket_offset;

    // Search bucket for matching codepoint
    for i in 0..count {
        let e_off = bucket_start + i * entry_size;
        if e_off + entry_size > data.len() { break; }

        let entry_cp = if hdr.codepoint_bytes == 2 {
            u16::from_le_bytes([data[e_off], data[e_off + 1]]) as u32
        } else {
            u32::from_le_bytes([data[e_off], data[e_off + 1], data[e_off + 2], data[e_off + 3]])
        };

        if entry_cp == codepoint {
            let glyph_offset = if hdr.offset_16 {
                let off = hdr.codepoint_bytes as usize;
                u16::from_le_bytes([data[e_off + off], data[e_off + off + 1]]) as usize
            } else {
                let off = hdr.codepoint_bytes as usize;
                u32::from_le_bytes([
                    data[e_off + off], data[e_off + off + 1],
                    data[e_off + off + 2], data[e_off + off + 3],
                ]) as usize
            };

            // Read glyph header (5 bytes)
            let gh_off = glyph_table_start + glyph_offset;
            if gh_off + 5 > data.len() { return None; }

            let width = data[gh_off];
            let height = data[gh_off + 1];
            let left_offset = data[gh_off + 2] as i8;
            let top_offset = data[gh_off + 3] as i8;
            let advance = data[gh_off + 4] as i8;

            let bitmap_bits = width as usize * height as usize;
            let bitmap_bytes = (bitmap_bits + 7) / 8;
            let bitmap_start = gh_off + 5;

            return Some(GlyphInfo {
                width, height, left_offset, top_offset, advance,
                bitmap_start, bitmap_bytes,
            });
        }
    }

    None
}

/// Get a pixel from a 1-bit glyph bitmap (LSB-first packing).
#[inline]
fn glyph_pixel(data: &[u8], bitmap_start: usize, width: usize, x: usize, y: usize) -> bool {
    let bit_idx = y * width + x;
    let byte_idx = bitmap_start + bit_idx / 8;
    let bit_pos = bit_idx % 8;
    if byte_idx < data.len() {
        (data[byte_idx] >> bit_pos) & 1 != 0
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Font handle management
// ---------------------------------------------------------------------------

/// A font handle is a pointer to a PfoFont struct stored in a global table.
/// We store up to MAX_LOADED_FONTS fonts (system + app custom).
const MAX_LOADED_FONTS: usize = 32;
static mut LOADED_FONTS: [Option<PfoFont>; MAX_LOADED_FONTS] = [const { None }; MAX_LOADED_FONTS];
static mut FONTS_INITIALIZED: bool = false;

type FontBytes = std::sync::Arc<std::borrow::Cow<'static, [u8]>>;

struct PfoFont {
    data: FontBytes, // owned storage; readers keep a snapshot during rendering
    max_height: u8,
}

/// Initialize system fonts (called once on first use).
fn ensure_init() {
    unsafe {
        if FONTS_INITIALIZED { return; }
        FONTS_INITIALIZED = true;

        for (i, sf) in SYSTEM_FONTS.iter().enumerate() {
            if i >= MAX_LOADED_FONTS { break; }
            let max_height = if sf.data.len() >= 2 { sf.data[1] } else { 14 };
            LOADED_FONTS[i] = Some(PfoFont {
                data: std::sync::Arc::new(std::borrow::Cow::Borrowed(sf.data)),
                max_height,
            });
        }
    }
}

/// Reset font state (called between app launches to free app fonts).
pub fn reset_app_fonts() {
    unsafe {
        // Keep system fonts (indices 0..SYSTEM_FONTS.len()), clear the rest
        for i in SYSTEM_FONTS.len()..MAX_LOADED_FONTS {
            LOADED_FONTS[i] = None;
        }
    }
}

pub fn unload_custom_font(font: *const u8) {
    if let Some(index) = decode_font_handle(font).filter(|&i| i >= SYSTEM_FONTS.len()) {
        unsafe { LOADED_FONTS[index] = None; }
    }
}

#[cfg(test)]
pub fn custom_font_bytes() -> usize {
    unsafe { LOADED_FONTS.iter().skip(SYSTEM_FONTS.len()).flatten().map(|f| f.data.len()).sum() }
}

/// Get font handle pointer for a system font key (e.g. "RESOURCE_ID_GOTHIC_28_BOLD").
/// Returns a pointer that encodes the font index. The pointer is NOT dereferenceable
/// by the Pebble app — it's only used as an opaque handle passed back to draw_text.
pub fn font_for_key(key: &str) -> *const u8 {
    ensure_init();

    // Match key suffix against our system fonts (longest match first since
    // GOTHIC_14_BOLD must match before GOTHIC_14)
    for (i, sf) in SYSTEM_FONTS.iter().enumerate() {
        if key.ends_with(sf.key_suffix) {
            return encode_font_handle(i);
        }
    }

    // Fallback to GOTHIC_14
    eprintln!("[pebble:font] Unknown font key '{}', falling back to GOTHIC_14", key);
    encode_font_handle(FALLBACK_FONT_INDEX)
}

/// Encode a font index as a pointer handle.
/// We use addresses in a reserved range that won't collide with real pointers.
pub fn encode_font_handle(index: usize) -> *const u8 {
    // Use a distinctive base address. The Pebble app never dereferences this.
    (0xF0F0_0000usize + index) as *const u8
}

/// Decode a font handle pointer back to an index, or None.
fn decode_font_handle(ptr: *const u8) -> Option<usize> {
    let addr = ptr as usize;
    if addr >= 0xF0F0_0000 && addr < 0xF0F0_0000 + MAX_LOADED_FONTS {
        Some(addr - 0xF0F0_0000)
    } else {
        None
    }
}

/// Get font data from a handle pointer.
fn font_data(font_ptr: *const u8) -> Option<FontBytes> {
    let idx = decode_font_handle(font_ptr)?;
    unsafe { LOADED_FONTS[idx].as_ref().map(|f| f.data.clone()) }
}

// ---------------------------------------------------------------------------
// Public API (kept compatible with pebble_api.rs)
// ---------------------------------------------------------------------------

/// Load a custom font from a resource handle (ResHandle = resource ID).
/// Loads the font data from the resource pack and stores it in the font table.
/// Returns a font handle pointer, or null if the resource couldn't be loaded.
pub fn load_custom_font(resource_id: u32) -> *const u8 {
    use crate::pebble_api;

    ensure_init();

    let data = match pebble_api::resource_get_data(resource_id) {
        Some(d) => d,
        None => {
            eprintln!("[pebble:font] fonts_load_custom_font({}) — resource not found", resource_id);
            return encode_font_handle(FALLBACK_FONT_INDEX);
        }
    };

    // Verify it's a valid PFO font
    if data.len() < 10 || (data[0] & 0x3F) < 2 {
        eprintln!("[pebble:font] fonts_load_custom_font({}) — invalid font data (len={}, ver={})",
            resource_id, data.len(), data.first().copied().unwrap_or(0) & 0x3F);
        return encode_font_handle(FALLBACK_FONT_INDEX);
    }

    let max_height = data[1];

    // Find a free slot in the font table (after system fonts)
    unsafe {
        for i in SYSTEM_FONTS.len()..MAX_LOADED_FONTS {
            if LOADED_FONTS[i].is_none() {
                LOADED_FONTS[i] = Some(PfoFont {
                    data: std::sync::Arc::new(std::borrow::Cow::Owned(data.to_vec())),
                    max_height,
                });
                eprintln!("[pebble:font] fonts_load_custom_font({}) — loaded, height={}, slot={}",
                    resource_id, max_height, i);
                return encode_font_handle(i);
            }
        }
    }

    eprintln!("[pebble:font] fonts_load_custom_font({}) — no free font slots", resource_id);
    encode_font_handle(FALLBACK_FONT_INDEX)
}

/// Map a Pebble font key to a font handle pointer.
/// Replaces the old font_key_to_scale + font_ptr_for_scale pair.
pub fn font_key_to_scale(_key: &str) -> usize {
    // Legacy compatibility — not used in new code path but kept for signature.
    // The actual font is selected by font_for_key().
    0
}

/// Get a font handle pointer for a given scale. Legacy compatibility.
pub fn font_ptr_for_scale(_scale: usize) -> *const u8 {
    // Legacy: return fallback font
    ensure_init();
    encode_font_handle(FALLBACK_FONT_INDEX)
}

/// Draw text into the framebuffer using a real .pfo font.
pub fn draw_text(
    fb: &mut [u8],
    text: &str,
    font_ptr: *const u8,
    box_rect: GRect,
    alignment: u8,
    color: u8,
) {
    let data = match font_data(font_ptr) {
        Some(d) => d,
        None => {
            // Unknown font handle — use fallback
            ensure_init();
            match font_data(encode_font_handle(FALLBACK_FONT_INDEX)) {
                Some(d) => d,
                None => return,
            }
        }
    };

    let data = &*data;
    let hdr = match parse_header(data) {
        Some(h) => h,
        None => return,
    };

    let max_height = hdr.max_height as i32;

    // Split text into lines and render each
    let mut y_cursor = box_rect.y as i32;

    for line in text.split('\n') {
        if y_cursor >= (box_rect.y + box_rect.h) as i32 {
            break;
        }

        // Measure line width for alignment
        let line_width = measure_line(data, &hdr, line);

        let x_start = match alignment {
            TEXT_ALIGN_CENTER => box_rect.x as i32 + (box_rect.w as i32 - line_width) / 2,
            TEXT_ALIGN_RIGHT => box_rect.x as i32 + box_rect.w as i32 - line_width,
            _ => box_rect.x as i32,
        };

        let mut cursor_x = x_start;

        for ch in line.chars() {
            let cp = ch as u32;
            let glyph = match lookup_glyph(data, &hdr, cp) {
                Some(g) => g,
                None => {
                    // Try wildcard
                    match lookup_glyph(data, &hdr, hdr.wildcard_codepoint as u32) {
                        Some(g) => g,
                        None => continue,
                    }
                }
            };

            // Clip: skip if past right edge
            if cursor_x >= (box_rect.x + box_rect.w) as i32 {
                break;
            }

            // Render glyph
            // top_offset = distance from line top to glyph top (from fontgen.py: max_height - bitmap_top)
            let gx = cursor_x + glyph.left_offset as i32;
            let gy = y_cursor + glyph.top_offset as i32;

            render_glyph(fb, data, &glyph, gx, gy, color, &box_rect);

            cursor_x += glyph.advance as i32;
        }

        y_cursor += max_height + 1; // line spacing
    }
}

/// Measure the content size (width, height) of a block of text with the given font.
/// Returns (width, height) in pixels.  Width is the max line width; height is
/// the total height of all lines (including line spacing).
pub fn measure_text(font_ptr: *const u8, text: &str) -> (i16, i16) {
    let data = match font_data(font_ptr) {
        Some(d) => d,
        None => {
            ensure_init();
            match font_data(encode_font_handle(FALLBACK_FONT_INDEX)) {
                Some(d) => d,
                None => return (0, 0),
            }
        }
    };

    let data = &*data;
    let hdr = match parse_header(data) {
        Some(h) => h,
        None => return (0, 0),
    };

    let max_height = hdr.max_height as i32;
    let mut max_w: i32 = 0;
    let mut num_lines: i32 = 0;

    for line in text.split('\n') {
        let lw = measure_line(data, &hdr, line);
        if lw > max_w { max_w = lw; }
        num_lines += 1;
    }

    let total_h = if num_lines > 0 {
        num_lines * max_height + (num_lines - 1) // +1 line spacing per gap
    } else {
        0
    };

    (max_w as i16, total_h as i16)
}

/// Measure the pixel width of a line of text.
fn measure_line(data: &[u8], hdr: &PfoHeader, line: &str) -> i32 {
    let mut width: i32 = 0;
    for ch in line.chars() {
        let cp = ch as u32;
        let glyph = lookup_glyph(data, hdr, cp)
            .or_else(|| lookup_glyph(data, hdr, hdr.wildcard_codepoint as u32));
        if let Some(g) = glyph {
            width += g.advance as i32;
        }
    }
    width
}

/// Render a single glyph bitmap to the framebuffer.
fn render_glyph(
    fb: &mut [u8],
    data: &[u8],
    glyph: &GlyphInfo,
    x: i32,
    y: i32,
    color: u8,
    clip: &GRect,
) {
    let clip_x0 = (clip.x.max(0) as i32).max(0);
    let clip_y0 = (clip.y.max(0) as i32).max(0);
    let clip_x1 = ((clip.x + clip.w) as i32).min(DISPLAY_WIDTH as i32);
    let clip_y1 = ((clip.y + clip.h) as i32).min(DISPLAY_HEIGHT as i32);

    let w = glyph.width as usize;
    let h = glyph.height as usize;

    for row in 0..h {
        let py = y + row as i32;
        if py < clip_y0 || py >= clip_y1 { continue; }

        for col in 0..w {
            let px = x + col as i32;
            if px < clip_x0 || px >= clip_x1 { continue; }

            if glyph_pixel(data, glyph.bitmap_start, w, col, row) {
                fb[py as usize * DISPLAY_WIDTH + px as usize] = color;
            }
        }
    }
}
