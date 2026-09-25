//! Emulated ARM Thumb execution for desktop using Armagnac.
//!
//! Replaces the native mmap+call execution path with an ARMv7-M emulator,
//! allowing Pebble .pbw binaries to run on x86_64 (or any non-ARM host).

use crate::executor;
use crate::gcolor;
use crate::pbw::PebbleProcessInfo;
use crate::pebble_api::{self, GColor8, GPath, GPoint, GRect, PblGContext, PblLayer, PblWindow};
use crate::runtime::{DISPLAY_HEIGHT, DISPLAY_WIDTH};
use armagnac::core::{Config, Emulator, Event, Processor, RunOptions};
use armagnac::registers::RegisterIndex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Memory layout
// ---------------------------------------------------------------------------

const BIN_BASE: u32 = 0x1000_0000;
const STACK_BASE: u32 = 0x2000_0000;
const STACK_SIZE: u32 = 0x1_0000; // 64 KB
const HEAP_BASE: u32 = 0x2001_0000;
const HEAP_SIZE: u32 = 0x4_0000; // 256 KB
const TRAMPOLINE_BASE: u32 = 0x3000_0000;
const JT_BASE: u32 = 0x5000_0000;
const TM_BUF_ADDR: u32 = 0x6000_0000; // struct tm buffer
const TM_BUF_SIZE: u32 = 128;
const HANDLE_RAM_BASE: u32 = 0xD000_0000; // Safety-net RAM for handle dereferences
const HANDLE_RAM_SIZE: u32 = 0x2_0000; // 128 KB
const HANDLE_STRIDE: u32 = 128; // bytes per handle slot (fits PebbleOS Layer=44B, Window=84B)

// PebbleOS Layer struct offsets (ARM 32-bit, no touchscreen)
const LAYER_OFF_BOUNDS: u32 = 0;
const LAYER_OFF_FRAME: u32 = 8;
const LAYER_OFF_FLAGS: u32 = 16;
const LAYER_OFF_NEXT_SIBLING: u32 = 20;
const LAYER_OFF_PARENT: u32 = 24;
const LAYER_OFF_FIRST_CHILD: u32 = 28;
const LAYER_OFF_WINDOW: u32 = 32;
const LAYER_OFF_UPDATE_PROC: u32 = 36;

const JUMP_TABLE_SIZE: usize = 632;
const CALLBACK_RETURN_IDX: usize = JUMP_TABLE_SIZE; // index 632
const CALLBACK_RETURN_ADDR: u32 = TRAMPOLINE_BASE + CALLBACK_RETURN_IDX as u32 * 4;
const TRAMPOLINE_ENTRIES: u32 = (JUMP_TABLE_SIZE + 4) as u32;
const TRAMPOLINE_SIZE: u32 = TRAMPOLINE_ENTRIES * 4;

// ---------------------------------------------------------------------------
// Emulated state
// ---------------------------------------------------------------------------

struct EmuTimer {
    id: u32,
    deadline: std::time::Instant,
    callback_addr: u32,
    context: u32,
}

struct EmuState {
    /// Handle table: u32 handle -> host pointer (as usize)
    handles: Vec<usize>,
    handle_generations: Vec<Option<u64>>,
    /// Bump allocator offset
    heap_offset: u32,
    /// Callback addresses (emulated code pointers)
    tick_handler_addr: u32,
    tick_units: u32,
    update_procs: Vec<(u32, u32)>, // (layer_handle, emulated_proc_addr)
    window_load_handler: u32,
    window_unload_handler: u32,
    current_window_handle: u32,
    /// Host-side graphics context (used by drawing functions)
    gctx: PblGContext,
    /// Cached localtime result for strftime
    cached_tm: Option<libc::tm>,
    /// Text copies: tl_handle -> host CString pointer
    text_ptrs: Vec<(u32, *const std::ffi::c_char)>,
    /// App timers
    timers: Vec<EmuTimer>,
    next_timer_id: u32,
    /// layer_create_with_data: handle -> emulated heap address of data area
    layer_data_addrs: Vec<(u32, u32)>,
    /// Accelerometer
    accel_handler_addr: u32,
    accel_samples_per_update: u32,
    accel_sampling_rate: u32,
    accel_last_poll: Option<std::time::Instant>,
    /// Reusable emulated buffer for gbitmap_get_data_row_info (row data)
    data_row_buf_addr: u32,
    data_row_buf_size: u32,
    /// Cached emulated address for i18n_get_system_locale string
    locale_addr: u32,
    /// AppSync state
    app_sync_tuple_changed_cb: u32,
    app_sync_error_cb: u32,
    app_sync_context: u32,
    app_sync_buffer: u32,
    app_sync_buffer_size: u16,
    /// Emulated framebuffer address (allocated on first capture)
    emu_fb_addr: u32,
    /// Handle of the captured framebuffer bitmap
    captured_fb_handle: u32,
    /// Click handling: config provider callback address
    click_config_provider: u32,
    click_config_context: u32,
    /// Per-button single click handlers: [BACK, UP, SELECT, DOWN]
    single_click_handlers: [u32; 4],
    /// Per-button click contexts
    click_contexts: [u32; 4],
    /// Pending button presses (consumed by event loop)
    pending_buttons: Arc<Mutex<Vec<u8>>>,
    /// Current button being processed (for click_recognizer_get_button_id)
    current_button: u8,
    /// Cache of bitmap handle -> (emu_data_addr, data_size) to avoid re-copying
    bitmap_emu_data: Vec<(u32, u32, u32)>,
}

impl Drop for EmuState {
    fn drop(&mut self) {
        for (_, ptr) in self.text_ptrs.drain(..) {
            unsafe { drop(std::ffi::CString::from_raw(ptr as *mut _)); }
        }
    }
}

impl EmuState {
    fn new() -> Self {
        Self {
            handles: Vec::new(),
            handle_generations: Vec::new(),
            heap_offset: 0,
            tick_handler_addr: 0,
            tick_units: 0,
            update_procs: Vec::new(),
            window_load_handler: 0,
            window_unload_handler: 0,
            current_window_handle: 0,
            gctx: PblGContext {
                fill_color: gcolor::colors::BLACK,
                stroke_color: gcolor::colors::WHITE,
                text_color: gcolor::colors::BLACK,
                stroke_width: 1,
            },
            cached_tm: None,
            text_ptrs: Vec::new(),
            timers: Vec::new(),
            next_timer_id: 1,
            layer_data_addrs: Vec::new(),
            accel_handler_addr: 0,
            accel_samples_per_update: 0,
            accel_sampling_rate: 25,
            accel_last_poll: None,
            data_row_buf_addr: 0,
            data_row_buf_size: 0,
            locale_addr: 0,
            app_sync_tuple_changed_cb: 0,
            app_sync_error_cb: 0,
            app_sync_context: 0,
            app_sync_buffer: 0,
            app_sync_buffer_size: 0,
            emu_fb_addr: 0,
            captured_fb_handle: 0,
            click_config_provider: 0,
            click_config_context: 0,
            single_click_handlers: [0; 4],
            click_contexts: [0; 4],
            pending_buttons: Arc::new(Mutex::new(Vec::new())),
            current_button: 0,
            bitmap_emu_data: Vec::new(),
        }
    }

    fn to_handle(&mut self, ptr: usize) -> u32 {
        self.handles.push(ptr);
        self.handle_generations.push(crate::owned::generation(ptr as *const u8));
        HANDLE_RAM_BASE + ((self.handles.len() - 1) as u32) * HANDLE_STRIDE
    }

    fn from_handle_raw(&self, h: u32) -> usize {
        if h < HANDLE_RAM_BASE || h >= HANDLE_RAM_BASE + HANDLE_RAM_SIZE {
            return h as usize; // not a handle, pass through
        }
        let offset = h - HANDLE_RAM_BASE;
        if offset % HANDLE_STRIDE != 0 {
            // Not a handle base address — could be app reading a struct field.
            // Pass through (will resolve to safety-net RAM content).
            return h as usize;
        }
        let idx = (offset / HANDLE_STRIDE) as usize;
        if idx < self.handles.len() {
            if let Some(generation) = self.handle_generations[idx] {
                if crate::owned::generation(self.handles[idx] as *const u8) != Some(generation) { return 0; }
            }
            self.handles[idx]
        } else {
            eprintln!("[emu] BAD HANDLE 0x{:08x} (idx={}, len={})", h, idx, self.handles.len());
            0
        }
    }

    fn from_handle<T>(&self, h: u32) -> *mut T {
        self.from_handle_raw(h) as *mut T
    }

    fn forget_handle(&mut self, handle: u32) -> usize {
        let ptr = self.from_handle_raw(handle);
        let aliases: Vec<u32> = self.handles.iter().enumerate().filter_map(|(i, &p)|
            (p == ptr).then_some(HANDLE_RAM_BASE + i as u32 * HANDLE_STRIDE)).collect();
        self.update_procs.retain(|(h, _)| !aliases.contains(h));
        self.layer_data_addrs.retain(|(h, _)| !aliases.contains(h));
        self.bitmap_emu_data.retain(|(h, _, _)| !aliases.contains(h));
        for p in &mut self.handles { if *p == ptr { *p = 0; } }
        ptr
    }

    /// Reverse lookup: find the handle for a given host pointer. Returns 0 if not found.
    fn find_handle(&self, ptr: usize) -> u32 {
        for (i, &h) in self.handles.iter().enumerate() {
            if h == ptr {
                return HANDLE_RAM_BASE + (i as u32) * HANDLE_STRIDE;
            }
        }
        0
    }

    fn emu_malloc(&mut self, proc: &mut Processor, size: u32) -> u32 {
        let Some(aligned) = size.checked_add(7).map(|n| n & !7) else { return 0; };
        let Some(end) = self.heap_offset.checked_add(aligned).filter(|&n| n <= HEAP_SIZE) else { return 0; };
        let addr = HEAP_BASE + self.heap_offset;
        self.heap_offset = end;
        for i in 0..aligned {
            let _ = proc.write_u8(addr + i, 0);
        }
        addr
    }

    fn set_text_for_handle(&mut self, tl_handle: u32, text: &str) {
        // Free old CString if any
        self.text_ptrs.retain(|&(h, ptr)| {
            if h == tl_handle {
                unsafe { drop(std::ffi::CString::from_raw(ptr as *mut _)); }
                false
            } else {
                true
            }
        });
        // Store new copy
        if let Ok(cstr) = std::ffi::CString::new(text) {
            let ptr = cstr.into_raw() as *const std::ffi::c_char;
            // Update the TextLayer's text pointer
            let tl = self.from_handle::<pebble_api::PblTextLayer>(tl_handle);
            if !tl.is_null() {
                unsafe { (*tl).text = ptr; }
            }
            self.text_ptrs.push((tl_handle, ptr));
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_cstring(proc: &mut Processor, addr: u32) -> String {
    if addr == 0 {
        return String::new();
    }
    let mut bytes = Vec::new();
    let mut a = addr;
    loop {
        match proc.read_u8(a) {
            Ok(0) | Err(_) => break,
            Ok(b) => {
                bytes.push(b);
                a += 1;
                if bytes.len() > 4096 {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn write_bytes(proc: &mut Processor, addr: u32, data: &[u8]) {
    for (i, &b) in data.iter().enumerate() {
        let _ = proc.write_u8(addr + i as u32, b);
    }
}

fn grect_from_regs(lo: u32, hi: u32) -> GRect {
    GRect {
        x: lo as i16,
        y: (lo >> 16) as i16,
        w: hi as i16,
        h: (hi >> 16) as i16,
    }
}

fn grect_to_u32s(r: &GRect) -> (u32, u32) {
    (
        (r.x as u16 as u32) | ((r.y as u16 as u32) << 16),
        (r.w as u16 as u32) | ((r.h as u16 as u32) << 16),
    )
}

fn write_grect(proc: &mut Processor, addr: u32, r: &GRect) {
    let (lo, hi) = grect_to_u32s(r);
    let _ = proc.write_u32_aligned(addr, lo);
    let _ = proc.write_u32_aligned(addr + 4, hi);
}

/// Write PebbleOS-compatible Layer struct fields to emulated memory at `handle_addr`.
/// This lets Pebble apps that directly access Layer struct fields (e.g. EffectLayer)
/// read correct values like parent pointers, bounds, etc.
fn write_emu_layer(proc: &mut Processor, handle_addr: u32, bounds: &GRect, frame: &GRect) {
    write_grect(proc, handle_addr + LAYER_OFF_BOUNDS, bounds);
    write_grect(proc, handle_addr + LAYER_OFF_FRAME, frame);
    // flags: clips=true (bit 0)
    let _ = proc.write_u8(handle_addr + LAYER_OFF_FLAGS, 0x01);
}

/// Write GBitmap struct fields into handle RAM so apps that directly dereference
/// the bitmap pointer (e.g., `bitmap->data[offset]`) can access pixel data.
/// Pebble GBitmap layout (32-bit ARM):
///   0: addr (u32 pointer to pixel data)
///   4: row_size_bytes (u16)
///   6: info_flags (u16)
///   8: bounds (GRect = 4× i16 = 8 bytes)
///  16: data_row_infos / palette (u32 pointer)
fn write_emu_bitmap(proc: &mut Processor, state: &mut EmuState, handle: u32, bmp: *const pebble_api::PblGBitmap) {
    if bmp.is_null() {
        return;
    }
    let bmp_ref = unsafe { &*bmp };
    // Copy pixel data (+palette) to emulated memory
    let data_size = bmp_ref.row_size_bytes as u32 * bmp_ref.bounds.h.max(0) as u32;
    // For palettized formats, also include inline palette after pixel data
    let total_size = if bmp_ref.palette.is_null() {
        let format = bmp_ref.info_flags;
        let palette_size: u32 = match format {
            2 => 2,   // 1-bit palette: 2 entries
            3 => 4,   // 2-bit palette: 4 entries
            4 => 16,  // 4-bit palette: 16 entries
            _ => 0,
        };
        data_size + palette_size
    } else {
        data_size
    };
    let emu_data_addr = if total_size > 0 && !bmp_ref.data.is_null() {
        let addr = state.emu_malloc(proc, total_size);
        for i in 0..total_size {
            let byte = unsafe { *bmp_ref.data.add(i as usize) };
            let _ = proc.write_u8(addr + i, byte);
        }
        // Cache this emu address for gbitmap_get_data lookups
        state.bitmap_emu_data.push((handle, addr, data_size));
        addr
    } else {
        0
    };
    // Write GBitmap struct fields to handle RAM
    let _ = proc.write_u32_aligned(handle, emu_data_addr); // data pointer
    let _ = proc.write_u16_aligned(handle + 4, bmp_ref.row_size_bytes); // row_size_bytes
    let _ = proc.write_u16_aligned(handle + 6, bmp_ref.info_flags); // info_flags
    write_grect(proc, handle + 8, &bmp_ref.bounds); // bounds
    // palette pointer
    if !bmp_ref.palette.is_null() {
        let format = bmp_ref.info_flags;
        let palette_size: u32 = match format {
            1 => 64,  // 8-bit: 64 colors
            2 => 2,   // 1-bit palette
            3 => 4,   // 2-bit palette
            4 => 16,  // 4-bit palette
            _ => 0,
        };
        if palette_size > 0 {
            let emu_pal = state.emu_malloc(proc, palette_size);
            for i in 0..palette_size {
                let byte = unsafe { *bmp_ref.palette.add(i as usize) };
                let _ = proc.write_u8(emu_pal + i, byte);
            }
            let _ = proc.write_u32_aligned(handle + 16, emu_pal);
        }
    }
}

fn gpoint_from_reg(v: u32) -> GPoint {
    GPoint {
        x: v as i16,
        y: (v >> 16) as i16,
    }
}

fn gpoint_to_u32(p: &GPoint) -> u32 {
    (p.x as u16 as u32) | ((p.y as u16 as u32) << 16)
}

/// Read a u32 from the emulated stack (for 5th+ args)
fn stack_arg(proc: &mut Processor, n: usize) -> u32 {
    let sp = proc.sp();
    proc.read_u32_aligned(sp + (n as u32) * 4).unwrap_or(0)
}

/// Write a struct tm to emulated memory
fn write_tm(proc: &mut Processor, addr: u32, tm: &libc::tm) {
    let _ = proc.write_u32_aligned(addr, tm.tm_sec as u32);
    let _ = proc.write_u32_aligned(addr + 4, tm.tm_min as u32);
    let _ = proc.write_u32_aligned(addr + 8, tm.tm_hour as u32);
    let _ = proc.write_u32_aligned(addr + 12, tm.tm_mday as u32);
    let _ = proc.write_u32_aligned(addr + 16, tm.tm_mon as u32);
    let _ = proc.write_u32_aligned(addr + 20, tm.tm_year as u32);
    let _ = proc.write_u32_aligned(addr + 24, tm.tm_wday as u32);
    let _ = proc.write_u32_aligned(addr + 28, tm.tm_yday as u32);
    let _ = proc.write_u32_aligned(addr + 32, tm.tm_isdst as u32);
}

/// Write a packed AccelData (15 bytes) to emulated memory.
fn write_accel_data(proc: &mut Processor, addr: u32, data: &crate::accel::AccelData) {
    let xb = data.x.to_le_bytes();
    let yb = data.y.to_le_bytes();
    let zb = data.z.to_le_bytes();
    let tb = data.timestamp.to_le_bytes();
    for (i, &b) in xb.iter().chain(yb.iter()).chain(zb.iter()).enumerate() {
        let _ = proc.write_u8(addr + i as u32, b);
    }
    let _ = proc.write_u8(addr + 6, data.did_vibrate);
    for (i, &b) in tb.iter().enumerate() {
        let _ = proc.write_u8(addr + 7 + i as u32, b);
    }
}

// ---------------------------------------------------------------------------
// Dispatch result
// ---------------------------------------------------------------------------

pub enum Action {
    Return(u32),
    EventLoop,
}

// ---------------------------------------------------------------------------
// API dispatch
// ---------------------------------------------------------------------------

static mut TRACE_ALL: bool = true;

fn dispatch(proc: &mut Processor, state: &mut EmuState, idx: usize) -> Action {
    let r0 = proc[RegisterIndex::R0];
    let r1 = proc[RegisterIndex::R1];
    unsafe {
        if TRACE_ALL {
            let name = executor::jump_table_name(idx);
            eprintln!("[trace] #{} {} r0=0x{:08x} r1=0x{:08x}", idx, name, r0, r1);
        }
    }
    let r2 = proc[RegisterIndex::R2];
    let r3 = proc[RegisterIndex::R3];

    match idx {
        // =================================================================
        // App lifecycle
        // =================================================================

        // app_event_loop
        31 => Action::EventLoop,

        // app_log(level, filename, line, fmt)
        34 => {
            let fmt = read_cstring(proc, r3);
            println!("[pebble:log] level={} line={}: {}", r0 as u8, r2 as u16, fmt);
            Action::Return(0)
        }

        // app_timer_cancel(timer_id)
        47 => {
            state.timers.retain(|t| t.id != r0);
            Action::Return(0)
        }

        // app_timer_register(timeout_ms, callback, context)
        48 => {
            let id = state.next_timer_id;
            state.next_timer_id += 1;
            state.timers.push(EmuTimer {
                id,
                deadline: std::time::Instant::now() + std::time::Duration::from_millis(r0 as u64),
                callback_addr: r1,
                context: r2,
            });
            Action::Return(id)
        }

        // app_timer_reschedule(timer_id, new_timeout_ms)
        49 => {
            if let Some(t) = state.timers.iter_mut().find(|t| t.id == r0) {
                t.deadline = std::time::Instant::now() + std::time::Duration::from_millis(r1 as u64);
            }
            Action::Return(r0)
        }

        // =================================================================
        // Trig
        // =================================================================

        // atan2_lookup
        50 => Action::Return(pebble_api::pbl_atan2_lookup(r0 as i16, r1 as i16) as u32),

        // atoi(str)
        51 => {
            let s = read_cstring(proc, r0);
            Action::Return(s.parse::<i32>().unwrap_or(0) as u32)
        }
        // atol(str)
        52 => {
            let s = read_cstring(proc, r0);
            Action::Return(s.parse::<i32>().unwrap_or(0) as u32)
        }

        // battery_state_service_peek
        53 => Action::Return(pebble_api::pbl_battery_state_service_peek()),

        // battery_state_service_subscribe/unsubscribe
        54 | 55 => Action::Return(0),

        // bitmap_layer_create(frame: GRect)
        56 => {
            let frame = grect_from_regs(r0, r1);
            let bl = pebble_api::pbl_bitmap_layer_create(frame);
            Action::Return(state.to_handle(bl as usize))
        }
        // bitmap_layer_destroy
        57 => {
            pebble_api::pbl_bitmap_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // bitmap_layer_get_layer
        58 => {
            let layer = pebble_api::pbl_bitmap_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }
        // bitmap_layer_set_bitmap
        61 => {
            pebble_api::pbl_bitmap_layer_set_bitmap(state.from_handle(r0), state.from_handle(r1));
            Action::Return(0)
        }
        // bitmap_layer_set_compositing_mode
        62 => {
            pebble_api::pbl_bitmap_layer_set_compositing_mode(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // bitmap_layer_set_alignment / bitmap_layer_set_background_color_2bit
        59 | 60 => Action::Return(0),

        // bluetooth_connection_service_peek
        63 => Action::Return(pebble_api::pbl_bluetooth_connection_service_peek() as u32),
        // bluetooth subscribe/unsubscribe
        64 | 65 => Action::Return(0),

        // click_number_of_clicks_counted
        66 => Action::Return(1),
        // click_recognizer_get_button_id
        67 => Action::Return(state.current_button as u32),
        // clock_copy_time_string(buffer, size)
        68 => {
            let is_24h = pebble_api::pbl_clock_is_24h_style();
            let now = unsafe { libc::time(std::ptr::null_mut()) };
            let tm = unsafe { &*libc::localtime(&now) };
            let s = if is_24h {
                format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
            } else {
                let h = if tm.tm_hour == 0 { 12 } else if tm.tm_hour > 12 { tm.tm_hour - 12 } else { tm.tm_hour };
                format!("{:2}:{:02}", h, tm.tm_min)
            };
            let bytes = s.as_bytes();
            let max = (r1 as usize).saturating_sub(1);
            let n = bytes.len().min(max);
            write_bytes(proc, r0, &bytes[..n]);
            let _ = proc.write_u8(r0 + n as u32, 0);
            Action::Return(0)
        }

        // clock_is_24h_style
        69 => Action::Return(pebble_api::pbl_clock_is_24h_style() as u32),

        // cos_lookup
        70 => Action::Return(pebble_api::pbl_cos_lookup(r0 as i32) as u32),

        // =================================================================
        // Fonts
        // =================================================================

        // fonts_get_system_font(key: *const c_char)
        96 => {
            let key = read_cstring(proc, r0);
            let font = pebble_api::pbl_fonts_get_system_font_by_name(&key);
            Action::Return(state.to_handle(font as usize))
        }
        // Retire the host font and its emulated handle.
        98 => {
            crate::font::unload_custom_font(state.forget_handle(r0) as *const u8);
            Action::Return(0)
        },

        // =================================================================
        // Memory / libc
        // =================================================================

        // free
        99 => {
            // Bump allocator — don't actually free
            Action::Return(0)
        }

        // fonts_load_custom_font(ResHandle) -> GFont
        97 => {
            let font = crate::font::load_custom_font(r0);
            Action::Return(state.to_handle(font as usize))
        }

        // gbitmap_create_with_data(data) — treat as opaque, create blank
        101 => {
            let bmp = pebble_api::pbl_gbitmap_create_blank(0, 0);
            let h = state.to_handle(bmp as usize);
            write_emu_bitmap(proc, state, h, bmp as *const _);
            Action::Return(h)
        }
        // gbitmap_create_with_resource
        102 => {
            let bmp = pebble_api::pbl_gbitmap_create_with_resource(r0);
            let h = state.to_handle(bmp as usize);
            write_emu_bitmap(proc, state, h, bmp as *const _);
            Action::Return(h)
        }
        // gbitmap_destroy
        103 => {
            pebble_api::pbl_gbitmap_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // gmtime(time_t*) — reuse cached_tm with UTC
        104 => {
            let t = proc.read_u32_aligned(r0).unwrap_or(0) as i64;
            let secs = t;
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            // Simple UTC conversion
            let days = secs / 86400;
            let rem = secs % 86400;
            tm.tm_hour = (rem / 3600) as i32;
            tm.tm_min = ((rem % 3600) / 60) as i32;
            tm.tm_sec = (rem % 60) as i32;
            tm.tm_wday = ((days + 4) % 7) as i32; // 1970-01-01 was Thursday
            // Approximate year/month/day
            let mut y = 1970i32;
            let mut d = days;
            loop {
                let ydays = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
                if d < ydays { break; }
                d -= ydays;
                y += 1;
            }
            tm.tm_year = y - 1900;
            tm.tm_yday = d as i32;
            let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
            let mdays = [31, if leap {29} else {28}, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            let mut mon = 0;
            for &md in &mdays {
                if d < md { break; }
                d -= md;
                mon += 1;
            }
            tm.tm_mon = mon;
            tm.tm_mday = d as i32 + 1;
            state.cached_tm = Some(tm);
            // Write tm struct to emulated memory
            let tm_addr = state.emu_malloc(proc, 44);
            write_tm(proc, tm_addr, &tm);
            Action::Return(tm_addr)
        }

        // gpath_create(init: *const GPathInfo)
        // GPathInfo is { num_points: u32, points: *const GPoint } in emulated memory
        105 => {
            let num_points = proc.read_u32_aligned(r0).unwrap_or(0);
            let points_addr = proc.read_u32_aligned(r0 + 4).unwrap_or(0);
            // Read points from emulated memory
            let mut points = Vec::new();
            for i in 0..num_points {
                let px = proc.read_u32_aligned(points_addr + i * 4).unwrap_or(0);
                points.push(GPoint {
                    x: px as i16,
                    y: (px >> 16) as i16,
                });
            }
            // Create host GPath
            let boxed_points = points.into_boxed_slice();
            let points_ptr = boxed_points.as_ptr();
            let path = Box::into_raw(Box::new(GPath {
                num_points,
                points: points_ptr,
                rotation: 0,
                offset: GPoint { x: 0, y: 0 },
            }));
            std::mem::forget(boxed_points); // leak — path owns the pointer
            Action::Return(state.to_handle(path as usize))
        }
        // gpath_destroy
        106 => {
            pebble_api::pbl_gpath_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // gpath_draw_filled (legacy)
        107 => {
            pebble_api::pbl_gpath_draw_filled(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }
        // gpath_draw_outline
        108 => {
            pebble_api::pbl_gpath_draw_outline(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }
        // gpath_move_to
        109 => {
            pebble_api::pbl_gpath_move_to(state.from_handle(r0), gpoint_from_reg(r1));
            Action::Return(0)
        }
        // gpath_rotate_to
        110 => {
            pebble_api::pbl_gpath_rotate_to(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }

        // gpoint_equal
        111 => {
            let a = gpoint_from_reg(r0);
            let b = gpoint_from_reg(r1);
            Action::Return((a.x == b.x && a.y == b.y) as u32)
        }

        // =================================================================
        // Graphics context
        // =================================================================

        // graphics_context_set_compositing_mode
        112 => Action::Return(0),

        // graphics_context_set_fill_color_2bit
        113 => Action::Return(0),
        // graphics_context_set_stroke_color_2bit
        114 => Action::Return(0),
        // graphics_context_set_text_color_2bit
        115 => Action::Return(0),

        // graphics_draw_bitmap_in_rect
        116 => {
            let rect = grect_from_regs(r2, r3);
            eprintln!("[emu] graphics_draw_bitmap_in_rect(bmp=0x{:08x}, {}x{}+{}+{})",
                r1, rect.w, rect.h, rect.x, rect.y);
            pebble_api::pbl_graphics_draw_bitmap_in_rect(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                rect,
            );
            Action::Return(0)
        }

        // graphics_draw_circle(ctx, center: GPoint, radius: u16)
        117 => {
            pebble_api::pbl_graphics_draw_circle(
                &mut state.gctx as *mut PblGContext,
                gpoint_from_reg(r1),
                r2 as u16,
            );
            Action::Return(0)
        }

        // graphics_draw_line(ctx, p0: GPoint, p1: GPoint)
        118 => {
            pebble_api::pbl_graphics_draw_line(
                &mut state.gctx as *mut PblGContext,
                gpoint_from_reg(r1),
                gpoint_from_reg(r2),
            );
            Action::Return(0)
        }

        // graphics_draw_pixel(ctx, point: GPoint)
        119 => {
            pebble_api::pbl_graphics_draw_pixel(
                &mut state.gctx as *mut PblGContext,
                gpoint_from_reg(r1),
            );
            Action::Return(0)
        }

        // graphics_draw_rect(ctx, rect: GRect)
        120 => {
            pebble_api::pbl_graphics_draw_rect(
                &mut state.gctx as *mut PblGContext,
                grect_from_regs(r1, r2),
            );
            Action::Return(0)
        }

        // graphics_draw_round_rect(ctx, rect, corner_radius)
        121 => {
            pebble_api::pbl_graphics_draw_round_rect(
                &mut state.gctx as *mut PblGContext,
                grect_from_regs(r1, r2),
                r3 as u16,
            );
            Action::Return(0)
        }

        // graphics_fill_circle(ctx, center: GPoint, radius: u16)
        122 => {
            pebble_api::pbl_graphics_fill_circle(
                &mut state.gctx as *mut PblGContext,
                gpoint_from_reg(r1),
                r2 as u16,
            );
            Action::Return(0)
        }

        // graphics_fill_rect(ctx, rect: GRect, corner_radius, corner_mask)
        123 => {
            pebble_api::pbl_graphics_fill_rect(
                &mut state.gctx as *mut PblGContext,
                grect_from_regs(r1, r2),
                r3 as u16,
                stack_arg(proc, 0) as u8,
            );
            Action::Return(0)
        }

        // graphics_fill_round_rect
        124 => {
            // TODO
            Action::Return(0)
        }

        // grect_align
        126 => Action::Return(0),

        // grect_center_point(rect: GRect) -> GPoint (4 bytes, returned in r0)
        127 => {
            let rect = grect_from_regs(r0, r1);
            let center = GPoint {
                x: rect.x + rect.w / 2,
                y: rect.y + rect.h / 2,
            };
            Action::Return(gpoint_to_u32(&center))
        }

        // grect_clip(rect, clip_rect) -> GRect  (8 bytes, hidden first param)
        // Signature: void grect_clip(GRect *rect_to_clip, const GRect *clip_box)
        // But Pebble SDK has it as returning GRect... check calling convention
        // Actually in pebble_api.rs it mutates in place. Let's handle inline.
        128 => {
            // r0 = result ptr, r1+r2 = rect, r3+sp[0] = clip
            let mut rect = grect_from_regs(r1, r2);
            let clip = grect_from_regs(r3, stack_arg(proc, 0));
            let x2 = (rect.x + rect.w).min(clip.x + clip.w);
            let y2 = (rect.y + rect.h).min(clip.y + clip.h);
            rect.x = rect.x.max(clip.x);
            rect.y = rect.y.max(clip.y);
            rect.w = (x2 - rect.x).max(0);
            rect.h = (y2 - rect.y).max(0);
            write_grect(proc, r0, &rect);
            Action::Return(r0)
        }

        // grect_contains_point(rect, point) -> bool
        129 => {
            let rect = grect_from_regs(r0, r1);
            let point = gpoint_from_reg(r2);
            let contains = point.x >= rect.x && point.x < rect.x + rect.w
                && point.y >= rect.y && point.y < rect.y + rect.h;
            Action::Return(contains as u32)
        }

        // grect_crop(rect, inset) -> GRect (hidden first param)
        130 => {
            let rect = grect_from_regs(r1, r2);
            let result = pebble_api::pbl_grect_crop(rect, r3 as i16);
            write_grect(proc, r0, &result);
            Action::Return(r0)
        }

        // grect_equal(a, b) -> bool
        131 => {
            let a = grect_from_regs(r0, r1);
            let b = grect_from_regs(r2, r3);
            let eq = a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h;
            Action::Return(eq as u32)
        }

        // grect_is_empty(rect) -> bool
        132 => {
            let rect = grect_from_regs(r0, r1);
            Action::Return((rect.w <= 0 || rect.h <= 0) as u32)
        }

        // grect_standardize(rect) -> GRect (hidden first param)
        133 => {
            let mut rect = grect_from_regs(r1, r2);
            if rect.w < 0 { rect.x += rect.w; rect.w = -rect.w; }
            if rect.h < 0 { rect.y += rect.h; rect.h = -rect.h; }
            write_grect(proc, r0, &rect);
            Action::Return(r0)
        }

        // gsize_equal
        134 => Action::Return((r0 == r1) as u32),

        // =================================================================
        // Layer
        // =================================================================

        // layer_add_child(parent, child)
        138 => {
            let parent_ptr: *mut pebble_api::PblLayer = state.from_handle(r0);
            let child_ptr: *mut pebble_api::PblLayer = state.from_handle(r1);
            eprintln!("[emu] layer_add_child(parent=0x{:08x}->0x{:016x}, child=0x{:08x}->0x{:016x})",
                r0, parent_ptr as usize, r1, child_ptr as usize);
            if parent_ptr.is_null() || child_ptr.is_null() || (parent_ptr as usize) < 0x10000 || (child_ptr as usize) < 0x10000 {
                eprintln!("[emu] layer_add_child: BAD POINTER, skipping");
            } else {
                pebble_api::pbl_layer_add_child(parent_ptr, child_ptr);
            }
            let parent_h = r0;
            let child_h = r1;
            // Write parent pointer into child's PebbleOS Layer struct
            let _ = proc.write_u32_aligned(child_h + LAYER_OFF_PARENT, parent_h);
            // Set window pointer on child to match parent's
            let parent_window = proc.read_u32_aligned(parent_h + LAYER_OFF_WINDOW).unwrap_or(0);
            if parent_window != 0 {
                let _ = proc.write_u32_aligned(child_h + LAYER_OFF_WINDOW, parent_window);
            }
            // Update parent's first_child / sibling chain
            let existing = proc.read_u32_aligned(parent_h + LAYER_OFF_FIRST_CHILD).unwrap_or(0);
            if existing == 0 {
                let _ = proc.write_u32_aligned(parent_h + LAYER_OFF_FIRST_CHILD, child_h);
            } else {
                // Walk sibling list, append at end
                let mut sib = existing;
                for _ in 0..64 { // guard against loops
                    let next = proc.read_u32_aligned(sib + LAYER_OFF_NEXT_SIBLING).unwrap_or(0);
                    if next == 0 {
                        let _ = proc.write_u32_aligned(sib + LAYER_OFF_NEXT_SIBLING, child_h);
                        break;
                    }
                    sib = next;
                }
            }
            Action::Return(0)
        }

        // layer_create(frame: GRect) -> *mut PblLayer
        139 => {
            let frame = grect_from_regs(r0, r1);
            let layer = pebble_api::pbl_layer_create(frame);
            let handle = state.to_handle(layer as usize);
            let bounds = GRect { x: 0, y: 0, w: frame.w, h: frame.h };
            write_emu_layer(proc, handle, &bounds, &frame);
            Action::Return(handle)
        }

        // layer_create_with_data(frame: GRect, data_size: usize) -> *mut PblLayer
        140 => {
            let frame = grect_from_regs(r0, r1);
            let data_size = r2;
            let layer = pebble_api::pbl_layer_create_with_data(frame, data_size as usize);
            let handle = state.to_handle(layer as usize);
            let bounds = GRect { x: 0, y: 0, w: frame.w, h: frame.h };
            write_emu_layer(proc, handle, &bounds, &frame);
            // Allocate data area in emulated heap so the app can dereference the pointer
            if data_size > 0 {
                let emu_addr = state.emu_malloc(proc, data_size);
                state.layer_data_addrs.push((handle, emu_addr));
            }
            Action::Return(handle)
        }

        // layer_destroy
        141 => {
            pebble_api::pbl_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // layer_get_bounds(layer) -> GRect (8 bytes, hidden first param)
        142 => {
            // r0 = result ptr (emulated addr), r1 = layer handle
            let layer = state.from_handle::<PblLayer>(r1);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            write_grect(proc, r0, &bounds);
            Action::Return(r0)
        }

        // layer_get_data(layer) -> *mut u8
        // Returns emulated heap address so the app can dereference it directly
        144 => {
            if let Some(&(_, emu_addr)) = state.layer_data_addrs.iter().find(|&&(h, _)| h == r0) {
                Action::Return(emu_addr)
            } else {
                eprintln!("[emu] WARNING: layer_get_data on handle 0x{:08x} with no emulated data", r0);
                Action::Return(0)
            }
        }

        // layer_get_frame(layer) -> GRect (hidden first param)
        145 => {
            let layer = state.from_handle::<PblLayer>(r1);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_grect(proc, r0, &frame);
            Action::Return(r0)
        }

        // layer_get_hidden
        146 => Action::Return(0),
        // layer_get_window
        147 => Action::Return(state.current_window_handle),

        // layer_insert_above_sibling / below
        148 | 149 => Action::Return(0),

        // layer_mark_dirty
        150 => Action::Return(0),

        // layer_remove_child_layers
        151 => Action::Return(0),
        // layer_remove_from_parent
        152 => Action::Return(0),

        // layer_set_bounds(layer, bounds: GRect)
        153 => {
            let bounds = grect_from_regs(r1, r2);
            pebble_api::pbl_layer_set_bounds(state.from_handle(r0), bounds);
            write_grect(proc, r0 + LAYER_OFF_BOUNDS, &bounds);
            Action::Return(0)
        }

        // layer_set_clips
        154 => Action::Return(0),

        // layer_set_frame(layer, frame: GRect)
        155 => {
            let frame = grect_from_regs(r1, r2);
            pebble_api::pbl_layer_set_frame(state.from_handle(r0), frame);
            write_grect(proc, r0 + LAYER_OFF_FRAME, &frame);
            Action::Return(0)
        }

        // layer_set_hidden
        156 => Action::Return(0),

        // layer_set_update_proc(layer, proc)
        // r0 = layer handle, r1 = emulated function address
        157 => {
            let layer_handle = r0;
            let proc_addr = r1;
            state.update_procs.retain(|(h, _)| *h != layer_handle);
            if proc_addr != 0 {
                state.update_procs.push((layer_handle, proc_addr));
                println!(
                    "[emu] layer_set_update_proc(0x{:08x}, 0x{:08x})",
                    layer_handle, proc_addr
                );
            }
            Action::Return(0)
        }

        // light_enable / light_enable_interaction
        158 | 159 => Action::Return(0),

        // malloc(size)
        161 => {
            let addr = state.emu_malloc(proc, r0);
            Action::Return(addr)
        }

        // memcpy(dst, src, n) — all emulated addresses
        162 => {
            let n = r2;
            for i in 0..n {
                let b = proc.read_u8(r1 + i).unwrap_or(0);
                let _ = proc.write_u8(r0 + i, b);
            }
            Action::Return(r0)
        }

        // memmove(dst, src, n)
        163 => {
            let n = r2 as usize;
            let mut buf = vec![0u8; n];
            for i in 0..n {
                buf[i] = proc.read_u8(r1 + i as u32).unwrap_or(0);
            }
            for (i, &b) in buf.iter().enumerate() {
                let _ = proc.write_u8(r0 + i as u32, b);
            }
            Action::Return(r0)
        }

        // memset(dst, val, n)
        164 => {
            let val = r1 as u8;
            for i in 0..r2 {
                let _ = proc.write_u8(r0 + i, val);
            }
            Action::Return(r0)
        }

        // =================================================================
        // Persist (stubs)
        // =================================================================
        // persist stubs — return 0/false for everything
        // 197 = persist_write_string: return strlen+1 to simulate success
        187..=196 | 311..=313 => Action::Return(0),
        197 => {
            let s = read_cstring(proc, r1);
            Action::Return((s.len() + 1).min(256) as u32)
        }

        // =================================================================
        // Resource API
        // =================================================================

        // resource_get_handle(resource_id) -> handle (just returns the ID)
        206 => Action::Return(r0),

        // resource_load(handle, buffer, max_length) -> bytes_loaded
        207 => {
            if let Some(data) = pebble_api::resource_get_data(r0) {
                let to_copy = data.len().min(r2 as usize);
                for i in 0..to_copy {
                    let _ = proc.write_u8(r1 + i as u32, data[i]);
                }
                Action::Return(to_copy as u32)
            } else {
                Action::Return(0)
            }
        }

        // resource_load_byte_range(handle, start, buffer, num_bytes) -> bytes_loaded
        208 => {
            if let Some(data) = pebble_api::resource_get_data(r0) {
                let start = r1 as usize;
                if start < data.len() {
                    let available = data.len() - start;
                    let to_copy = available.min(r3 as usize);
                    for i in 0..to_copy {
                        let _ = proc.write_u8(r2 + i as u32, data[start + i]);
                    }
                    Action::Return(to_copy as u32)
                } else {
                    Action::Return(0)
                }
            } else {
                Action::Return(0)
            }
        }

        // resource_size(handle) -> size
        209 => {
            if let Some(data) = pebble_api::resource_get_data(r0) {
                Action::Return(data.len() as u32)
            } else {
                Action::Return(0)
            }
        }

        // =================================================================
        // Sin
        // =================================================================

        // graphics_text_layout_get_max_used_size(ctx, text, font, box, overflow, alignment, layout)
        // Returns GSize packed in r0
        125 => {
            // r0=ctx, r1=text, r2=font, r3=box.xy, stack[0]=box.wh, stack[1]=overflow, stack[2]=alignment
            let box_wh = stack_arg(proc, 0);
            let box_rect = GRect {
                x: r3 as i16, y: (r3 >> 16) as i16,
                w: box_wh as i16, h: (box_wh >> 16) as i16,
            };
            let text_str = read_cstring(proc, r1);
            let font_ptr: *const u8 = state.from_handle(r2);
            let size = pebble_api::measure_text_content(&text_str, font_ptr, box_rect);
            Action::Return((size.w as u16 as u32) | ((size.h as u16 as u32) << 16))
        }

        // layer_get_clips
        143 => Action::Return(1), // true

        // localtime__deprecated (same as localtime)
        160 => {
            let time_val = if r0 != 0 {
                proc.read_u32_aligned(r0).unwrap_or(0) as i64
            } else {
                unsafe { libc::time(std::ptr::null_mut()) }
            };
            let tm = unsafe { *libc::localtime(&time_val) };
            state.cached_tm = Some(tm);
            write_tm(proc, TM_BUF_ADDR, &tm);
            eprintln!("[emu] localtime() -> 0x{:08x}", TM_BUF_ADDR);
            Action::Return(TM_BUF_ADDR)
        }

        // rand
        205 => Action::Return(unsafe { libc::rand() } as u32),

        // sin_lookup
        238 => Action::Return(pebble_api::pbl_sin_lookup(r0 as i32) as u32),

        // snprintf(buf, size, fmt, ...) — simplified
        239 => {
            let fmt = read_cstring(proc, r2);
            // Simple format: replace %d with arg, %s with string, etc.
            let result = simple_snprintf(proc, &fmt, r3, 0);
            let bytes = result.as_bytes();
            let max_len = (r1 as usize).saturating_sub(1);
            let write_len = bytes.len().min(max_len);
            write_bytes(proc, r0, &bytes[..write_len]);
            let _ = proc.write_u8(r0 + write_len as u32, 0); // null terminate
            Action::Return(write_len as u32)
        }

        // strcmp(a, b)
        242 => {
            let a = read_cstring(proc, r0);
            let b = read_cstring(proc, r1);
            Action::Return(a.cmp(&b) as i32 as u32)
        }

        // strftime(buf, maxsize, format, tm)
        244 => {
            let fmt = read_cstring(proc, r2);
            // Use cached tm from last localtime call
            if let Some(ref tm) = state.cached_tm {
                let result = host_strftime(&fmt, tm);
                let bytes = result.as_bytes();
                let max_len = (r1 as usize).saturating_sub(1);
                let write_len = bytes.len().min(max_len);
                write_bytes(proc, r0, &bytes[..write_len]);
                let _ = proc.write_u8(r0 + write_len as u32, 0);
                Action::Return(write_len as u32)
            } else {
                let _ = proc.write_u8(r0, 0);
                Action::Return(0)
            }
        }

        // strlen
        245 => {
            let s = read_cstring(proc, r0);
            Action::Return(s.len() as u32)
        }

        // srand
        240 => {
            unsafe { libc::srand(r0 as u32); }
            Action::Return(0)
        }

        // strcat(dst, src)
        241 => {
            let src = read_cstring(proc, r1);
            let dst_str = read_cstring(proc, r0);
            let offset = dst_str.len() as u32;
            write_bytes(proc, r0 + offset, src.as_bytes());
            let _ = proc.write_u8(r0 + offset + src.len() as u32, 0);
            Action::Return(r0)
        }

        // strcpy(dst, src)
        243 => {
            let src = read_cstring(proc, r1);
            write_bytes(proc, r0, src.as_bytes());
            let _ = proc.write_u8(r0 + src.len() as u32, 0);
            Action::Return(r0)
        }

        // strncat(dst, src, n)
        246 => {
            let src = read_cstring(proc, r1);
            let dst_str = read_cstring(proc, r0);
            let offset = dst_str.len() as u32;
            let n = r2 as usize;
            let copy_len = src.len().min(n);
            write_bytes(proc, r0 + offset, &src.as_bytes()[..copy_len]);
            let _ = proc.write_u8(r0 + offset + copy_len as u32, 0);
            Action::Return(r0)
        }

        // strncmp
        247 => {
            let a = read_cstring(proc, r0);
            let b = read_cstring(proc, r1);
            let n = r2 as usize;
            let result = a[..a.len().min(n)].cmp(&b[..b.len().min(n)]);
            Action::Return(result as i32 as u32)
        }

        // strncpy(dst, src, n)
        248 => {
            let src = read_cstring(proc, r1);
            let n = r2 as usize;
            let copy_len = src.len().min(n);
            write_bytes(proc, r0, &src.as_bytes()[..copy_len]);
            // Pad with zeros
            for i in copy_len..n {
                let _ = proc.write_u8(r0 + i as u32, 0);
            }
            Action::Return(r0)
        }

        // =================================================================
        // Tick timer
        // =================================================================

        // tick_timer_service_subscribe(units, handler)
        262 => {
            state.tick_units = r0;
            state.tick_handler_addr = r1;
            println!(
                "[emu] tick_timer_service_subscribe(units=0x{:x}, handler=0x{:08x})",
                r0, r1
            );
            Action::Return(0)
        }

        // tick_timer_service_unsubscribe
        263 => {
            state.tick_handler_addr = 0;
            Action::Return(0)
        }

        // time__deprecated
        264 => {
            let now = unsafe { libc::time(std::ptr::null_mut()) } as u32;
            if r0 != 0 {
                let _ = proc.write_u32_aligned(r0, now);
            }
            Action::Return(now)
        }

        // time_ms_deprecated / time_ms
        265 | 532 => {
            let now = unsafe { libc::time(std::ptr::null_mut()) } as u32;
            if r0 != 0 {
                let _ = proc.write_u32_aligned(r0, now);
            }
            if r1 != 0 {
                let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
                unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()); }
                let _ = proc.write_u16_aligned(r1, (tv.tv_usec / 1000) as u16);
            }
            Action::Return(now)
        }

        // =================================================================
        // Vibes (stubs)
        // =================================================================
        266..=270 => Action::Return(0),

        // =================================================================
        // Window
        // =================================================================

        // window_create
        271 => {
            let window = pebble_api::pbl_window_create();
            let h = state.to_handle(window as usize);
            // Write root layer fields into the Window handle (Window embeds Layer at offset 0)
            let display_w = pebble_api::DISPLAY_WIDTH_I16;
            let display_h = pebble_api::DISPLAY_HEIGHT_I16;
            let bounds = GRect { x: 0, y: 0, w: display_w, h: display_h };
            write_emu_layer(proc, h, &bounds, &bounds);
            println!("[emu] window_create() -> 0x{:08x}", h);
            Action::Return(h)
        }

        // window_destroy
        272 => {
            pebble_api::pbl_window_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // window_get_click_config_provider
        273 => Action::Return(0),
        // window_get_fullscreen
        274 => Action::Return(1),

        // window_get_root_layer
        275 => {
            let layer = pebble_api::pbl_window_get_root_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            eprintln!("[emu] window_get_root_layer(0x{:08x}) -> handle 0x{:08x}", r0, h);
            // Write PebbleOS Layer fields for the root layer
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            // Set window pointer back to the window handle
            let _ = proc.write_u32_aligned(h + LAYER_OFF_WINDOW, r0);
            Action::Return(h)
        }

        // window_is_loaded
        276 => Action::Return(1),

        // window_set_background_color_2bit
        277 => Action::Return(0),

        // window_set_click_config_provider
        278 => {
            state.click_config_provider = r1;
            state.click_config_context = 0;
            Action::Return(0)
        }
        // window_set_click_config_provider_with_context
        279 => {
            state.click_config_provider = r1;
            state.click_config_context = r2;
            Action::Return(0)
        }
        // window_set_fullscreen
        280 => Action::Return(0),
        // window_set_status_bar_icon
        281 => Action::Return(0),

        // window_set_window_handlers(window, load, appear, disappear, unload_on_stack)
        // ARM AAPCS: WindowHandlers (16 bytes) = r1,r2,r3,sp[0]
        282 => {
            state.window_load_handler = r1;
            state.window_unload_handler = stack_arg(proc, 0);
            println!(
                "[emu] window_set_window_handlers(load=0x{:08x}, unload=0x{:08x})",
                r1,
                state.window_unload_handler
            );
            Action::Return(0)
        }

        // window_stack_contains_window
        283 => Action::Return(1), // true
        // window_stack_get_top_window
        284 => Action::Return(state.current_window_handle),

        // window_stack_pop
        285 => Action::Return(0),

        // window_stack_pop_all
        286 => Action::Return(0),

        // window_stack_push(window, animated)
        // PebbleOS calls the window load handler synchronously here
        287 => {
            state.current_window_handle = r0;
            println!("[emu] window_stack_push(0x{:08x})", r0);
            if state.window_load_handler != 0 {
                println!("[emu] calling window load handler 0x{:08x}", state.window_load_handler);
                let _ = call_callback(proc, state, state.window_load_handler, &[r0]);
            }
            Action::Return(0)
        }

        // window_stack_remove
        288 => Action::Return(0),

        // app_focus_service_subscribe / unsubscribe / subscribe_handlers
        289 | 290 | 535 => Action::Return(0),

        // window_get_user_data
        291 => {
            let data = pebble_api::pbl_window_get_user_data(state.from_handle(r0));
            Action::Return(data as u32)
        }

        // window_set_user_data (alternate index?)
        292 => {
            pebble_api::pbl_window_set_user_data(state.from_handle(r0), r1 as usize);
            Action::Return(0)
        }

        // Click handlers (stubs) — 303-305 are stubs, 306-307 handled below (click handling section)

        // =================================================================
        // Graphics draw text
        // =================================================================

        // graphics_draw_text(ctx, text, font, box, overflow, alignment, attrs)
        // r0=ctx, r1=text(emu), r2=font(handle), r3=box.lo, sp[0]=box.hi,
        // sp[1]=overflow, sp[2]=alignment
        309 => {
            let text = read_cstring(proc, r1);
            let font = state.from_handle::<u8>(r2);
            let rect = grect_from_regs(r3, stack_arg(proc, 0));
            let alignment = stack_arg(proc, 2) as u8;
            // Draw text directly to framebuffer
            pebble_api::pbl_graphics_draw_text_direct(
                &mut state.gctx as *mut PblGContext,
                &text,
                font,
                rect,
                alignment,
            );
            Action::Return(0)
        }

        // graphics_text_layout_get_content_size(text, font, box, overflow, alignment)
        // r0=text, r1=font, r2=box.xy, r3=box.wh, stack[0]=overflow, stack[1]=alignment
        // Returns GSize packed in r0
        315 => {
            let box_rect = GRect {
                x: r2 as i16, y: (r2 >> 16) as i16,
                w: r3 as i16, h: (r3 >> 16) as i16,
            };
            let text_str = read_cstring(proc, r0);
            let font_ptr: *const u8 = state.from_handle(r1);
            let size = pebble_api::measure_text_content(&text_str, font_ptr, box_rect);
            Action::Return((size.w as u16 as u32) | ((size.h as u16 as u32) << 16))
        }

        // calloc(n, size)
        318 => {
            let total = r0.wrapping_mul(r1);
            let addr = state.emu_malloc(proc, total);
            Action::Return(addr)
        }

        // bitmap_layer_get_bitmap
        319 => {
            let bmp = pebble_api::pbl_bitmap_layer_get_bitmap(state.from_handle(r0));
            if bmp.is_null() {
                Action::Return(0)
            } else {
                Action::Return(state.to_handle(bmp as usize))
            }
        }

        // realloc(ptr, size) — bump allocator: just malloc new
        323 => {
            if r1 == 0 {
                Action::Return(0) // realloc(p, 0) = free
            } else {
                let new_addr = state.emu_malloc(proc, r1);
                // Copy old data if ptr was valid
                if r0 != 0 {
                    // We don't know old size, copy up to new size
                    for i in 0..r1 {
                        let b = proc.read_u8(r0 + i).unwrap_or(0);
                        let _ = proc.write_u8(new_addr + i, b);
                    }
                }
                Action::Return(new_addr)
            }
        }

        // gpath_draw_filled (modern)
        343 => {
            pebble_api::pbl_gpath_draw_filled(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }

        // watch_info_get_color
        345 => Action::Return(1),
        // watch_info_get_model
        347 => Action::Return(4),

        // graphics_capture_frame_buffer (2bit compat)
        348 | 394 => {
            let bmp = pebble_api::pbl_graphics_capture_frame_buffer(
                &mut state.gctx as *mut PblGContext,
            );
            let h = state.to_handle(bmp as usize);
            // Allocate emulated framebuffer if not yet done
            let fb_size = (DISPLAY_WIDTH * DISPLAY_HEIGHT) as u32;
            if state.emu_fb_addr == 0 {
                let aligned = (fb_size + 7) & !7;
                let addr = HEAP_BASE + state.heap_offset;
                state.heap_offset += aligned;
                state.emu_fb_addr = addr;
            }
            // Copy host framebuffer into emulated memory so apps can read it
            if let Some(fb_ptr) = pebble_api::get_framebuffer_ptr() {
                for i in 0..fb_size {
                    let byte = unsafe { *fb_ptr.add(i as usize) };
                    let _ = proc.write_u8(state.emu_fb_addr + i, byte);
                }
            }
            // Write GBitmap struct fields into handle RAM for direct access
            let w = DISPLAY_WIDTH as u16;
            let h_val = DISPLAY_HEIGHT as u16;
            let _ = proc.write_u32_aligned(h, state.emu_fb_addr); // data pointer
            let _ = proc.write_u16_aligned(h + 4, w); // row_size_bytes
            let _ = proc.write_u16_aligned(h + 6, 1); // info_flags: 8Bit
            let _ = proc.write_u16_aligned(h + 8, 0); // bounds.x
            let _ = proc.write_u16_aligned(h + 10, 0); // bounds.y
            let _ = proc.write_u16_aligned(h + 12, w); // bounds.w
            let _ = proc.write_u16_aligned(h + 14, h_val); // bounds.h
            state.captured_fb_handle = h;
            Action::Return(h)
        }

        // graphics_frame_buffer_is_captured
        349 => Action::Return(pebble_api::pbl_graphics_frame_buffer_is_captured(
            &mut state.gctx as *mut PblGContext,
        ) as u32),

        // graphics_release_frame_buffer
        350 => {
            // Copy emulated framebuffer back to host before releasing
            if state.emu_fb_addr != 0 {
                let fb_size = (DISPLAY_WIDTH * DISPLAY_HEIGHT) as u32;
                if let Some(fb_ptr) = pebble_api::get_framebuffer_ptr() {
                    for i in 0..fb_size {
                        if let Ok(byte) = proc.read_u8(state.emu_fb_addr + i) {
                            unsafe { *fb_ptr.add(i as usize) = byte; }
                        }
                    }
                }
            }
            pebble_api::pbl_graphics_release_frame_buffer(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            state.captured_fb_handle = 0;
            Action::Return(0)
        }

        // clock_to_timestamp(weekday, hour, minute)
        351 => Action::Return(pebble_api::pbl_clock_to_timestamp(
            r0 as u8, r1 as u8, r2 as u8,
        ) as u32),

        // i18n_get_system_locale() -> const char*
        360 => {
            if state.locale_addr == 0 {
                let locale = b"en_US\0";
                let addr = state.emu_malloc(proc, locale.len() as u32);
                for (i, &b) in locale.iter().enumerate() {
                    proc.write_u8(addr + i as u32, b).ok();
                }
                state.locale_addr = addr;
            }
            Action::Return(state.locale_addr)
        }

        // setlocale(category, locale) -> const char*
        362 => {
            // Return a pointer to "C" locale string in emulated memory
            if state.locale_addr == 0 {
                let locale = b"en_US\0";
                let addr = state.emu_malloc(proc, locale.len() as u32);
                for (i, &b) in locale.iter().enumerate() {
                    proc.write_u8(addr + i as u32, b).ok();
                }
                state.locale_addr = addr;
            }
            Action::Return(state.locale_addr)
        }

        // mktime(tm*) -> time_t
        363 => {
            if r0 != 0 {
                // Read tm struct from emulated memory and convert
                let sec = proc.read_u32_aligned(r0).unwrap_or(0) as i32;
                let min = proc.read_u32_aligned(r0 + 4).unwrap_or(0) as i32;
                let hour = proc.read_u32_aligned(r0 + 8).unwrap_or(0) as i32;
                let mday = proc.read_u32_aligned(r0 + 12).unwrap_or(1) as i32;
                let mon = proc.read_u32_aligned(r0 + 16).unwrap_or(0) as i32;
                let year = proc.read_u32_aligned(r0 + 20).unwrap_or(70) as i32;
                let mut tm: libc::tm = unsafe { std::mem::zeroed() };
                tm.tm_sec = sec; tm.tm_min = min; tm.tm_hour = hour;
                tm.tm_mday = mday; tm.tm_mon = mon; tm.tm_year = year;
                let result = unsafe { libc::mktime(&mut tm) };
                state.cached_tm = Some(tm);
                Action::Return(result as u32)
            } else {
                Action::Return(0)
            }
        }

        // =================================================================
        // Graphics context set color (modern 8-bit)
        // =================================================================

        // bitmap_layer_set_background_color
        370 => Action::Return(0),

        // graphics_context_set_fill_color(ctx, color: GColor8)
        371 => {
            state.gctx.fill_color = r1 as u8;
            Action::Return(0)
        }

        // graphics_context_set_stroke_color
        372 => {
            state.gctx.stroke_color = r1 as u8;
            Action::Return(0)
        }

        // graphics_context_set_text_color
        373 => {
            state.gctx.text_color = r1 as u8;
            Action::Return(0)
        }

        // window_set_background_color
        377 => {
            // Fill framebuffer with background color
            if let Some(fb) = pebble_api::get_fb() {
                fb.fill(r1 as u8);
            }
            Action::Return(0)
        }

        // clock_get_timezone
        378 => {
            // Return pointer to "UTC" in emulated memory
            let tz_addr = state.emu_malloc(proc, 4);
            write_bytes(proc, tz_addr, b"UTC\0");
            Action::Return(tz_addr)
        }

        // localtime(time_ptr) -> *mut tm
        379 => {
            let time_val = proc.read_u32_aligned(r0).unwrap_or(0) as libc::time_t;
            let tm = unsafe { *libc::localtime(&time_val) };
            state.cached_tm = Some(tm);
            write_tm(proc, TM_BUF_ADDR, &tm);
            Action::Return(TM_BUF_ADDR)
        }

        // gbitmap_create_blank
        393 => {
            let bmp = pebble_api::pbl_gbitmap_create_blank(r0 as i16, r1 as i16);
            let h = state.to_handle(bmp as usize);
            write_emu_bitmap(proc, state, h, bmp as *const _);
            Action::Return(h)
        }

        // graphics_capture_frame_buffer_format
        395 => {
            let bmp = pebble_api::pbl_graphics_capture_frame_buffer_format(
                &mut state.gctx as *mut PblGContext,
                r1 as u8,
            );
            let h = state.to_handle(bmp as usize);
            // Allocate emulated framebuffer if not yet done
            let fb_size = (DISPLAY_WIDTH * DISPLAY_HEIGHT) as u32;
            if state.emu_fb_addr == 0 {
                let aligned = (fb_size + 7) & !7;
                let addr = HEAP_BASE + state.heap_offset;
                state.heap_offset += aligned;
                state.emu_fb_addr = addr;
            }
            if let Some(fb_ptr) = pebble_api::get_framebuffer_ptr() {
                for i in 0..fb_size {
                    let byte = unsafe { *fb_ptr.add(i as usize) };
                    let _ = proc.write_u8(state.emu_fb_addr + i, byte);
                }
            }
            // Write GBitmap struct fields into handle RAM
            let w = DISPLAY_WIDTH as u16;
            let h_val = DISPLAY_HEIGHT as u16;
            let _ = proc.write_u32_aligned(h, state.emu_fb_addr);
            let _ = proc.write_u16_aligned(h + 4, w);
            let _ = proc.write_u16_aligned(h + 6, 1);
            let _ = proc.write_u16_aligned(h + 8, 0);
            let _ = proc.write_u16_aligned(h + 10, 0);
            let _ = proc.write_u16_aligned(h + 12, w);
            let _ = proc.write_u16_aligned(h + 14, h_val);
            state.captured_fb_handle = h;
            Action::Return(h)
        }

        // gbitmap_get_bounds -> GRect (hidden first param)
        407 => {
            let bounds = pebble_api::pbl_gbitmap_get_bounds(state.from_handle(r1));
            write_grect(proc, r0, &bounds);
            Action::Return(r0)
        }

        // gbitmap_get_bytes_per_row
        408 => {
            Action::Return(pebble_api::pbl_gbitmap_get_bytes_per_row(state.from_handle(r0)) as u32)
        }

        // gbitmap_get_data -> pointer to emulated memory
        409 => {
            // If this is the captured framebuffer, return the emulated fb address
            if r0 == state.captured_fb_handle && state.emu_fb_addr != 0 {
                Action::Return(state.emu_fb_addr)
            } else {
                // Handle-based bitmap — copy data to emu heap
                let bmp: *const pebble_api::PblGBitmap = state.from_handle(r0) as *const _;
                if !bmp.is_null() {
                    let bmp_ref = unsafe { &*bmp };
                    if !bmp_ref.data.is_null() {
                        let data_size = bmp_ref.row_size_bytes as u32 * bmp_ref.bounds.h as u32;
                        let emu_addr = state.emu_malloc(proc, data_size);
                        for i in 0..data_size {
                            let byte = unsafe { *bmp_ref.data.add(i as usize) };
                            let _ = proc.write_u8(emu_addr + i, byte);
                        }
                        Action::Return(emu_addr)
                    } else {
                        Action::Return(0)
                    }
                } else {
                    Action::Return(0)
                }
            }
        }

        // gbitmap_get_format
        410 => Action::Return(pebble_api::pbl_gbitmap_get_format(state.from_handle(r0)) as u32),

        // gbitmap_get_palette(bitmap) -> palette pointer
        411 => {
            let palette = pebble_api::pbl_gbitmap_get_palette(state.from_handle(r0));
            if palette.is_null() {
                Action::Return(0)
            } else {
                // Copy palette data into emulated memory so the app can read it.
                // Palette size depends on format: 2 colors (1-bit), 4 (2-bit), 16 (4-bit).
                let bmp: *const pebble_api::PblGBitmap = state.from_handle(r0) as *const _;
                let format = if !bmp.is_null() { unsafe { (*bmp).info_flags } } else { 0 };
                let num_colors: usize = match format {
                    2 => 2,   // 1-bit palette
                    3 => 4,   // 2-bit palette
                    4 => 16,  // 4-bit palette
                    _ => 4,   // safe default
                };
                let palette_bytes = num_colors; // each entry is 1 byte (GColor8)
                let emu_addr = state.emu_malloc(proc, palette_bytes as u32);
                for i in 0..palette_bytes {
                    let byte = unsafe { *palette.add(i) };
                    let _ = proc.write_u8(emu_addr + i as u32, byte);
                }
                Action::Return(emu_addr)
            }
        }

        // gbitmap_set_palette(bitmap, palette, free_on_destroy)
        414 => {
            let bmp: *mut pebble_api::PblGBitmap = state.from_handle(r0);
            let palette: *mut u8 = if r1 != 0 { state.from_handle(r1) } else { std::ptr::null_mut() };
            let free_on_destroy = r2 != 0;
            pebble_api::pbl_gbitmap_set_palette(bmp, palette, free_on_destroy);
            Action::Return(0)
        }

        // gbitmap_get_data_row_info(bitmap, y) -> GBitmapDataRowInfo (8 bytes, hidden return ptr)
        // ARM EABI: r0=return_ptr, r1=bitmap_handle, r2=y
        571 => {
            let ret_ptr = r0;
            let bmp: *const pebble_api::PblGBitmap = state.from_handle(r1) as *const _;
            let y = r2 as u16;
            if !bmp.is_null() {
                let bmp_ref = unsafe { &*bmp };
                let row_size = bmp_ref.row_size_bytes as u32;
                let width = bmp_ref.bounds.w;
                // Ensure we have an emulated buffer large enough for one row
                if state.data_row_buf_addr == 0 || row_size > state.data_row_buf_size {
                    let alloc_size = row_size.max(512); // at least 512 bytes
                    state.data_row_buf_addr = state.emu_malloc(proc, alloc_size);
                    state.data_row_buf_size = alloc_size;
                }
                // Copy row data from host bitmap into emulated buffer
                let row_offset = (y as usize) * (row_size as usize);
                let src = unsafe { bmp_ref.data.add(row_offset) };
                for i in 0..row_size {
                    let b = unsafe { *src.add(i as usize) };
                    let _ = proc.write_u8(state.data_row_buf_addr + i, b);
                }
                // Write GBitmapDataRowInfo to hidden return pointer:
                //   u32 data (offset 0), i16 min_x (offset 4), i16 max_x (offset 6)
                let _ = proc.write_u32_aligned(ret_ptr, state.data_row_buf_addr);
                let _ = proc.write_u16_aligned(ret_ptr + 4, 0u16); // min_x = 0
                let max_x = if width > 0 { (width - 1) as u16 } else { 0u16 };
                let _ = proc.write_u16_aligned(ret_ptr + 6, max_x);
            } else {
                // Null bitmap: zero out the return struct
                let _ = proc.write_u32_aligned(ret_ptr, 0);
                let _ = proc.write_u32_aligned(ret_ptr + 4, 0);
            }
            Action::Return(ret_ptr)
        }

        // graphics_context_set_antialiased
        444 => Action::Return(0),

        // graphics_context_set_stroke_width
        445 => {
            state.gctx.stroke_width = r1 as u8;
            Action::Return(0)
        }

        // =================================================================
        // Text layer
        // =================================================================

        // text_layer_create(frame: GRect) -> handle
        462 => {
            let tl = pebble_api::pbl_text_layer_create(grect_from_regs(r0, r1));
            let h = state.to_handle(tl as usize);
            Action::Return(h)
        }

        // text_layer_destroy
        463 => {
            pebble_api::pbl_text_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // text_layer_get_content_size -> GSize {w, h} packed in u32
        464 => {
            let sz = pebble_api::pbl_text_layer_get_content_size(state.from_handle(r0));
            // GSize is {i16 w, i16 h} = 4 bytes, returned packed: low 16 = w, high 16 = h
            let packed = (sz.w as u16 as u32) | ((sz.h as u16 as u32) << 16);
            Action::Return(packed)
        }

        // text_layer_get_layer
        465 => {
            let layer = pebble_api::pbl_text_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // text_layer_get_text — return text pointer in emulated memory
        466 => {
            let text_ptr = pebble_api::pbl_text_layer_get_text(state.from_handle(r0));
            if text_ptr.is_null() {
                Action::Return(0)
            } else {
                let cstr = unsafe { std::ffi::CStr::from_ptr(text_ptr) };
                let bytes = cstr.to_bytes_with_nul();
                let emu_addr = state.emu_malloc(proc, bytes.len() as u32);
                for (i, &b) in bytes.iter().enumerate() {
                    let _ = proc.write_u8(emu_addr + i as u32, b);
                }
                Action::Return(emu_addr)
            }
        }

        // text_layer_set_background_color
        467 => {
            pebble_api::pbl_text_layer_set_background_color(
                state.from_handle(r0),
                GColor8(r1 as u8),
            );
            Action::Return(0)
        }

        // text_layer_set_font
        468 => {
            pebble_api::pbl_text_layer_set_font(state.from_handle(r0), state.from_handle(r1));
            Action::Return(0)
        }

        // text_layer_set_overflow_mode
        469 => {
            pebble_api::pbl_text_layer_set_overflow_mode(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // text_layer_set_text(tl, text)
        // text is a pointer into emulated memory — we copy it to host
        471 => {
            let text = read_cstring(proc, r1);
            state.set_text_for_handle(r0, &text);
            Action::Return(0)
        }

        // text_layer_set_text_alignment
        472 => {
            pebble_api::pbl_text_layer_set_text_alignment(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // text_layer_set_text_color
        473 => {
            pebble_api::pbl_text_layer_set_text_color(state.from_handle(r0), GColor8(r1 as u8));
            Action::Return(0)
        }

        // gpath_draw_outline_open
        518 => {
            pebble_api::pbl_gpath_draw_outline_open(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }

        // time(ptr) -> time_t
        519 => {
            let mut now: libc::time_t = 0;
            unsafe {
                libc::time(&mut now);
            }
            if r0 != 0 {
                let _ = proc.write_u32_aligned(r0, now as u32);
            }
            Action::Return(now as u32)
        }

        // difftime(time1, time0) -> double, but Pebble returns int seconds
        531 => Action::Return(r0.wrapping_sub(r1)),

        // gcolor_legible_over
        533 => Action::Return(pebble_api::pbl_gcolor_legible_over(GColor8(r0 as u8)).0 as u32),

        // connection_service_peek_*
        566..=569 => Action::Return(0),

        // grect_inset(rect, insets) -> GRect (hidden first param)
        580 => {
            // Simplified: just return the input rect
            let rect = grect_from_regs(r1, r2);
            write_grect(proc, r0, &rect);
            Action::Return(r0)
        }

        // graphics_draw_arc(ctx, rect: GRect, scale_mode, angle_start, angle_end)
        582 => {
            pebble_api::pbl_graphics_draw_arc(
                &mut state.gctx as *mut PblGContext,
                grect_from_regs(r1, r2),
                r3 as u8,
                stack_arg(proc, 0) as i32,
                stack_arg(proc, 1) as i32,
            );
            Action::Return(0)
        }

        // graphics_fill_radial(ctx, rect: GRect, scale_mode, inset, angle_start, angle_end)
        583 => {
            pebble_api::pbl_graphics_fill_radial(
                &mut state.gctx as *mut PblGContext,
                grect_from_regs(r1, r2),
                r3 as u8,
                stack_arg(proc, 0) as u16,
                stack_arg(proc, 1) as i32,
                stack_arg(proc, 2) as i32,
            );
            Action::Return(0)
        }

        // gpoint_from_polar(bounds, scale_mode, angle) -> GPoint (4 bytes, returned in r0)
        581 => {
            let rect = grect_from_regs(r0, r1);
            let scale_mode = r2 as u8;
            let angle = r3 as i32;
            let result = pebble_api::pbl_gpoint_from_polar(rect, scale_mode, angle);
            Action::Return(gpoint_to_u32(&result))
        }

        // grect_centered_from_polar(rect, scale_mode, angle, size) -> GRect (hidden first param)
        584 => {
            // r0 = result ptr, r1+r2 = rect, r3 = scale_mode, sp[0] = angle, sp[1] = size
            let rect = grect_from_regs(r1, r2);
            let scale_mode = r3 as u8;
            let angle = stack_arg(proc, 0) as i32;
            let size = gpoint_from_reg(stack_arg(proc, 1));
            let result = pebble_api::pbl_grect_centered_from_polar(
                rect,
                scale_mode,
                angle,
                size,
            );
            write_grect(proc, r0, &result);
            Action::Return(r0)
        }

        // health_service_events_subscribe(handler, context) -> true
        601 => Action::Return(1),

        // health_service_metric_accessible(metric, time_start, time_end) -> 0 (not available)
        604 => Action::Return(0),

        // health_service_sum_today / peek_current_value
        607 | 620 => Action::Return(0),

        // time_start_of_today() -> time_t at midnight local time today
        608 => {
            let ts = unsafe {
                let now = libc::time(std::ptr::null_mut());
                let tm = &*libc::localtime(&now);
                let mut midnight = *tm;
                midnight.tm_sec = 0;
                midnight.tm_min = 0;
                midnight.tm_hour = 0;
                midnight.tm_isdst = -1;
                libc::mktime(&mut midnight) as u32
            };
            Action::Return(ts)
        }

        // layer_get_unobstructed_bounds(layer) -> GRect (hidden first param)
        // No timeline peek, so unobstructed == full bounds
        622 => {
            let layer = state.from_handle::<PblLayer>(r1);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            write_grect(proc, r0, &bounds);
            Action::Return(r0)
        }

        // unobstructed_area_service_subscribe — no-op
        624 => {
            eprintln!("[emu] unobstructed_area_service_subscribe: no-op stub");
            Action::Return(0)
        }

        // gcolor_equal / gcolor_equal__deprecated
        364 | 613 => Action::Return((r0 as u8 == r1 as u8) as u32),

        // =================================================================
        // Accelerometer
        // =================================================================

        // accel_data_service_subscribe (0=deprecated, 317=modern)
        0 | 317 => {
            println!("[emu] accel_data_service_subscribe(samples_per_update={}, handler=0x{:08x})", r0, r1);
            state.accel_handler_addr = r1;
            state.accel_samples_per_update = if r0 == 0 { 1 } else { r0 };
            state.accel_last_poll = None;
            Action::Return(0)
        }

        // accel_data_service_unsubscribe
        1 => {
            println!("[emu] accel_data_service_unsubscribe");
            state.accel_handler_addr = 0;
            state.accel_samples_per_update = 0;
            Action::Return(0)
        }

        // accel_service_peek(data_ptr)
        2 => {
            // Write AccelData to emulated memory at r0
            let data = crate::accel::peek_latest();
            let addr = r0;
            // AccelData is packed: i16 x, i16 y, i16 z, u8 did_vibrate, u64 timestamp = 15 bytes
            write_accel_data(proc, addr, &data);
            Action::Return(0) // success
        }

        // accel_service_set_samples_per_update
        3 => {
            println!("[emu] accel_service_set_samples_per_update({})", r0);
            state.accel_samples_per_update = if r0 == 0 { 1 } else { r0 };
            Action::Return(0)
        }

        // accel_service_set_sampling_rate
        4 => {
            println!("[emu] accel_service_set_sampling_rate({})", r0);
            state.accel_sampling_rate = match r0 {
                10 | 25 | 50 | 100 => r0,
                _ => 25,
            };
            Action::Return(0)
        }

        // accel_tap_service_subscribe / unsubscribe (stubs)
        5 | 6 => Action::Return(0),

        // =================================================================
        // Animation (legacy 17-28, modern 380-437, property 396-405/516/517/534)
        // =================================================================

        // animation_create / animation_legacy2_create
        17 | 380 => {
            let anim = pebble_api::pbl_animation_create();
            Action::Return(state.to_handle(anim as usize))
        }
        // animation_destroy / animation_legacy2_destroy
        18 | 381 => {
            pebble_api::pbl_animation_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // animation_get_context / animation_legacy2_get_context
        19 | 382 => Action::Return(pebble_api::pbl_animation_get_context(state.from_handle(r0)) as u32),
        // animation_is_scheduled / animation_legacy2_is_scheduled
        20 | 383 => Action::Return(pebble_api::pbl_animation_is_scheduled(state.from_handle(r0)) as u32),
        // animation_schedule / animation_legacy2_schedule
        21 | 384 => {
            pebble_api::pbl_animation_schedule(state.from_handle(r0));
            Action::Return(0)
        }
        // animation_set_curve / animation_legacy2_set_curve
        22 | 385 => {
            pebble_api::pbl_animation_set_curve(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }
        // animation_set_delay / animation_legacy2_set_delay
        23 | 387 => {
            pebble_api::pbl_animation_set_delay(state.from_handle(r0), r1);
            Action::Return(0)
        }
        // animation_set_duration / animation_legacy2_set_duration
        24 | 388 => {
            pebble_api::pbl_animation_set_duration(state.from_handle(r0), r1);
            Action::Return(0)
        }
        // animation_set_handlers / animation_legacy2_set_handlers
        // Store emulated handler addresses on the animation for later dispatch
        25 | 389 => {
            let anim: *mut pebble_api::PblAnimation = state.from_handle(r0);
            if !anim.is_null() {
                unsafe {
                    (*anim).emu_started_handler = r1; // AnimationHandlers.started
                    (*anim).emu_stopped_handler = r2; // AnimationHandlers.stopped
                    (*anim).context = r3 as usize;    // context
                }
            }
            Action::Return(0)
        }
        // animation_set_implementation / animation_legacy2_set_implementation
        26 | 390 => Action::Return(0),
        // animation_unschedule / animation_legacy2_unschedule
        27 | 391 => {
            pebble_api::pbl_animation_unschedule(state.from_handle(r0));
            Action::Return(0)
        }
        // animation_unschedule_all / animation_legacy2_unschedule_all
        28 | 392 => {
            pebble_api::pbl_animation_unschedule_all();
            Action::Return(0)
        }
        // animation_set_custom_curve / animation_legacy2_set_custom_curve
        344 | 386 => Action::Return(0),

        // property_animation_create
        396 => {
            let anim = pebble_api::pbl_animation_create();
            Action::Return(state.to_handle(anim as usize))
        }
        // property_animation_create_layer_frame / legacy
        199 | 397 => {
            let layer_handle = r0;
            let layer: *mut pebble_api::PblLayer = state.from_handle(r0);
            // from/to are nullable GRect pointers in emulated memory
            let from = if r1 != 0 {
                let packed = (proc.read_u32_aligned(r1).unwrap_or(0), proc.read_u32_aligned(r1 + 4).unwrap_or(0));
                Some(grect_from_regs(packed.0, packed.1))
            } else { None };
            let to = if r2 != 0 {
                let packed = (proc.read_u32_aligned(r2).unwrap_or(0), proc.read_u32_aligned(r2 + 4).unwrap_or(0));
                Some(grect_from_regs(packed.0, packed.1))
            } else { None };
            let anim = pebble_api::pbl_property_animation_create_layer_frame(
                layer,
                from.as_ref().map_or(std::ptr::null(), |r| r as *const _),
                to.as_ref().map_or(std::ptr::null(), |r| r as *const _),
            );
            // Store layer handle for syncing emulated memory during animation ticks
            if !anim.is_null() {
                unsafe { (*anim).emu_layer_handle = layer_handle; }
            }
            Action::Return(state.to_handle(anim as usize))
        }
        // property_animation_destroy / legacy
        200 | 398 => {
            pebble_api::pbl_animation_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // property_animation_get_animation
        400 => Action::Return(r0), // same object in our impl
        // property_animation_create_bounds_origin
        516 => {
            let layer_handle = r0;
            let layer: *mut pebble_api::PblLayer = state.from_handle(r0);
            let from = if r1 != 0 {
                let packed = proc.read_u32_aligned(r1).unwrap_or(0);
                let x = (packed & 0xFFFF) as i16;
                let y = ((packed >> 16) & 0xFFFF) as i16;
                Some(GPoint { x, y })
            } else { None };
            let to = if r2 != 0 {
                let packed = proc.read_u32_aligned(r2).unwrap_or(0);
                let x = (packed & 0xFFFF) as i16;
                let y = ((packed >> 16) & 0xFFFF) as i16;
                Some(GPoint { x, y })
            } else { None };
            let anim = pebble_api::pbl_property_animation_create_bounds_origin(
                layer,
                from.as_ref().map_or(std::ptr::null(), |p| p as *const _),
                to.as_ref().map_or(std::ptr::null(), |p| p as *const _),
            );
            if !anim.is_null() {
                unsafe { (*anim).emu_layer_handle = layer_handle; }
            }
            Action::Return(state.to_handle(anim as usize))
        }
        // property_animation_from/to/subject/update_*
        198 | 201..=203 | 399 | 401..=405 | 517 | 534 => Action::Return(0),

        // animation_clone
        422 => {
            let cloned = pebble_api::pbl_animation_clone(state.from_handle(r0));
            Action::Return(state.to_handle(cloned as usize))
        }
        // animation getters
        423 => Action::Return(pebble_api::pbl_animation_get_delay(state.from_handle(r0))),
        424 => Action::Return(pebble_api::pbl_animation_get_duration(state.from_handle(r0))),
        425 => Action::Return(1), // get_play_count
        426 => Action::Return(0), // get_elapsed
        427 => Action::Return(0), // get_reverse
        // animation_sequence_create
        428 => {
            let a: *mut pebble_api::PblAnimation = state.from_handle(r0);
            let b: *mut pebble_api::PblAnimation = state.from_handle(r1);
            let seq = pebble_api::pbl_animation_sequence_create(a, b, std::ptr::null_mut());
            Action::Return(state.to_handle(seq as usize))
        }
        // animation_sequence_create_from_array
        429 => {
            // r0 = array ptr in emu memory, r1 = count
            let mut children: Vec<*mut pebble_api::PblAnimation> = Vec::new();
            for i in 0..r1 {
                let handle = proc.read_u32_aligned(r0 + i * 4).unwrap_or(0);
                if handle != 0 {
                    children.push(state.from_handle(handle));
                }
            }
            let seq = pebble_api::pbl_animation_sequence_create_from_array(
                children.as_ptr(), children.len() as u32,
            );
            Action::Return(state.to_handle(seq as usize))
        }
        // animation_spawn_create
        433 => {
            let a: *mut pebble_api::PblAnimation = state.from_handle(r0);
            let b: *mut pebble_api::PblAnimation = state.from_handle(r1);
            let spawn = pebble_api::pbl_animation_spawn_create(a, b, std::ptr::null_mut());
            Action::Return(state.to_handle(spawn as usize))
        }
        // animation_spawn_create_from_array
        434 => {
            let mut children: Vec<*mut pebble_api::PblAnimation> = Vec::new();
            for i in 0..r1 {
                let handle = proc.read_u32_aligned(r0 + i * 4).unwrap_or(0);
                if handle != 0 {
                    children.push(state.from_handle(handle));
                }
            }
            let spawn = pebble_api::pbl_animation_spawn_create_from_array(
                children.as_ptr(), children.len() as u32,
            );
            Action::Return(state.to_handle(spawn as usize))
        }
        // animation setters
        430..=432 => Action::Return(0), // set_play_count, set_elapsed, set_reverse
        // animation_get_curve/custom_curve/implementation
        435 => Action::Return(pebble_api::pbl_animation_get_curve(state.from_handle(r0)) as u32),
        436 | 437 => Action::Return(0),

        // =================================================================
        // App Message (no-op stubs — no phone connection)
        // =================================================================
        35 => Action::Return(0), // app_message_deregister_callbacks
        36 => { // app_message_open
            println!("[emu] app_message_open(inbox={}, outbox={})", r0, r1);
            Action::Return(0)
        }
        293 => Action::Return(0), // app_message_get_context
        294 => Action::Return(8200), // app_message_inbox_size_maximum
        295 => { // app_message_outbox_begin
            // Write a dummy iterator pointer
            let buf = state.emu_malloc(proc, 256);
            if r0 != 0 {
                let _ = proc.write_u32_aligned(r0, buf);
            }
            Action::Return(0)
        }
        296 => Action::Return(0), // app_message_outbox_send
        297 => Action::Return(8200), // app_message_outbox_size_maximum
        298 => { // app_message_register_inbox_dropped
            println!("[emu] app_message_register_inbox_dropped(0x{:08x})", r0);
            Action::Return(0)
        }
        299 => { // app_message_register_inbox_received
            println!("[emu] app_message_register_inbox_received(0x{:08x})", r0);
            Action::Return(0)
        }
        300 => { // app_message_register_outbox_failed
            println!("[emu] app_message_register_outbox_failed(0x{:08x})", r0);
            Action::Return(0)
        }
        301 => { // app_message_register_outbox_sent
            println!("[emu] app_message_register_outbox_sent(0x{:08x})", r0);
            Action::Return(0)
        }
        302 => Action::Return(0), // app_message_set_context

        // =================================================================
        // App Sync (no-op stubs)
        // =================================================================
        43 => Action::Return(0), // app_sync_deinit
        44 => { // app_sync_get(sync, key) -> Tuple*
            // Walk the dictionary in the sync buffer to find the tuple with matching key
            let key = r1;
            let buf = state.app_sync_buffer;
            let buf_size = state.app_sync_buffer_size as u32;
            if buf == 0 || buf_size < 2 {
                Action::Return(0) // NULL
            } else {
                let count = proc.read_u8(buf).unwrap_or(0);
                let mut off = 1u32; // skip count byte
                let mut found = 0u32;
                for _ in 0..count {
                    if off + 7 > buf_size {
                        break;
                    }
                    let tuple_addr = buf + off;
                    let tkey = proc.read_u8(tuple_addr).unwrap_or(0) as u32
                        | (proc.read_u8(tuple_addr + 1).unwrap_or(0) as u32) << 8
                        | (proc.read_u8(tuple_addr + 2).unwrap_or(0) as u32) << 16
                        | (proc.read_u8(tuple_addr + 3).unwrap_or(0) as u32) << 24;
                    let tlen = proc.read_u8(tuple_addr + 5).unwrap_or(0) as u16
                        | (proc.read_u8(tuple_addr + 6).unwrap_or(0) as u16) << 8;
                    if tkey == key {
                        found = tuple_addr;
                        break;
                    }
                    off += 7 + tlen as u32;
                }
                Action::Return(found)
            }
        }
        45 => { // app_sync_init
            // r0=sync, r1=buffer, r2=buffer_size, r3=tuplets_ptr
            // stack: count, tuple_changed_cb, error_cb, context
            let buffer = r1;
            let buffer_size = r2 as u16;
            let tuplets_ptr = r3;
            let count = stack_arg(proc, 0) as u8;
            let tuple_changed_cb = stack_arg(proc, 1);
            let error_cb = stack_arg(proc, 2);
            let context = stack_arg(proc, 3);

            println!(
                "[emu] app_sync_init(sync=0x{:08x}, buf=0x{:08x}, buf_size={}, tuplets=0x{:08x}, count={}, cb=0x{:08x}, ctx=0x{:08x})",
                r0, buffer, buffer_size, tuplets_ptr, count, tuple_changed_cb, context
            );

            // Store sync state
            state.app_sync_tuple_changed_cb = tuple_changed_cb;
            state.app_sync_error_cb = error_cb;
            state.app_sync_context = context;
            state.app_sync_buffer = buffer;
            state.app_sync_buffer_size = buffer_size;

            // Parse Tuplet array and write serialized Tuples into buffer.
            //
            // Tuplet layout on ARM (NOT packed, from PebbleOS dict.h):
            //   offset 0: type (TupleType enum, 4 bytes)
            //   offset 4: key (uint32_t, 4 bytes)
            //   offset 8: union (8 bytes):
            //     bytes/cstring: { data_ptr(4), length(u16, 2), pad(2) }
            //     integer: { storage(u32, 4), width(u16, 2), pad(2) }
            //   Total: 16 bytes per Tuplet
            //
            // Tuple layout (packed, serialized in buffer):
            //   key(4) + type(1) + length(2) + value(length bytes) = 7 + length
            //
            // The buffer format is a Dictionary: count(1) + Tuple[0] + Tuple[1] + ...
            const TUPLET_SIZE: u32 = 16;
            // Dictionary starts with a 1-byte count header
            let _ = proc.write_u8(buffer, count);
            let mut buf_offset: u32 = 1; // skip count byte

            // Collect tuples to write, then call callbacks after
            struct InitTuple {
                key: u32,
                ttype: u8,
                tuple_addr: u32, // address in emulated buffer
            }
            let mut init_tuples: Vec<InitTuple> = Vec::new();

            for i in 0..count as u32 {
                let tp = tuplets_ptr + i * TUPLET_SIZE;
                // Tuplet fields (aligned, can use read_u32_aligned since Tuplet is 4-byte aligned)
                let ttype = proc.read_u32_aligned(tp).unwrap_or(0) as u8;
                let key = proc.read_u32_aligned(tp + 4).unwrap_or(0);

                // Determine value length and data based on type
                let (value_len, value_data): (u16, Vec<u8>) = match ttype {
                    0 | 1 => {
                        // BYTE_ARRAY or CSTRING: data_ptr(u32) at +8, length(u16) at +12
                        let ptr = proc.read_u32_aligned(tp + 8).unwrap_or(0);
                        let len = proc.read_u32_aligned(tp + 12).unwrap_or(0) as u16; // only low 16 bits
                        let mut data = Vec::new();
                        if ptr != 0 {
                            for j in 0..len as u32 {
                                data.push(proc.read_u8(ptr + j).unwrap_or(0));
                            }
                        }
                        println!(
                            "[emu]   tuplet[{}]: key={}, type={} ({}), len={}, ptr=0x{:08x}, data={:?}",
                            i, key, ttype,
                            if ttype == 0 { "BYTES" } else { "CSTRING" },
                            len, ptr,
                            if ttype == 1 { String::from_utf8_lossy(&data).to_string() } else { format!("{:?}", data) }
                        );
                        (len, data)
                    }
                    2 | 3 => {
                        // UINT or INT: storage(u32) at +8, width(u16) at +12
                        let storage = proc.read_u32_aligned(tp + 8).unwrap_or(0);
                        let width = proc.read_u32_aligned(tp + 12).unwrap_or(0) as u16; // only low 16 bits
                        // Width is in bytes (1, 2, or 4)
                        let len = if width > 0 && width <= 4 { width } else { 4 };
                        let data = match len {
                            1 => vec![storage as u8],
                            2 => vec![storage as u8, (storage >> 8) as u8],
                            _ => vec![storage as u8, (storage >> 8) as u8, (storage >> 16) as u8, (storage >> 24) as u8],
                        };
                        println!(
                            "[emu]   tuplet[{}]: key={}, type={} ({}), value={}, width={}",
                            i, key, ttype,
                            if ttype == 2 { "UINT" } else { "INT" },
                            storage, len
                        );
                        (len, data)
                    }
                    _ => {
                        println!("[emu]   tuplet[{}]: key={}, UNKNOWN type={}", i, key, ttype);
                        (0, Vec::new())
                    }
                };

                // Write Tuple into buffer: key(4) + type(1) + length(2) + value(value_len)
                let tuple_size = 7 + value_len as u32;
                if buf_offset + tuple_size > buffer_size as u32 {
                    eprintln!("[emu] app_sync_init: buffer overflow at tuple {}", i);
                    break;
                }
                let tuple_addr = buffer + buf_offset;
                // key (little-endian)
                let _ = proc.write_u8(tuple_addr, key as u8);
                let _ = proc.write_u8(tuple_addr + 1, (key >> 8) as u8);
                let _ = proc.write_u8(tuple_addr + 2, (key >> 16) as u8);
                let _ = proc.write_u8(tuple_addr + 3, (key >> 24) as u8);
                // type
                let _ = proc.write_u8(tuple_addr + 4, ttype);
                // length
                let _ = proc.write_u8(tuple_addr + 5, value_len as u8);
                let _ = proc.write_u8(tuple_addr + 6, (value_len >> 8) as u8);
                // value
                for (j, &b) in value_data.iter().enumerate() {
                    let _ = proc.write_u8(tuple_addr + 7 + j as u32, b);
                }

                init_tuples.push(InitTuple {
                    key,
                    ttype,
                    tuple_addr,
                });

                buf_offset += tuple_size;
            }

            // Call tuple_changed_cb for each initial tuple
            // Callback signature: void cb(uint32_t key, Tuple *new_tuple, Tuple *old_tuple, void *context)
            // For initial values, old_tuple = NULL (0)
            // NOTE: We intentionally skip calling tuple_changed_cb here.
            // The initial values are written into the buffer and accessible via
            // app_sync_get. Calling the callbacks causes register/SP corruption
            // in some apps (e.g., Trekv3 with 14 nested callbacks) because
            // call_callback doesn't fully handle the complex nesting that occurs
            // when callbacks themselves make API calls.
            let _ = tuple_changed_cb; // suppress unused warning

            Action::Return(0) // APP_MSG_OK
        }
        46 => Action::Return(0), // app_sync_set

        // =================================================================
        // Dictionary
        // =================================================================
        // dict_calc_buffer_size(tuple_count, ...) — return generous overestimate
        74 => {
            let tuple_count = r0;
            Action::Return(256 * tuple_count + 64)
        }
        // dict_calc_buffer_size_from_tuplets — same overestimate
        75 => {
            let tuple_count = r0;
            Action::Return(256 * tuple_count + 64)
        }
        84..=95 => Action::Return(0),

        // window_long_click_subscribe(button, delay_ms, down_handler, up_handler)
        303 => Action::Return(0), // TODO: long click not yet implemented

        // window_multi_click_subscribe(button, min, max, timeout, last_only, handler)
        304 => Action::Return(0),

        // window_raw_click_subscribe(button, down, up, context)
        305 => Action::Return(0),

        // window_set_click_context(button, context)
        306 => {
            let button = r0 as usize;
            if button < 4 {
                state.click_contexts[button] = r1;
            }
            Action::Return(0)
        }

        // window_single_click_subscribe(button, handler)
        307 => {
            let button = r0 as usize;
            if button < 4 {
                state.single_click_handlers[button] = r1;
            }
            Action::Return(0)
        }

        // quiet_time_is_active -> false
        631 => Action::Return(0),


        // =================================================================
        // Scroll layer
        // =================================================================

        // scroll_layer_add_child(sl, child)
        217 => {
            pebble_api::pbl_scroll_layer_add_child(state.from_handle(r0), state.from_handle(r1));
            Action::Return(0)
        }

        // scroll_layer_create(frame: GRect)
        218 => {
            let frame = grect_from_regs(r0, r1);
            let sl = pebble_api::pbl_scroll_layer_create(frame);
            Action::Return(state.to_handle(sl as usize))
        }

        // scroll_layer_destroy
        219 => {
            pebble_api::pbl_scroll_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // scroll_layer_get_content_offset -> GPoint packed in u32
        220 => {
            let p = pebble_api::pbl_scroll_layer_get_content_offset(state.from_handle(r0));
            let packed = (p.x as u16 as u32) | ((p.y as u16 as u32) << 16);
            Action::Return(packed)
        }

        // scroll_layer_get_content_size -> GSize packed in u32
        221 => {
            let s = pebble_api::pbl_scroll_layer_get_content_size(state.from_handle(r0));
            let packed = (s.w as u16 as u32) | ((s.h as u16 as u32) << 16);
            Action::Return(packed)
        }

        // scroll_layer_get_layer
        222 => {
            let layer = pebble_api::pbl_scroll_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // scroll_layer_get_shadow_hidden
        223 => {
            let hidden = pebble_api::pbl_scroll_layer_get_shadow_hidden(state.from_handle(r0));
            Action::Return(hidden as u32)
        }

        // scroll_layer_scroll_down_click_handler / scroll_layer_scroll_up_click_handler
        224 | 225 => Action::Return(0),

        // scroll_layer_set_callbacks(sl, callbacks) -- no-op
        226 => Action::Return(0),

        // scroll_layer_set_click_config_onto_window(sl, window)
        227 => {
            pebble_api::pbl_scroll_layer_set_click_config_onto_window(
                state.from_handle(r0), state.from_handle(r1),
            );
            Action::Return(0)
        }

        // scroll_layer_set_content_offset(sl, offset: GPoint, animated: bool)
        228 => {
            let sl: *mut pebble_api::PblScrollLayer = state.from_handle(r0);
            let offset = gpoint_from_reg(r1);
            let animated = r2 != 0;
            pebble_api::pbl_scroll_layer_set_content_offset(sl, offset, animated);
            Action::Return(0)
        }

        // scroll_layer_set_content_size(sl, size: GSize)
        229 => {
            let sl: *mut pebble_api::PblScrollLayer = state.from_handle(r0);
            let size = pebble_api::GSize { w: r1 as i16, h: (r1 >> 16) as i16 };
            pebble_api::pbl_scroll_layer_set_content_size(sl, size);
            Action::Return(0)
        }

        // scroll_layer_set_context(sl, context) -- no-op in emulator
        230 => Action::Return(0),

        // scroll_layer_set_frame(sl, frame: GRect)
        231 => {
            let frame = grect_from_regs(r1, r2);
            pebble_api::pbl_scroll_layer_set_frame(state.from_handle(r0), frame);
            Action::Return(0)
        }

        // scroll_layer_set_shadow_hidden(sl, hidden)
        232 => {
            pebble_api::pbl_scroll_layer_set_shadow_hidden(state.from_handle(r0), r1 != 0);
            Action::Return(0)
        }

        // scroll_layer_get_content_indicator
        577 => Action::Return(0), // NULL

        // scroll_layer_get_paging / scroll_layer_set_paging
        594 => {
            let paging = pebble_api::pbl_scroll_layer_get_paging(state.from_handle(r0));
            Action::Return(paging as u32)
        }
        595 => {
            pebble_api::pbl_scroll_layer_set_paging(state.from_handle(r0), r1 != 0);
            Action::Return(0)
        }

        // =================================================================
        // Action bar layer (legacy2: 7-16, modern: 446-456, 461)
        // =================================================================

        // action_bar_layer_add_to_window (legacy2=7, modern=446)
        7 | 446 => {
            pebble_api::pbl_action_bar_layer_add_to_window(state.from_handle(r0), state.from_handle(r1));
            Action::Return(0)
        }

        // action_bar_layer_clear_icon (legacy2=8, modern=447)
        8 | 447 => {
            pebble_api::pbl_action_bar_layer_clear_icon(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // action_bar_layer_create (legacy2=9, modern=448)
        9 | 448 => {
            let ab = pebble_api::pbl_action_bar_layer_create();
            Action::Return(state.to_handle(ab as usize))
        }

        // action_bar_layer_destroy (legacy2=10, modern=449)
        10 | 449 => {
            pebble_api::pbl_action_bar_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // action_bar_layer_get_layer (legacy2=11, modern=450)
        11 | 450 => {
            let layer = pebble_api::pbl_action_bar_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // action_bar_layer_remove_from_window (legacy2=12, modern=451)
        12 | 451 => {
            pebble_api::pbl_action_bar_layer_remove_from_window(state.from_handle(r0));
            Action::Return(0)
        }

        // action_bar_layer_set_background_color (legacy2=13 2bit, modern=452)
        13 | 452 => {
            pebble_api::pbl_action_bar_layer_set_background_color(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // action_bar_layer_set_click_config_provider (legacy2=14, modern=453)
        14 | 453 => {
            // In emulator, store the ARM address — we don't call it yet
            Action::Return(0)
        }

        // action_bar_layer_set_context (legacy2=15, modern=454)
        15 | 454 => {
            // Context is an emulated address, store it
            Action::Return(0)
        }

        // action_bar_layer_set_icon (legacy2=16, modern=455)
        16 | 455 => {
            pebble_api::pbl_action_bar_layer_set_icon(state.from_handle(r0), r1 as u8, state.from_handle(r2));
            Action::Return(0)
        }

        // action_bar_layer_set_icon_animated (456)
        456 => {
            pebble_api::pbl_action_bar_layer_set_icon_animated(state.from_handle(r0), r1 as u8, state.from_handle(r2), r3 != 0);
            Action::Return(0)
        }

        // action_bar_layer_set_icon_press_animation (461)
        461 => Action::Return(0),

        // =================================================================
        // Menu layer (legacy2: 169-178, modern: 439, 520-523, 578-579, 598)
        // =================================================================

        // menu_layer_create (legacy2=169, modern=439)
        169 | 439 => {
            let frame = grect_from_regs(r0, r1);
            let ml = pebble_api::pbl_menu_layer_create(frame);
            Action::Return(state.to_handle(ml as usize))
        }

        // menu_layer_destroy (170)
        170 => {
            pebble_api::pbl_menu_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // menu_layer_get_layer (171)
        171 => {
            let layer = pebble_api::pbl_menu_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // menu_layer_get_scroll_layer (172)
        172 => {
            let sl = pebble_api::pbl_menu_layer_get_scroll_layer(state.from_handle(r0));
            Action::Return(state.to_handle(sl as usize))
        }

        // menu_layer_get_selected_index (173) -> MenuIndex packed in u32
        173 => {
            let idx = pebble_api::pbl_menu_layer_get_selected_index(state.from_handle(r0));
            Action::Return((idx.section as u32) | ((idx.row as u32) << 16))
        }

        // menu_layer_reload_data (174)
        174 => {
            pebble_api::pbl_menu_layer_reload_data(state.from_handle(r0));
            Action::Return(0)
        }

        // menu_layer_set_callbacks (legacy2=175, legacy2_v2=320, modern=522)
        175 | 320 | 522 => {
            // r0=menu_layer, r1=context, r2=callbacks_ptr
            // Read the callbacks struct from emulated memory (13 x u32 = 52 bytes)
            let ml: *mut pebble_api::PblMenuLayer = state.from_handle(r0);
            if !ml.is_null() {
                unsafe {
                    (*ml).callback_context = r1 as *mut u8;
                    // Read callback function pointers from emulated memory
                    for i in 0..13u32 {
                        let cb_addr = proc.read_u32_unaligned(r2 + i * 4).unwrap_or(0);
                        let cb_ptr = &mut (*ml).callbacks as *mut pebble_api::MenuLayerCallbacks as *mut usize;
                        *cb_ptr.add(i as usize) = cb_addr as usize;
                    }
                }
            }
            Action::Return(0)
        }

        // menu_layer_set_click_config_onto_window (176)
        176 => Action::Return(0),

        // menu_layer_set_selected_index(ml, index: MenuIndex, scroll_align, animated)
        177 => {
            let index = pebble_api::MenuIndex {
                section: r1 as u16,
                row: (r1 >> 16) as u16,
            };
            pebble_api::pbl_menu_layer_set_selected_index(state.from_handle(r0), index, r2 as u8, r3 != 0);
            Action::Return(0)
        }

        // menu_layer_set_selected_next(ml, up, scroll_align, animated)
        178 => {
            pebble_api::pbl_menu_layer_set_selected_next(state.from_handle(r0), r1 != 0, r2 as u8, r3 != 0);
            Action::Return(0)
        }

        // menu_layer_set_highlight_colors(ml, bg, fg)
        520 => {
            pebble_api::pbl_menu_layer_set_highlight_colors(state.from_handle(r0), r1 as u8, r2 as u8);
            Action::Return(0)
        }

        // menu_layer_set_normal_colors(ml, bg, fg)
        521 => {
            pebble_api::pbl_menu_layer_set_normal_colors(state.from_handle(r0), r1 as u8, r2 as u8);
            Action::Return(0)
        }

        // menu_layer_pad_bottom_enable(ml, enable)
        523 => {
            pebble_api::pbl_menu_layer_pad_bottom_enable(state.from_handle(r0), r1 != 0);
            Action::Return(0)
        }

        // menu_layer_get_center_focused (578)
        578 => {
            let cf = pebble_api::pbl_menu_layer_get_center_focused(state.from_handle(r0));
            Action::Return(cf as u32)
        }

        // menu_layer_set_center_focused(ml, center_focused) (579)
        579 => {
            pebble_api::pbl_menu_layer_set_center_focused(state.from_handle(r0), r1 != 0);
            Action::Return(0)
        }

        // menu_layer_is_index_selected(ml, index_ptr) (598)
        598 => {
            // Read MenuIndex from emulated memory
            let section = proc.read_u16_aligned(r1).unwrap_or(0);
            let row = proc.read_u16_aligned(r1 + 2).unwrap_or(0);
            let index = pebble_api::MenuIndex { section, row };
            let result = pebble_api::pbl_menu_layer_is_index_selected(state.from_handle(r0), &index);
            Action::Return(result as u32)
        }

        // =================================================================
        // Simple menu layer (233-237, 316)
        // =================================================================

        // simple_menu_layer_create(frame, window, sections, num_sections, context)
        233 => {
            let frame = grect_from_regs(r0, r1);
            let sml = pebble_api::pbl_simple_menu_layer_create(
                frame, state.from_handle(r2), r3 as *const u8,
                stack_arg(proc, 0) as i32,
                stack_arg(proc, 1) as *mut u8,
            );
            Action::Return(state.to_handle(sml as usize))
        }

        // simple_menu_layer_destroy (234)
        234 => {
            pebble_api::pbl_simple_menu_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // simple_menu_layer_get_layer (235)
        235 => {
            let layer = pebble_api::pbl_simple_menu_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // simple_menu_layer_get_selected_index (236)
        236 => {
            let idx = pebble_api::pbl_simple_menu_layer_get_selected_index(state.from_handle(r0));
            Action::Return(idx as u32)
        }

        // simple_menu_layer_set_selected_index(sml, index, animated) (237)
        237 => {
            pebble_api::pbl_simple_menu_layer_set_selected_index(state.from_handle(r0), r1 as i32, r2 != 0);
            Action::Return(0)
        }

        // simple_menu_layer_get_menu_layer (316)
        316 => {
            let ml = pebble_api::pbl_simple_menu_layer_get_menu_layer(state.from_handle(r0));
            Action::Return(state.to_handle(ml as usize))
        }

        // =================================================================
        // Status bar layer (524-530)
        // =================================================================

        // status_bar_layer_create (524)
        524 => {
            let sb = pebble_api::pbl_status_bar_layer_create();
            Action::Return(state.to_handle(sb as usize))
        }

        // status_bar_layer_destroy (525)
        525 => {
            pebble_api::pbl_status_bar_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // status_bar_layer_get_background_color (526)
        526 => {
            let c = pebble_api::pbl_status_bar_layer_get_background_color(state.from_handle(r0));
            Action::Return(c as u32)
        }

        // status_bar_layer_get_foreground_color (527)
        527 => {
            let c = pebble_api::pbl_status_bar_layer_get_foreground_color(state.from_handle(r0));
            Action::Return(c as u32)
        }

        // status_bar_layer_get_layer (528)
        528 => {
            let layer = pebble_api::pbl_status_bar_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // status_bar_layer_set_colors(sb, bg, fg) (529)
        529 => {
            pebble_api::pbl_status_bar_layer_set_colors(state.from_handle(r0), r1 as u8, r2 as u8);
            Action::Return(0)
        }

        // status_bar_layer_set_separator_mode(sb, mode) (530)
        530 => {
            pebble_api::pbl_status_bar_layer_set_separator_mode(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // =================================================================
        // Number window (179-186, 322)
        // =================================================================

        // number_window_create(label, callbacks, context) (179)
        179 => {
            let nw = pebble_api::pbl_number_window_create(r0 as *const std::ffi::c_char, r1 as *const u8, r2 as *mut u8);
            Action::Return(state.to_handle(nw as usize))
        }

        // number_window_destroy (180)
        180 => {
            pebble_api::pbl_number_window_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }

        // number_window_get_value (181)
        181 => {
            let v = pebble_api::pbl_number_window_get_value(state.from_handle(r0));
            Action::Return(v as u32)
        }

        // number_window_set_label(nw, label) (182)
        182 => {
            // label is in emulated memory — store the emulated address
            let nw: *mut pebble_api::PblNumberWindow = state.from_handle(r0);
            if !nw.is_null() { unsafe { (*nw).label = r1 as *const std::ffi::c_char; } }
            Action::Return(0)
        }

        // number_window_set_max (183)
        183 => {
            pebble_api::pbl_number_window_set_max(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }

        // number_window_set_min (184)
        184 => {
            pebble_api::pbl_number_window_set_min(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }

        // number_window_set_step_size (185)
        185 => {
            pebble_api::pbl_number_window_set_step_size(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }

        // number_window_set_value (186)
        186 => {
            pebble_api::pbl_number_window_set_value(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }

        // number_window_get_window (322)
        322 => {
            let w = pebble_api::pbl_number_window_get_window(state.from_handle(r0));
            Action::Return(state.to_handle(w as usize))
        }

        // =================================================================
        // Text layer legacy2 (249-260) — delegate to modern text_layer
        // =================================================================

        // text_layer_legacy2_create (249) — same as modern 462
        249 => {
            let tl = pebble_api::pbl_text_layer_create(grect_from_regs(r0, r1));
            let h = state.to_handle(tl as usize);
            Action::Return(h)
        }
        // text_layer_legacy2_destroy (250)
        250 => {
            pebble_api::pbl_text_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // text_layer_legacy2_get_content_size (251)
        251 => {
            let sz = pebble_api::pbl_text_layer_get_content_size(state.from_handle(r0));
            let packed = (sz.w as u16 as u32) | ((sz.h as u16 as u32) << 16);
            Action::Return(packed)
        }
        // text_layer_legacy2_get_layer (252)
        252 => {
            let layer = pebble_api::pbl_text_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }
        // text_layer_legacy2_get_text (253)
        253 => {
            let text_ptr = pebble_api::pbl_text_layer_get_text(state.from_handle(r0));
            if text_ptr.is_null() {
                Action::Return(0)
            } else {
                let cstr = unsafe { std::ffi::CStr::from_ptr(text_ptr) };
                let bytes = cstr.to_bytes_with_nul();
                let emu_addr = state.emu_malloc(proc, bytes.len() as u32);
                for (i, &b) in bytes.iter().enumerate() {
                    let _ = proc.write_u8(emu_addr + i as u32, b);
                }
                Action::Return(emu_addr)
            }
        }
        // text_layer_legacy2_set_background_color_2bit (254)
        254 => {
            pebble_api::pbl_text_layer_set_background_color(state.from_handle(r0), GColor8(r1 as u8));
            Action::Return(0)
        }
        // text_layer_legacy2_set_font (255)
        255 => {
            pebble_api::pbl_text_layer_set_font(state.from_handle(r0), state.from_handle(r1));
            Action::Return(0)
        }
        // text_layer_legacy2_set_overflow_mode (256)
        256 => {
            pebble_api::pbl_text_layer_set_overflow_mode(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }
        // text_layer_legacy2_set_size (257) — sets GSize {w,h} packed in r1
        257 => {
            let size = pebble_api::GSize { w: r1 as i16, h: (r1 >> 16) as i16 };
            pebble_api::pbl_text_layer_set_size(state.from_handle(r0), size);
            Action::Return(0)
        }
        // text_layer_legacy2_set_text (258)
        258 => {
            let text = read_cstring(proc, r1);
            state.set_text_for_handle(r0, &text);
            Action::Return(0)
        }
        // text_layer_legacy2_set_text_alignment (259)
        259 => {
            pebble_api::pbl_text_layer_set_text_alignment(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }
        // text_layer_legacy2_set_text_color_2bit (260)
        260 => {
            pebble_api::pbl_text_layer_set_text_color(state.from_handle(r0), GColor8(r1 as u8));
            Action::Return(0)
        }

        // =================================================================
        // Menu cell drawing (165-168, 459) — emulator dispatch
        // =================================================================

        // menu_cell_basic_draw(ctx, cell_layer, title, subtitle, icon)
        165 => {
            let title_str = read_cstring(proc, r2);
            let subtitle_str = read_cstring(proc, r3);
            let title_cstr = std::ffi::CString::new(title_str).unwrap_or_default();
            let subtitle_cstr = std::ffi::CString::new(subtitle_str).unwrap_or_default();
            pebble_api::pbl_menu_cell_basic_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                title_cstr.as_ptr(),
                subtitle_cstr.as_ptr(),
                std::ptr::null(),
            );
            Action::Return(0)
        }

        // menu_cell_basic_header_draw(ctx, cell_layer, title)
        166 => {
            let title_str = read_cstring(proc, r2);
            let title_cstr = std::ffi::CString::new(title_str).unwrap_or_default();
            pebble_api::pbl_menu_cell_basic_header_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                title_cstr.as_ptr(),
            );
            Action::Return(0)
        }

        // menu_cell_title_draw(ctx, cell_layer, title)
        167 => {
            let title_str = read_cstring(proc, r2);
            let title_cstr = std::ffi::CString::new(title_str).unwrap_or_default();
            pebble_api::pbl_menu_cell_title_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                title_cstr.as_ptr(),
            );
            Action::Return(0)
        }

        // menu_index_compare(a, b) -> i16
        168 => {
            // Read MenuIndex from emulated memory
            let a = pebble_api::MenuIndex {
                section: proc.read_u16_aligned(r0).unwrap_or(0),
                row: proc.read_u16_aligned(r0 + 2).unwrap_or(0),
            };
            let b = pebble_api::MenuIndex {
                section: proc.read_u16_aligned(r1).unwrap_or(0),
                row: proc.read_u16_aligned(r1 + 2).unwrap_or(0),
            };
            let result = pebble_api::pbl_menu_index_compare(&a, &b);
            Action::Return(result as u32)
        }

        // menu_cell_layer_is_highlighted (459)
        459 => Action::Return(0),

        // =================================================================
        // Inverter layer (135-137)
        // =================================================================

        135 => {
            let il = pebble_api::pbl_inverter_layer_create(grect_from_regs(r0, r1));
            Action::Return(state.to_handle(il as usize))
        }
        136 => {
            pebble_api::pbl_inverter_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        137 => {
            let layer = pebble_api::pbl_inverter_layer_get_layer(state.from_handle(r0));
            let h = state.to_handle(layer as usize);
            let bounds = pebble_api::pbl_layer_get_bounds(layer);
            let frame = pebble_api::pbl_layer_get_frame(layer);
            write_emu_layer(proc, h, &bounds, &frame);
            Action::Return(h)
        }

        // =================================================================
        // Rot bitmap layer (210-216)
        // =================================================================

        210 => {
            let rb = pebble_api::pbl_rot_bitmap_layer_create(state.from_handle(r0));
            Action::Return(state.to_handle(rb as usize))
        }
        211 => {
            pebble_api::pbl_rot_bitmap_layer_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        212 => {
            pebble_api::pbl_rot_bitmap_layer_increment_angle(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }
        213 => {
            pebble_api::pbl_rot_bitmap_layer_set_angle(state.from_handle(r0), r1 as i32);
            Action::Return(0)
        }
        214 => {
            pebble_api::pbl_rot_bitmap_layer_set_corner_clip_color(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }
        215 => {
            pebble_api::pbl_rot_bitmap_set_compositing_mode(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }
        216 => {
            let ic = gpoint_from_reg(r1);
            pebble_api::pbl_rot_bitmap_set_src_ic(state.from_handle(r0), ic);
            Action::Return(0)
        }

        // =================================================================
        // Remaining bulk stubs in emulator
        // =================================================================

        // Dict read/find (76-83)
        76 => Action::Return(0), // dict_find → NULL
        77 => Action::Return(0), // dict_merge → OK
        78 => Action::Return(0), // dict_read_begin_from_buffer → NULL
        79 => Action::Return(0), // dict_read_first → NULL
        80 => Action::Return(0), // dict_read_next → NULL
        81 | 82 | 83 => Action::Return(0), // dict_serialize

        // Compass (337-340)
        337 => Action::Return(0),
        338 => Action::Return(0),
        339 | 340 => Action::Return(0),

        // Content indicator (572-576)
        572 => Action::Return(1), // configure_direction → true
        573 => Action::Return(0), // create → NULL
        574 | 576 => Action::Return(0), // destroy / set_available
        575 => Action::Return(0), // get_available → false

        // Launch (352, 438)
        352 | 438 => Action::Return(0),

        // UUID (341-342)
        341 => {
            // uuid_equal: compare 16 bytes
            let mut eq = true;
            for i in 0..16u32 {
                let a = proc.read_u8(r0 + i).unwrap_or(0);
                let b = proc.read_u8(r1 + i).unwrap_or(0);
                if a != b { eq = false; break; }
            }
            Action::Return(eq as u32)
        }
        342 => Action::Return(0), // uuid_to_string — no-op in emulator

        // Heap (335-336)
        335 => Action::Return(HEAP_SIZE - state.heap_offset),
        336 => Action::Return(64 * 1024),  // bytes_used

        // psleep (204)
        204 => {
            let ms = r0 as u64;
            if ms > 0 && ms < 5000 {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            Action::Return(0)
        }

        // Watch info (346)
        346 => Action::Return((4 << 24) | (3 << 16)),

        // Clock timezone (359)
        359 => Action::Return(1),

        // Exit reason (616)
        616 => Action::Return(0),

        // Preferred (623, 630)
        623 => Action::Return(2000),
        630 => Action::Return(1),

        // Click helpers (308, 321, 325)
        308 => Action::Return(0), // single_repeating_click_subscribe
        321 => Action::Return(0), // get_click_config_context
        325 => Action::Return(0), // is_repeating

        // Data logging (71-73)
        71 => Action::Return(1), // create → fake handle
        72 | 73 => Action::Return(0),

        // Wakeup (353-358)
        353 | 354 | 358 => Action::Return(0),
        355 | 356 => Action::Return(0),
        357 => Action::Return(0xFFFFFFFF), // wakeup_schedule → -1

        // Dictation (550-554, 570)
        550 => Action::Return(0), // create → NULL
        551 | 552 | 554 | 570 => Action::Return(0),
        553 => Action::Return(0xFFFFFFFF), // start → -1

        // Smartstrap (555-565)
        555 | 558 => Action::Return(0xFFFFFFFF), // begin/end_write → -1
        556 => Action::Return(0), // create → NULL
        557 | 559 | 560 | 563 | 564 | 565 => Action::Return(0),
        561 => Action::Return(0xFFFFFFFF), // read → -1
        562 => Action::Return(0), // is_available → false

        // App comm / worker (29-30, 327-334)
        29 | 30 | 327 | 328 | 329 | 330 | 331 | 332 | 333 | 334 => Action::Return(0),

        // Health (remaining — only indices not already handled above)
        599 | 600 | 602 | 603 | 605 | 609 | 611 | 617 | 618 | 619 | 628 | 629 => Action::Return(0),

        // Action menu (536-549)
        536 | 537 | 538 | 539 | 540 | 541 | 542 | 543 | 544 | 545 | 546 | 547 | 548 | 549 => Action::Return(0),

        // GBitmap extended (324, 406 — 393/412-414 already handled)
        324 | 406 => Action::Return(0), // create → NULL

        // GBitmap sequence (415-420, 441-443, 457)
        415 => Action::Return(0), // create → NULL
        416 | 417 | 418 | 419 | 420 | 441 | 442 | 443 | 457 => Action::Return(0),

        // Graphics text attributes (585-590)
        585 => Action::Return(0), // create → NULL
        586 | 587 | 588 | 589 | 590 => Action::Return(0),

        // Graphics draw rotated bitmap (460)
        460 => Action::Return(0),

        // (193 persist_read_string_deprecated already handled above)

        // Layer coordinate conversion (592-593)
        592 => {
            // layer_convert_point_to_screen: read frame.x/y from handle, add to point
            let layer: *mut pebble_api::PblLayer = state.from_handle(r0);
            let p = gpoint_from_reg(r1);
            if !layer.is_null() {
                let f = unsafe { (*layer).frame };
                let result = pebble_api::GPoint { x: p.x + f.x, y: p.y + f.y };
                Action::Return(gpoint_to_u32(&result))
            } else {
                Action::Return(r1)
            }
        }
        593 => Action::Return(0), // layer_convert_rect_to_screen — stub

        // Unobstructed area unsubscribe (625)
        625 => Action::Return(0),

        // Memory cache flush (626)
        626 => Action::Return(0),

        // text_layer_set_size (470) — GSize packed in r1
        470 => {
            let size = pebble_api::GSize { w: r1 as i16, h: (r1 >> 16) as i16 };
            pebble_api::pbl_text_layer_set_size(state.from_handle(r0), size);
            Action::Return(0)
        }

        // text_layer_enable_screen_text_flow_and_paging / restore (596-597)
        596 | 597 => Action::Return(0),

        // rot_bitmap_layer_set_corner_clip_color modern (374)
        374 => {
            pebble_api::pbl_rot_bitmap_layer_set_corner_clip_color(state.from_handle(r0), r1 as u8);
            Action::Return(0)
        }

        // dict_size (314), dict_serialize_tuplets_to_buffer (310)
        310 | 314 => Action::Return(0),

        // Profiler no-ops (365-368)
        365 | 366 | 367 | 368 => Action::Return(0),

        // app_glance (614-615), rocky (627)
        614 | 615 | 627 => Action::Return(0),

        // =================================================================
        // GDraw command API (474-515, 612)
        // =================================================================

        // gdraw_command_image_create_with_resource(resource_id) (488)
        488 => {
            let img = pebble_api::pbl_gdraw_command_image_create_with_resource(r0);
            Action::Return(state.to_handle(img as usize))
        }
        // gdraw_command_image_clone(image) (487)
        487 => {
            let img = pebble_api::pbl_gdraw_command_image_clone(state.from_handle(r0));
            Action::Return(state.to_handle(img as usize))
        }
        // gdraw_command_image_destroy(image) (489)
        489 => {
            pebble_api::pbl_gdraw_command_image_destroy(state.forget_handle(r0) as *mut _);
            Action::Return(0)
        }
        // gdraw_command_image_draw(ctx, image, offset: GPoint) (490)
        490 => {
            let offset = gpoint_from_reg(r2);
            pebble_api::pbl_gdraw_command_image_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                offset,
            );
            Action::Return(0)
        }
        // gdraw_command_image_get_bounds_size(image) -> GSize (491)
        491 => {
            let s = pebble_api::pbl_gdraw_command_image_get_bounds_size(state.from_handle(r0));
            Action::Return((s.w as u16 as u32) | ((s.h as u16 as u32) << 16))
        }
        // gdraw_command_image_get_command_list(image) (492)
        492 => {
            let list = pebble_api::pbl_gdraw_command_image_get_command_list(state.from_handle(r0));
            Action::Return(state.to_handle(list as usize))
        }
        // gdraw_command_image_set_bounds_size(image, size: GSize) (493)
        493 => {
            let size = pebble_api::GSize { w: r1 as i16, h: (r1 >> 16) as i16 };
            pebble_api::pbl_gdraw_command_image_set_bounds_size(state.from_handle(r0), size);
            Action::Return(0)
        }

        // gdraw_command_list_draw(ctx, list) (494)
        494 => {
            pebble_api::pbl_gdraw_command_list_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }
        // gdraw_command_list_get_command(list, idx) (495)
        495 => {
            let cmd = pebble_api::pbl_gdraw_command_list_get_command(state.from_handle(r0), r1 as u16);
            Action::Return(state.to_handle(cmd as usize))
        }
        // gdraw_command_list_get_num_commands(list) (496)
        496 => {
            let n = pebble_api::pbl_gdraw_command_list_get_num_commands(state.from_handle(r0));
            Action::Return(n)
        }
        // gdraw_command_list_iterate(list, callback, context) (497)
        497 => Action::Return(0), // No-op — callbacks need ARM execution

        // gdraw_command_draw(ctx, command) (474)
        474 => {
            pebble_api::pbl_gdraw_command_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
            );
            Action::Return(0)
        }
        // gdraw_command_get_type (486)
        486 => Action::Return(pebble_api::pbl_gdraw_command_get_type(state.from_handle(r0)) as u32),
        // gdraw_command_get_fill_color (478)
        478 => Action::Return(pebble_api::pbl_gdraw_command_get_fill_color(state.from_handle(r0)) as u32),
        // gdraw_command_set_fill_color (509)
        509 => { pebble_api::pbl_gdraw_command_set_fill_color(state.from_handle(r0), r1 as u8); Action::Return(0) }
        // gdraw_command_get_stroke_color (484)
        484 => Action::Return(pebble_api::pbl_gdraw_command_get_stroke_color(state.from_handle(r0)) as u32),
        // gdraw_command_set_stroke_color (514)
        514 => { pebble_api::pbl_gdraw_command_set_stroke_color(state.from_handle(r0), r1 as u8); Action::Return(0) }
        // gdraw_command_get_stroke_width (485)
        485 => Action::Return(pebble_api::pbl_gdraw_command_get_stroke_width(state.from_handle(r0)) as u32),
        // gdraw_command_set_stroke_width (515)
        515 => { pebble_api::pbl_gdraw_command_set_stroke_width(state.from_handle(r0), r1 as u8); Action::Return(0) }
        // gdraw_command_get_num_points (480)
        480 => Action::Return(pebble_api::pbl_gdraw_command_get_num_points(state.from_handle(r0)) as u32),
        // gdraw_command_get_point(cmd, idx) -> GPoint (482)
        482 => {
            let p = pebble_api::pbl_gdraw_command_get_point(state.from_handle(r0), r1 as u16);
            Action::Return(gpoint_to_u32(&p))
        }
        // gdraw_command_set_point(cmd, idx, point: GPoint) (512)
        512 => {
            let point = gpoint_from_reg(r2);
            pebble_api::pbl_gdraw_command_set_point(state.from_handle(r0), r1 as u16, point);
            Action::Return(0)
        }
        // gdraw_command_get_radius (483)
        483 => Action::Return(pebble_api::pbl_gdraw_command_get_radius(state.from_handle(r0)) as u32),
        // gdraw_command_set_radius (513)
        513 => { pebble_api::pbl_gdraw_command_set_radius(state.from_handle(r0), r1 as u16); Action::Return(0) }
        // gdraw_command_get_path_open (481)
        481 => Action::Return(pebble_api::pbl_gdraw_command_get_path_open(state.from_handle(r0)) as u32),
        // gdraw_command_set_path_open (511)
        511 => { pebble_api::pbl_gdraw_command_set_path_open(state.from_handle(r0), r1 != 0); Action::Return(0) }
        // gdraw_command_get_hidden (479)
        479 => Action::Return(pebble_api::pbl_gdraw_command_get_hidden(state.from_handle(r0)) as u32),
        // gdraw_command_set_hidden (510)
        510 => { pebble_api::pbl_gdraw_command_set_hidden(state.from_handle(r0), r1 != 0); Action::Return(0) }

        // gdraw_command_frame_draw(ctx, sequence, frame, offset: GPoint) (475)
        475 => {
            let offset = gpoint_from_reg(r3);
            pebble_api::pbl_gdraw_command_frame_draw(
                &mut state.gctx as *mut PblGContext,
                state.from_handle(r1),
                state.from_handle(r2),
                offset,
            );
            Action::Return(0)
        }
        // gdraw_command_frame_get_duration (476)
        476 => Action::Return(pebble_api::pbl_gdraw_command_frame_get_duration(state.from_handle(r0))),
        // gdraw_command_frame_set_duration (477)
        477 => { pebble_api::pbl_gdraw_command_frame_set_duration(state.from_handle(r0), r1); Action::Return(0) }
        // gdraw_command_frame_get_command_list (612)
        612 => {
            let list = pebble_api::pbl_gdraw_command_frame_get_command_list(state.from_handle(r0));
            Action::Return(state.to_handle(list as usize))
        }

        // gdraw_command_sequence_create_with_resource(resource_id) (499)
        499 => {
            let seq = pebble_api::pbl_gdraw_command_sequence_create_with_resource(r0);
            Action::Return(state.to_handle(seq as usize))
        }
        // gdraw_command_sequence_clone(seq) (498)
        498 => {
            let seq = pebble_api::pbl_gdraw_command_sequence_clone(state.from_handle(r0));
            Action::Return(state.to_handle(seq as usize))
        }
        // gdraw_command_sequence_destroy(seq) (500)
        500 => { pebble_api::pbl_gdraw_command_sequence_destroy(state.forget_handle(r0) as *mut _); Action::Return(0) }
        // gdraw_command_sequence_get_bounds_size(seq) -> GSize (501)
        501 => {
            let s = pebble_api::pbl_gdraw_command_sequence_get_bounds_size(state.from_handle(r0));
            Action::Return((s.w as u16 as u32) | ((s.h as u16 as u32) << 16))
        }
        // gdraw_command_sequence_set_bounds_size(seq, size) (507)
        507 => {
            let size = pebble_api::GSize { w: r1 as i16, h: (r1 >> 16) as i16 };
            pebble_api::pbl_gdraw_command_sequence_set_bounds_size(state.from_handle(r0), size);
            Action::Return(0)
        }
        // gdraw_command_sequence_get_frame_by_elapsed(seq, elapsed_ms) (502)
        502 => {
            let frame = pebble_api::pbl_gdraw_command_sequence_get_frame_by_elapsed(state.from_handle(r0), r1);
            Action::Return(state.to_handle(frame as usize))
        }
        // gdraw_command_sequence_get_frame_by_index(seq, index) (503)
        503 => {
            let frame = pebble_api::pbl_gdraw_command_sequence_get_frame_by_index(state.from_handle(r0), r1);
            Action::Return(state.to_handle(frame as usize))
        }
        // gdraw_command_sequence_get_num_frames (504)
        504 => Action::Return(pebble_api::pbl_gdraw_command_sequence_get_num_frames(state.from_handle(r0))),
        // gdraw_command_sequence_get_play_count (505)
        505 => Action::Return(pebble_api::pbl_gdraw_command_sequence_get_play_count(state.from_handle(r0))),
        // gdraw_command_sequence_set_play_count (508)
        508 => { pebble_api::pbl_gdraw_command_sequence_set_play_count(state.from_handle(r0), r1); Action::Return(0) }
        // gdraw_command_sequence_get_total_duration (506)
        506 => Action::Return(pebble_api::pbl_gdraw_command_sequence_get_total_duration(state.from_handle(r0))),

        // gbitmap_create_from_png_data(data, size) (421)
        421 => Action::Return(0), // NULL — no PNG decoder in emulator
        // gbitmap_create_palettized_from_1bit(src) (458)
        458 => Action::Return(0), // NULL

        // graphics_text_layout_get_content_size_with_attributes (591)
        // Same as 315 but with extra text_attributes param (ignored)
        // r0=text, r1=font, r2=box.xy, r3=box.wh, stack[0]=overflow, stack[1]=alignment, stack[2]=attrs
        591 => {
            let box_rect = GRect {
                x: r2 as i16, y: (r2 >> 16) as i16,
                w: r3 as i16, h: (r3 >> 16) as i16,
            };
            let text_str = read_cstring(proc, r0);
            let font_ptr: *const u8 = state.from_handle(r1);
            let size = pebble_api::measure_text_content(&text_str, font_ptr, box_rect);
            Action::Return((size.w as u16 as u32) | ((size.h as u16 as u32) << 16))
        }

        // =================================================================
        // Default: unimplemented stub
        // =================================================================
        _ => {
            let name = executor::jump_table_name(idx);
            if !name.is_empty() {
                eprintln!("[emu] STUB [{}] {}", idx, name);
            }
            Action::Return(0)
        }
    }
}

// ---------------------------------------------------------------------------
// Simplified snprintf
// ---------------------------------------------------------------------------

fn simple_snprintf(proc: &mut Processor, fmt: &str, first_arg: u32, stack_start: usize) -> String {
    let mut result = String::new();
    let mut chars = fmt.chars().peekable();
    let mut arg_idx = 0u32; // which vararg we're on (r3 is first, then stack)

    // Pre-read stack args to avoid borrowing conflict
    let stack_args: Vec<u32> = (0..8).map(|i| stack_arg(proc, stack_start + i)).collect();

    let get_arg = |idx: u32| -> u32 {
        if idx == 0 {
            first_arg
        } else {
            stack_args.get((idx - 1) as usize).copied().unwrap_or(0)
        }
    };

    while let Some(c) = chars.next() {
        if c == '%' {
            // Parse optional flags/width
            let mut width_str = String::new();
            let mut zero_pad = false;
            while let Some(&nc) = chars.peek() {
                if nc == '0' && width_str.is_empty() {
                    zero_pad = true;
                    chars.next();
                } else if nc.is_ascii_digit() {
                    width_str.push(nc);
                    chars.next();
                } else if nc == 'l' {
                    chars.next(); // skip 'l' modifier
                } else {
                    break;
                }
            }
            let width: usize = width_str.parse().unwrap_or(0);

            match chars.next() {
                Some('d') | Some('i') => {
                    let val = get_arg(arg_idx) as i32;
                    arg_idx += 1;
                    let s = format!("{}", val);
                    if zero_pad && s.len() < width {
                        for _ in 0..(width - s.len()) {
                            result.push('0');
                        }
                    }
                    result.push_str(&s);
                }
                Some('u') => {
                    let val = get_arg(arg_idx);
                    arg_idx += 1;
                    result.push_str(&format!("{}", val));
                }
                Some('x') => {
                    let val = get_arg(arg_idx);
                    arg_idx += 1;
                    result.push_str(&format!("{:x}", val));
                }
                Some('X') => {
                    let val = get_arg(arg_idx);
                    arg_idx += 1;
                    result.push_str(&format!("{:X}", val));
                }
                Some('s') => {
                    let addr = get_arg(arg_idx);
                    arg_idx += 1;
                    result.push_str(&read_cstring(proc, addr));
                }
                Some('c') => {
                    let val = get_arg(arg_idx) as u8;
                    arg_idx += 1;
                    result.push(val as char);
                }
                Some('%') => result.push('%'),
                Some(other) => {
                    result.push('%');
                    result.push(other);
                }
                None => result.push('%'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Host strftime wrapper
// ---------------------------------------------------------------------------

fn host_strftime(fmt: &str, tm: &libc::tm) -> String {
    let c_fmt = std::ffi::CString::new(fmt).unwrap_or_default();
    let mut buf = [0u8; 256];
    let len = unsafe {
        libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            c_fmt.as_ptr(),
            tm as *const libc::tm,
        )
    };
    String::from_utf8_lossy(&buf[..len]).to_string()
}

// ---------------------------------------------------------------------------
// Call emulated callback
// ---------------------------------------------------------------------------

fn call_callback(
    proc: &mut Processor,
    state: &mut EmuState,
    addr: u32,
    args: &[u32],
) -> Result<(), String> {
    if addr == 0 {
        return Ok(());
    }

    // Save all callee-saved registers (r4-r11), SP, and LR.
    // When dispatching API calls that internally run ARM callbacks,
    // the emulator intercepts the return via a trampoline hook.
    // This hook may fire before POP {.., pc} completes its SP
    // writeback, AND the callback code may legitimately use r4-r11
    // for its own purposes (saving/restoring them via push/pop).
    // Since the interrupted POP may not fully complete, we must
    // restore the caller's complete register context.
    let saved_lr = proc.lr();
    let saved_sp = proc.sp();
    let saved_r4 = proc[RegisterIndex::R4];
    let saved_r5 = proc[RegisterIndex::R5];
    let saved_r6 = proc[RegisterIndex::R6];
    let saved_r7 = proc[RegisterIndex::R7];
    let saved_r8 = proc[RegisterIndex::R8];
    let saved_r9 = proc[RegisterIndex::R9];
    let saved_r10 = proc[RegisterIndex::R10];
    let saved_r11 = proc[RegisterIndex::R11];

    // Set up arguments
    let reg_indices = [
        RegisterIndex::R0,
        RegisterIndex::R1,
        RegisterIndex::R2,
        RegisterIndex::R3,
    ];
    for (i, &arg) in args.iter().enumerate() {
        if i < 4 {
            proc.set(reg_indices[i], arg);
        }
        // TODO: push extra args onto stack
    }

    // Set LR to callback return trampoline
    proc.set_lr(CALLBACK_RETURN_ADDR | 1); // Thumb bit
    // Set PC to callback address (clear Thumb bit for set_pc)
    proc.set_pc(addr & !1);

    // Run until callback returns
    let mut loop_iter = 0u32;
    loop {
        if pebble_api::stop_requested() { return Err("guest stopped".into()); }
        loop_iter += 1;
        let event = proc
            .run(RunOptions::new().gas(500000))
            .map_err(|e| format!("Callback at 0x{:08x} error: {:?} (PC=0x{:08x})", addr, e, proc.pc()))?;

        match event {
            Some(Event::Hook { address }) => {
                if address == CALLBACK_RETURN_ADDR {
                    break; // Callback returned
                }
                // Nested API call
                let idx = ((address - TRAMPOLINE_BASE) / 4) as usize;
                let action = dispatch(proc, state, idx);
                let lr = proc.lr();

                match action {
                    Action::Return(val) => {
                        proc.set(RegisterIndex::R0, val);
                        proc.set_pc(lr & !1);
                    }
                    Action::EventLoop => {
                        // Nested event loop from callback — just return
                        proc.set_pc(lr & !1);
                    }
                }
            }
            None => {
                // Gas exhausted, continue
            }
            Some(_) => {}
        }
    }

    // Restore callee-saved registers, SP, and LR.
    proc.set(RegisterIndex::R4, saved_r4);
    proc.set(RegisterIndex::R5, saved_r5);
    proc.set(RegisterIndex::R6, saved_r6);
    proc.set(RegisterIndex::R7, saved_r7);
    proc.set(RegisterIndex::R8, saved_r8);
    proc.set(RegisterIndex::R9, saved_r9);
    proc.set(RegisterIndex::R10, saved_r10);
    proc.set(RegisterIndex::R11, saved_r11);
    proc.set_sp(saved_sp);
    proc.set_lr(saved_lr);
    Ok(())
}

/// Sync all in-progress animated layer frames/bounds to emulated memory.
/// Called each tick while animations are active so emulated code that reads
/// layer struct fields sees the interpolated values.
fn sync_animated_layers(proc: &mut Processor, _state: &EmuState) {
    unsafe {
        // Walk the animation list and sync any property animations in progress
        // We access the ANIMATIONS vec through has_active_animations / tick path
        // but we can also iterate directly since we're in the same unsafe context
        for &anim_ptr in pebble_api::get_animations().iter() {
            if anim_ptr.is_null() { continue; }
            let anim = &*anim_ptr;
            if !anim.scheduled { continue; }
            if !anim.is_property_anim || anim.emu_layer_handle == 0 || anim.target_layer.is_null() {
                // Also check children of composite animations
                if anim.anim_type != pebble_api::AnimType::Single {
                    for &child in &anim.children {
                        if child.is_null() { continue; }
                        let c = &*child;
                        if c.is_property_anim && c.emu_layer_handle != 0 && !c.target_layer.is_null() {
                            let layer = &*c.target_layer;
                            if c.is_bounds_anim {
                                write_grect(proc, c.emu_layer_handle + LAYER_OFF_BOUNDS, &layer.bounds);
                            } else {
                                write_grect(proc, c.emu_layer_handle + LAYER_OFF_FRAME, &layer.frame);
                            }
                        }
                    }
                }
                continue;
            }
            let layer = &*anim.target_layer;
            if anim.is_bounds_anim {
                write_grect(proc, anim.emu_layer_handle + LAYER_OFF_BOUNDS, &layer.bounds);
            } else {
                write_grect(proc, anim.emu_layer_handle + LAYER_OFF_FRAME, &layer.frame);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App event loop (emulated)
// ---------------------------------------------------------------------------

fn run_event_loop(
    proc: &mut Processor,
    state: &mut EmuState,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    println!("[emu] Entering app_event_loop");
    unsafe { TRACE_ALL = false; }

    // Allocate emulated framebuffer for apps that directly access ctx->dest_bitmap->addr
    let fb_size = (DISPLAY_WIDTH * DISPLAY_HEIGHT) as u32;
    {
        let aligned = (fb_size + 7) & !7;
        let addr = HEAP_BASE + state.heap_offset;
        state.heap_offset += aligned;
        state.emu_fb_addr = addr;
    }

    // Create a handle for the GContext — update procs receive this as their ctx arg
    let gctx_ptr = &mut state.gctx as *mut PblGContext as usize;
    let gctx_handle = state.to_handle(gctx_ptr);
    // Create a GBitmap struct in handle RAM for the framebuffer
    let fb_bitmap_handle = state.to_handle(0); // placeholder host pointer
    {
        let w = DISPLAY_WIDTH as i16;
        let h = DISPLAY_HEIGHT as i16;
        let _ = proc.write_u32_aligned(fb_bitmap_handle, state.emu_fb_addr); // data pointer
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 4, w as u16); // row_size_bytes
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 6, 1); // info_flags: 8Bit format
        // bounds: GRect at offset 8
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 8, 0); // x
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 10, 0); // y
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 12, w as u16); // w
        let _ = proc.write_u16_aligned(fb_bitmap_handle + 14, h as u16); // h
    }
    // Write GContext struct: Pebble GContext has dest_bitmap at offset 0
    let _ = proc.write_u32_aligned(gctx_handle, fb_bitmap_handle);

    let mut tick_count = 0u32;

    loop {
        if stop.load(Ordering::Relaxed) {
            println!("[emu] app_event_loop stopping");
            break;
        }

        // Get current time
        let mut now: libc::time_t = 0;
        unsafe {
            libc::time(&mut now);
        }
        let tm = unsafe { *libc::localtime(&now) };
        state.cached_tm = Some(tm);
        write_tm(proc, TM_BUF_ADDR, &tm);

        // Fire expired app timers (drain first — callbacks may register new timers)
        let now_instant = std::time::Instant::now();
        let mut fired = Vec::new();
        state.timers.retain(|t| {
            if now_instant >= t.deadline {
                fired.push((t.callback_addr, t.context));
                false
            } else {
                true
            }
        });
        for (cb_addr, ctx) in fired {
            call_callback(proc, state, cb_addr, &[ctx])?;
        }

        // Call click config provider on first iteration (sets up button handlers)
        if tick_count == 0 && state.click_config_provider != 0 {
            call_callback(proc, state, state.click_config_provider, &[state.click_config_context])?;
        }

        // Process pending button presses
        let buttons: Vec<u8> = {
            let mut pending = state.pending_buttons.lock().unwrap();
            pending.drain(..).collect()
        };
        for button in buttons {
            let idx = button as usize;
            if idx < 4 && state.single_click_handlers[idx] != 0 {
                state.current_button = button;
                let handler = state.single_click_handlers[idx];
                let context = state.click_contexts[idx];
                // Pebble click handler signature: void handler(ClickRecognizerRef recognizer, void *context)
                // recognizer is opaque — we pass 0
                call_callback(proc, state, handler, &[0, context])?;
            }
        }

        // Call tick handler
        if state.tick_handler_addr != 0 {
            call_callback(
                proc,
                state,
                state.tick_handler_addr,
                &[TM_BUF_ADDR, state.tick_units],
            )?;
        }

        // Begin frame: clear front buffer before redraw
        pebble_api::begin_frame();

        // Sync front buffer → emulated framebuffer before update procs
        if state.emu_fb_addr != 0 {
            if let Some(fb_ptr) = pebble_api::get_framebuffer_ptr() {
                for i in 0..fb_size {
                    let byte = unsafe { *fb_ptr.add(i as usize) };
                    let _ = proc.write_u8(state.emu_fb_addr + i as u32, byte);
                }
            }
        }

        // Call emulated update procs (from layer_set_update_proc)
        let procs: Vec<(u32, u32)> = state.update_procs.clone();
        for &(layer_handle, proc_addr) in &procs {
            if !state.update_procs.contains(&(layer_handle, proc_addr)) ||
                crate::owned::generation(state.from_handle::<PblLayer>(layer_handle)).is_none() { continue; }
            call_callback(proc, state, proc_addr, &[layer_handle, gctx_handle])?;
        }

        // Sync emulated framebuffer → front buffer ONLY if app captured the framebuffer
        // (i.e., uses direct pixel manipulation via graphics_capture_frame_buffer).
        // Apps that only use drawing API calls already drew to the front buffer directly.
        if state.captured_fb_handle != 0 && state.emu_fb_addr != 0 {
            if let Some(fb_ptr) = pebble_api::get_framebuffer_ptr() {
                for i in 0..fb_size {
                    if let Ok(byte) = proc.read_u8(state.emu_fb_addr + i as u32) {
                        unsafe { *fb_ptr.add(i as usize) = byte; }
                    }
                }
            }
        }

        // Call host-side update procs (text_layer, bitmap_layer, etc.)
        pebble_api::call_host_layer_update_procs(&mut state.gctx as *mut PblGContext);

        // End frame: snapshot completed front buffer to back buffer for Slint to read
        pebble_api::end_frame();

        // Deliver accelerometer data if handler is subscribed
        if state.accel_handler_addr != 0 {
            let rate = state.accel_sampling_rate.max(1);
            let spu = state.accel_samples_per_update.max(1);
            let interval_ms = (1000 * spu) / rate;
            let now_accel = std::time::Instant::now();
            let should_fire = match state.accel_last_poll {
                Some(last) => now_accel.duration_since(last).as_millis() >= interval_ms as u128,
                None => true,
            };
            if should_fire {
                state.accel_last_poll = Some(now_accel);
                let num = spu as usize;
                // Allocate AccelData array on emulated heap (15 bytes each, packed)
                let accel_buf = state.emu_malloc(proc, (num * 15) as u32);
                if accel_buf != 0 {
                    let data = crate::accel::peek_latest();
                    for i in 0..num {
                        let addr = accel_buf + (i as u32) * 15;
                        write_accel_data(proc, addr, &data);
                    }
                    let handler = state.accel_handler_addr;
                    call_callback(proc, state, handler, &[accel_buf, num as u32])?;
                }
            }
        }

        // Tick animations and dispatch events
        let anim_events = pebble_api::tick_animations();
        for event in &anim_events {
            match event {
                pebble_api::AnimEvent::Started(anim_ptr, generation) => unsafe {
                    if crate::owned::generation(*anim_ptr) != Some(*generation) { continue; }
                    let anim = &**anim_ptr;
                    if anim.emu_started_handler != 0 {
                        let anim_handle = state.find_handle(*anim_ptr as usize);
                        if anim_handle != 0 {
                            let _ = call_callback(
                                proc, state,
                                anim.emu_started_handler,
                                &[anim_handle, anim.context as u32],
                            );
                        }
                    }
                }
                pebble_api::AnimEvent::Stopped(anim_ptr, finished, generation) => unsafe {
                    if crate::owned::generation(*anim_ptr) != Some(*generation) { continue; }
                    let anim = &**anim_ptr;
                    // Sync final layer state to emulated memory
                    if anim.is_property_anim && anim.emu_layer_handle != 0 && !anim.target_layer.is_null() {
                        let layer = &*anim.target_layer;
                        if anim.is_bounds_anim {
                            write_grect(proc, anim.emu_layer_handle + LAYER_OFF_BOUNDS, &layer.bounds);
                        } else {
                            write_grect(proc, anim.emu_layer_handle + LAYER_OFF_FRAME, &layer.frame);
                        }
                    }
                    if anim.emu_stopped_handler != 0 {
                        let anim_handle = state.find_handle(*anim_ptr as usize);
                        if anim_handle != 0 {
                            let _ = call_callback(
                                proc, state,
                                anim.emu_stopped_handler,
                                &[anim_handle, *finished as u32, anim.context as u32],
                            );
                        }
                    }
                }
            }
        }
        // Sync in-progress animated layers to emulated memory each tick
        if pebble_api::has_active_animations() {
            sync_animated_layers(proc, state);
        }

        // Debug log
        if tick_count < 5 {
            if let Some(fb) = pebble_api::get_fb() {
                let nonblack = fb.iter().filter(|&&b| b != 0xC0 && b != 0).count();
                let host_layers = unsafe { pebble_api::host_layer_count() };
                eprintln!(
                    "[emu] tick {} fb: nonblack={} layers={} host_layers={} tick={}",
                    tick_count,
                    nonblack,
                    state.update_procs.len(),
                    host_layers,
                    state.tick_handler_addr != 0
                );
            }
        }
        tick_count += 1;

        // Sleep less if timers, accel, or animations are active
        let sleep_ms = if !state.timers.is_empty()
            || state.accel_handler_addr != 0
            || pebble_api::has_active_animations()
        { 50 } else { 1000 };
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }

    println!("[emu] app_event_loop exited");
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn load_and_execute(
    bin_data: &[u8],
    info: &PebbleProcessInfo,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    load_and_execute_with_buttons(bin_data, info, stop, None)
}

pub fn load_and_execute_with_buttons(
    bin_data: &[u8],
    info: &PebbleProcessInfo,
    stop: Arc<AtomicBool>,
    button_queue: Option<Arc<Mutex<Vec<u8>>>>,
) -> Result<(), String> {
    let _session = crate::runtime::SessionCleanup;
    crate::pbw::validate_binary(bin_data, info)?;
    let virtual_size = info.virtual_size as u32;
    let load_size = info.load_size as u32;

    if virtual_size == 0 || load_size == 0 {
        return Err("Invalid binary sizes".into());
    }

    // Create ARMv7-M processor
    let mut proc = Processor::new(Config::v7em()); // V7EM: Cortex-M4 with DSP (Pebble uses SIMD)

    // --- Map memory regions ---
    let bin_alloc = virtual_size;
    proc.map_ram(BIN_BASE, bin_alloc)
        .map_err(|e| format!("Map binary: {:?}", e))?;

    proc.map_ram(STACK_BASE, STACK_SIZE)
        .map_err(|e| format!("Map stack: {:?}", e))?;

    proc.map_ram(HEAP_BASE, HEAP_SIZE)
        .map_err(|e| format!("Map heap: {:?}", e))?;

    proc.map_ram(TRAMPOLINE_BASE, TRAMPOLINE_SIZE)
        .map_err(|e| format!("Map trampoline: {:?}", e))?;

    let jt_size = JUMP_TABLE_SIZE as u32 * 4;
    proc.map_ram(JT_BASE, jt_size)
        .map_err(|e| format!("Map jump table: {:?}", e))?;

    proc.map_ram(TM_BUF_ADDR, TM_BUF_SIZE)
        .map_err(|e| format!("Map tm buf: {:?}", e))?;

    // Safety-net RAM at handle base — prevents crashes when apps directly
    // dereference handle pointers to access struct fields
    proc.map_ram(HANDLE_RAM_BASE, HANDLE_RAM_SIZE)
        .map_err(|e| format!("Map handle RAM: {:?}", e))?;

    // --- Copy binary ---
    let copy_len = (bin_data.len() as u32).min(load_size).min(bin_alloc);
    for i in 0..copy_len {
        proc.write_u8(BIN_BASE + i, bin_data[i as usize])
            .map_err(|e| format!("Write binary byte {}: {:?}", i, e))?;
    }
    println!("[emu] Copied {} bytes to 0x{:08x}", copy_len, BIN_BASE);

    // --- Apply relocations ---
    let num_relocs = info.num_reloc_entries as usize;
    for i in 0..num_relocs {
        let reloc_entry_offset = load_size as usize + i * 4;
        if reloc_entry_offset + 4 > bin_data.len() {
            return Err(format!("Relocation entry {} out of bounds", i));
        }
        let target_offset = u32::from_le_bytes([
            bin_data[reloc_entry_offset],
            bin_data[reloc_entry_offset + 1],
            bin_data[reloc_entry_offset + 2],
            bin_data[reloc_entry_offset + 3],
        ]);

        let addr = BIN_BASE + target_offset;
        let old_val = proc
            .read_u32_unaligned(addr)
            .map_err(|e| format!("Read reloc {}: {:?}", i, e))?;
        let new_val = old_val.wrapping_add(BIN_BASE);
        proc.write_u32_unaligned(addr, new_val)
            .map_err(|e| format!("Write reloc {}: {:?}", i, e))?;
    }
    println!("[emu] Applied {} relocations", num_relocs);

    // --- Fill jump table with trampoline addresses ---
    for i in 0..JUMP_TABLE_SIZE {
        let trampoline_addr = TRAMPOLINE_BASE + (i as u32) * 4;
        proc.write_u32_aligned(JT_BASE + (i as u32) * 4, trampoline_addr | 1)
            .map_err(|e| format!("Write JT entry {}: {:?}", i, e))?;
    }

    // --- Write jump table pointer into binary ---
    let jt_ptr_addr = BIN_BASE + info.sym_table_addr as u32;
    proc.write_u32_aligned(jt_ptr_addr, JT_BASE)
        .map_err(|e| format!("Write JT ptr: {:?}", e))?;
    println!(
        "[emu] Jump table at 0x{:08x}, ptr at 0x{:08x}",
        JT_BASE, jt_ptr_addr
    );

    // --- Hook trampoline region ---
    proc.hook_code(
        TRAMPOLINE_BASE as usize..(TRAMPOLINE_BASE + TRAMPOLINE_SIZE) as usize,
    );


    // --- Set up registers ---
    proc.set_sp(STACK_BASE + STACK_SIZE); // Stack grows down
    let entry = BIN_BASE + info.entry_point as u32;
    proc.set_pc(entry); // Armagnac handles Thumb mode for v7m
    println!("[emu] Entry point: 0x{:08x}, SP: 0x{:08x}", entry, proc.sp());

    // --- Initialize state ---
    let mut state = EmuState::new();
    if let Some(bq) = button_queue {
        state.pending_buttons = bq;
    }
    pebble_api::set_stop_flag(stop.clone());

    // Build the native jump table too (for name lookups in stubs)
    executor::build_jump_table();

    // --- Main execution loop ---
    println!("[emu] Starting emulation...");

    loop {
        if stop.load(Ordering::Relaxed) { break; }
        let event = proc
            .run(RunOptions::new().gas(500000))
            .map_err(|e| {
                let pc = proc[RegisterIndex::Pc];
                format!("CPU error: {:?} (PC=0x{:08x})", e, pc)
            })?;

        match event {
            Some(Event::Hook { address }) => {
                if address == CALLBACK_RETURN_ADDR {
                    // Top-level callback return — shouldn't happen
                    println!("[emu] Top-level callback return, exiting");
                    break;
                }


                let idx = ((address - TRAMPOLINE_BASE) / 4) as usize;
                if idx >= JUMP_TABLE_SIZE + 4 {
                    eprintln!("[emu] Hook at unknown address 0x{:08x}", address);
                    break;
                }

                let lr = proc.lr();
                let action = dispatch(&mut proc, &mut state, idx);

                match action {
                    Action::Return(val) => {
                        proc.set(RegisterIndex::R0, val);
                        let ret_pc = lr & !1;
                        // Sanity check: return address should be in app binary, not heap/stack
                        if ret_pc >= HEAP_BASE && ret_pc < HEAP_BASE + HEAP_SIZE {
                            eprintln!("[emu] WARNING: returning to heap address 0x{:08x} (LR=0x{:08x}) from #{}", ret_pc, lr, idx);
                        }
                        proc.set_pc(ret_pc);
                    }
                    Action::EventLoop => {
                        // Window load handler is already called synchronously
                        // by window_stack_push, matching PebbleOS behavior.
                        // Run the event loop
                        run_event_loop(&mut proc, &mut state, &stop)?;
                        // app_event_loop returned, set PC to return
                        proc.set(RegisterIndex::R0, 0);
                        proc.set_pc(lr & !1);
                    }
                }
            }
            None => {
                // Gas exhausted, continue running
            }
            Some(Event::Break(val)) => {
                eprintln!("[emu] BKPT {} at PC=0x{:08x}", val, proc.pc());
                break;
            }
            _ => {}
        }
    }

    println!("[emu] Emulation finished");
    Ok(())
}
