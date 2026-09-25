//! Pebble API implementations.
//!
//! These are `extern "C"` functions that get wired into the jump table.
//! The Pebble binary calls them as if they were the real PebbleOS functions.
//! We use global state since the Pebble app is single-threaded.

use crate::font;
use crate::gcolor;
use crate::runtime::{DISPLAY_HEIGHT, DISPLAY_WIDTH};
pub const DISPLAY_WIDTH_I16: i16 = DISPLAY_WIDTH as i16;
pub const DISPLAY_HEIGHT_I16: i16 = DISPLAY_HEIGHT as i16;
use image::GenericImageView;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Global runtime state (accessed by the Pebble API functions)
// ---------------------------------------------------------------------------

static mut STOP_FLAG: Option<Arc<AtomicBool>> = None;
static mut FRAMEBUFFER: Option<*mut u8> = None;
static mut TICK_HANDLER: Option<TickHandler> = None;
static mut TICK_UNITS: u32 = 0;
static mut TICK_STATE: crate::ticks::TickState = crate::ticks::TickState::new();
static mut FRAME_DIRTY: bool = true;

type ClickProvider = extern "C" fn(*mut u8);
type ClickHandler = extern "C" fn(usize, *mut u8);
#[derive(Clone, Copy)]
struct ClickConfig { window: usize, provider: Option<ClickProvider>, context: usize }
static mut CLICK_CONFIGS: Vec<ClickConfig> = Vec::new();
static mut CLICK_HANDLERS: [Option<ClickHandler>; 4] = [None; 4];
static mut CLICK_CONTEXTS: [usize; 4] = [0; 4];
static mut BUTTON_QUEUE: Option<Arc<std::sync::Mutex<Vec<u8>>>> = None;

pub fn set_button_queue(queue: Arc<std::sync::Mutex<Vec<u8>>>) {
    unsafe { BUTTON_QUEUE = Some(queue); }
}

fn configure_native_clicks() {
    unsafe {
        CLICK_HANDLERS = [None; 4];
        CLICK_CONTEXTS = [CURRENT_WINDOW as usize; 4];
        let config = CLICK_CONFIGS.iter().find(|c| c.window == CURRENT_WINDOW as usize).copied();
        if let Some(config) = config {
            CLICK_CONTEXTS = [config.context; 4];
            if let Some(provider) = config.provider { provider(config.context as *mut u8); }
        }
    }
}

pub fn dispatch_native_clicks() {
    let buttons = unsafe { BUTTON_QUEUE.as_ref().map(|q| std::mem::take(&mut *q.lock().unwrap())).unwrap_or_default() };
    for button in buttons {
        if stop_requested() { break; }
        unsafe {
            if crate::owned::generation(CURRENT_WINDOW).is_none() || button >= 4 { continue; }
            if let Some(handler) = CLICK_HANDLERS[button as usize] {
                // Recognizer is valid only for this callback, like Pebble's opaque handle.
                let mut recognizer = button;
                handler(&mut recognizer as *mut u8 as usize, CLICK_CONTEXTS[button as usize] as *mut u8);
                FRAME_DIRTY = true;
            }
        }
    }
}

// Registered window handlers
static mut WINDOW_LOAD_HANDLER: Option<extern "C" fn(*mut PblWindow)> = None;
static mut WINDOW_UNLOAD_HANDLER: Option<extern "C" fn(*mut PblWindow)> = None;

// Draw registrations are replaced by identity and checked again after each callback.
#[derive(Clone, Copy)]
struct DrawRegistration { layer: usize, callback: LayerUpdateProc, generation: u64, revision: u64 }
static mut LAYER_UPDATE_PROCS: Vec<DrawRegistration> = Vec::new();
static mut NEXT_DRAW_REVISION: u64 = 1;

type TickHandler = extern "C" fn(*mut libc::tm, u32);
type LayerUpdateProc = extern "C" fn(*mut PblLayer, *mut PblGContext);

pub fn stop_requested() -> bool {
    unsafe { STOP_FLAG.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) }
}

pub fn set_stop_flag(flag: Arc<AtomicBool>) {
    unsafe { STOP_FLAG = Some(flag); }
}

pub fn set_framebuffer_ptr(fb: *mut u8) {
    unsafe {
        FRAMEBUFFER = Some(fb);
        // Initialize back buffer
        crate::display_frame::publish(vec![0u8; DISPLAY_WIDTH * DISPLAY_HEIGHT]);
    }
}

pub fn get_framebuffer_ptr() -> Option<*mut u8> {
    unsafe { FRAMEBUFFER }
}

/// Return the number of host-side layer update procs registered.
pub fn host_layer_count() -> usize {
    unsafe { LAYER_UPDATE_PROCS.len() }
}

/// Begin a frame: clear the front buffer (rendering target) before redraw.
pub fn begin_frame() {
    if let Some(fb) = fb() {
        unsafe { fb.fill(WINDOW_BG_COLOR); }
    }
}

/// End a frame: snapshot the completed front buffer into the back buffer.
/// Slint reads from the back buffer, so it always gets a complete frame.
pub fn end_frame() {
    unsafe {
        if let Some(ptr) = FRAMEBUFFER {
            let mut pixels = vec![0; DISPLAY_WIDTH * DISPLAY_HEIGHT];
            std::ptr::copy_nonoverlapping(ptr, pixels.as_mut_ptr(), pixels.len());
            crate::display_frame::publish(pixels);
        }
    }
}

/// Own an immutable completed frame, even while the worker publishes the next one.
pub fn get_display_buffer() -> Option<Arc<[u8]>> {
    crate::display_frame::snapshot()
}

/// Clear framebuffer with window background color. Called before redraw each tick.
pub fn clear_framebuffer() {
    if let Some(fb) = fb() {
        unsafe { fb.fill(WINDOW_BG_COLOR); }
    }
}

/// Call all host-side layer update procs (text_layer, bitmap_layer, etc.)
/// Used by the emulator event loop which otherwise only calls emulated update procs.
pub fn call_host_layer_update_procs(gctx: *mut PblGContext) {
    let callbacks = unsafe { LAYER_UPDATE_PROCS.clone() };
    for registration in callbacks {
        let active = unsafe { LAYER_UPDATE_PROCS.iter().any(|r| r.revision == registration.revision) };
        if active && crate::owned::generation(registration.layer as *const PblLayer) == Some(registration.generation) {
            (registration.callback)(registration.layer as *mut PblLayer, gctx);
        }
    }
}

fn register_draw(layer: *mut PblLayer, callback: Option<LayerUpdateProc>) {
    unsafe {
        LAYER_UPDATE_PROCS.retain(|r| r.layer != layer as usize);
        if let (Some(callback), Some(generation)) = (callback, crate::owned::generation(layer)) {
            let revision = NEXT_DRAW_REVISION;
            NEXT_DRAW_REVISION = NEXT_DRAW_REVISION.checked_add(1).expect("draw revision exhausted");
            LAYER_UPDATE_PROCS.push(DrawRegistration { layer: layer as usize, callback, generation, revision });
        }
    }
}

fn set_subtree_window(layer: *mut PblLayer, window: *mut PblWindow) {
    let mut pending = vec![layer];
    let mut seen = std::collections::HashSet::new();
    while let Some(layer) = pending.pop() {
        if crate::owned::generation(layer).is_none() || !seen.insert(layer as usize) { continue; }
        unsafe {
            (*layer).window = window;
            let mut child = (*layer).first_child;
            while !child.is_null() {
                pending.push(child);
                child = (*child).next_sibling;
            }
        }
    }
}

/// Detach first, then invalidate callbacks/animation targets before freeing any layer.
fn dispose_layer(layer: *mut PblLayer) {
    if crate::owned::generation(layer).is_none() { return; }
    unsafe {
        let window = (*layer).window;
        if crate::owned::generation(window).is_some() && (*window).root_layer == layer {
            (*window).root_layer = std::ptr::null_mut();
        }
    }
    pbl_layer_remove_from_parent(layer);
    pbl_layer_remove_child_layers(layer);
    register_draw(layer, None);
    unsafe {
        let mut pending = ALL_ANIMATIONS.clone();
        let mut seen = std::collections::HashSet::new();
        while let Some(anim) = pending.pop() {
            if crate::owned::generation(anim).is_none() || !seen.insert(anim as usize) { continue; }
            pending.extend((*anim).children.iter().copied());
            if (*anim).target_layer == layer {
                (*anim).target_layer = std::ptr::null_mut();
                (*anim).scheduled = false;
            }
        }
    }
    crate::owned::release(layer);
}

pub fn set_resource_pack(data: Vec<u8>) {
    *RESOURCE_PACK.lock().unwrap() = Some(crate::resources::ResourcePack::new(data));
}

/// Reset all global state between app launches
pub fn reset_state() {
    unsafe {
        STOP_FLAG = None;
        BUTTON_QUEUE = None;
        CLICK_CONFIGS.clear();
        CLICK_HANDLERS = [None; 4];
        CLICK_CONTEXTS = [0; 4];
        TICK_HANDLER = None;
        TICK_UNITS = 0;
        WINDOW_LOAD_HANDLER = None;
        WINDOW_UNLOAD_HANDLER = None;
        LAYER_UPDATE_PROCS.clear();
        BATTERY_HANDLER = None;
        LAST_BATTERY_STATE = u32::MAX;
        FRAMEBUFFER = None;
        WINDOW_USER_DATA = 0;
        CURRENT_WINDOW = std::ptr::null_mut();
        WINDOW_BG_COLOR = 0x00; // BLACK
        CAPTURED_BITMAP = None;
        APP_TIMERS.clear();
        NEXT_TIMER_ID = 1;
        *RESOURCE_PACK.lock().unwrap() = None;
        font::reset_app_fonts();
        crate::accel::reset();
        crate::compass::reset();
        crate::bluetooth::reset();
        crate::persist::reset();
        TICK_STATE = crate::ticks::TickState::default();
        FRAME_DIRTY = true;
        reset_animations();
        reset_app_message();
        crate::owned::release(OUTBOX_BUFFER);
        OUTBOX_BUFFER = std::ptr::null_mut();
        crate::owned::clear();
        crate::guest_heap::reset();
    }
}

// ---------------------------------------------------------------------------
// Pebble types (matching the C ABI layout)
// ---------------------------------------------------------------------------

/// GRect: { GPoint origin; GSize size; } = { i16 x, y; i16 w, h; } = 8 bytes
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GRect {
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
}

/// GPoint: { i16 x, y; } = 4 bytes
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GPoint {
    pub x: i16,
    pub y: i16,
}

/// GSize: { i16 w, h; } = 4 bytes
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GSize {
    pub w: i16,
    pub h: i16,
}

/// GColor8: single byte AARRGGBB
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GColor8(pub u8);

/// Window struct — root layer pointer at offset 0.
/// NOTE: PebbleOS embeds the Layer at offset 0, but some EffectLayer versions
/// scan the Window looking for the root layer address which won't match an embedded layer.
/// We keep it as a pointer for simplicity; the EffectLayer scan finds the parent
/// via PblLayer.parent (offset 24) which works regardless.
#[repr(C)]
pub struct PblWindow {
    pub root_layer: *mut PblLayer,
}

/// Layer struct — layout MUST match PebbleOS exactly (apps read at hardcoded offsets)
/// PebbleOS offsets: bounds(0), frame(8), flags(16), next_sibling(20), parent(24),
///                   first_child(28), window(32), update_proc(36), data(40+)
#[repr(C)]
pub struct PblLayer {
    pub bounds: GRect,                         // offset 0, 8 bytes
    pub frame: GRect,                          // offset 8, 8 bytes
    pub flags: u32,                            // offset 16, 4 bytes (includes clips, hidden)
    pub next_sibling: *mut PblLayer,           // offset 20, 4 bytes
    pub parent: *mut PblLayer,                 // offset 24, 4 bytes
    pub first_child: *mut PblLayer,            // offset 28, 4 bytes
    pub window: *mut PblWindow,                // offset 32, 4 bytes
    pub update_proc: Option<LayerUpdateProc>,  // offset 36, 4 bytes
    // data follows at offset 40 for layer_create_with_data
}

/// GContext — passed to layer update procs
#[repr(C)]
pub struct PblGContext {
    pub fill_color: u8,
    pub stroke_color: u8,
    pub text_color: u8,
    pub stroke_width: u8,
}

/// TextLayer
#[repr(C)]
pub struct PblTextLayer {
    pub layer: PblLayer,
    pub text: *const std::ffi::c_char,
    pub font: *const u8,
    pub color: u8,
    pub bg_color: u8,
    pub alignment: u8,
    pub overflow_mode: u8,
}

/// BitmapLayer
#[repr(C)]
pub struct PblBitmapLayer {
    pub layer: PblLayer,
    pub bitmap: *mut u8,
    pub compositing_mode: u8,
}

/// WindowHandlers — passed to window_set_window_handlers
/// In Pebble: struct { WindowHandler load; WindowHandler appear; WindowHandler disappear; WindowHandler unload; }
#[repr(C)]
pub struct WindowHandlers {
    pub load: Option<extern "C" fn(*mut PblWindow)>,
    pub appear: Option<extern "C" fn(*mut PblWindow)>,
    pub disappear: Option<extern "C" fn(*mut PblWindow)>,
    pub unload: Option<extern "C" fn(*mut PblWindow)>,
}

/// GPathInfo
#[repr(C)]
pub struct GPathInfo {
    pub num_points: u32,
    pub points: *const GPoint,
}

/// GPath — allocated by gpath_create
#[repr(C)]
pub struct GPath {
    pub num_points: u32,
    pub points: *const GPoint,
    pub rotation: i32,
    pub offset: GPoint,
}

// ---------------------------------------------------------------------------
// Core app lifecycle
// ---------------------------------------------------------------------------

/// app_event_loop — this is where the Pebble app "blocks".
/// We run a tick loop here, dispatching tick events and layer redraws.
#[no_mangle]
pub extern "C" fn pbl_app_event_loop() {
    let stop = unsafe { STOP_FLAG.as_ref().cloned() };
    let mut gctx = PblGContext {
        fill_color: gcolor::colors::BLACK, stroke_color: gcolor::colors::WHITE,
        text_color: gcolor::colors::BLACK, stroke_width: 1,
    };
    let mut last_second = None;
    loop {
        if stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed)) { break; }
        dispatch_native_clicks();
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        if last_second != Some(now) {
            last_second = Some(now);
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&now, &mut tm); }
            unsafe {
                if let Some(handler) = TICK_HANDLER {
                    if let Some(changed) = TICK_STATE.update(&tm, TICK_UNITS) {
                        handler(&mut tm, changed);
                        FRAME_DIRTY = true;
                    }
                }
            }
            // Battery work is needed only for clients listening for changes.
            if unsafe { BATTERY_HANDLER.is_some() } { poll_battery(); }
            if crate::bluetooth::poll() { unsafe { FRAME_DIRTY = true; } }
        }
        let now_mono = std::time::Instant::now();
        let mut fired = Vec::new();
        unsafe {
            APP_TIMERS.retain(|t| {
                if now_mono >= t.deadline { fired.push((t.callback, t.data)); false } else { true }
            });
        }
        for (callback, data) in fired {
            callback(data);
            unsafe { FRAME_DIRTY = true; }
        }
        let events = tick_animations();
        if !events.is_empty() { unsafe { FRAME_DIRTY = true; } }
        dispatch_native_anim_events(&events);
        let accel_changed = crate::accel::poll_and_deliver();
        let compass_changed = crate::compass::poll();
        if accel_changed || compass_changed {
            unsafe { FRAME_DIRTY = true; }
        }
        if unsafe { FRAME_DIRTY } {
            unsafe { FRAME_DIRTY = false; }
            begin_frame();
            call_host_layer_update_procs(&mut gctx);
            end_frame();
        }
        // Idle watchfaces sleep to the next second (battery/clock polling).
        // Timers retain their deadlines; interactive apps and sensors stay responsive.
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().subsec_millis();
        let mut delay = std::time::Duration::from_millis((1000 - millis) as u64);
        if has_active_animations() || crate::accel::has_handler() || crate::compass::has_handler()
            || unsafe { BUTTON_QUEUE.is_some() } {
            delay = delay.min(std::time::Duration::from_millis(50));
        }
        unsafe {
            for timer in &APP_TIMERS { delay = delay.min(timer.deadline.saturating_duration_since(std::time::Instant::now())); }
        }
        std::thread::sleep(delay);
    }
}

/// app_log(level, filename, line_number, fmt, ...)
#[no_mangle]
pub extern "C" fn pbl_app_log(level: u8, _filename: *const std::ffi::c_char, line: u16, fmt: *const std::ffi::c_char) {
    let fmt_str = unsafe {
        if fmt.is_null() {
            "<null>"
        } else {
            std::ffi::CStr::from_ptr(fmt).to_str().unwrap_or("<invalid>")
        }
    };
    println!("[pebble:log] level={} line={}: {}", level, line, fmt_str);
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_window_create() -> *mut PblWindow {
    let root_layer = crate::owned::new(PblLayer {
        bounds: GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 },
        frame: GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 },
        flags: 1,
        next_sibling: std::ptr::null_mut(),
        parent: std::ptr::null_mut(),
        first_child: std::ptr::null_mut(),
        window: std::ptr::null_mut(), // set below
        update_proc: None,
    });

    let window = crate::owned::new(PblWindow { root_layer });
    // Point root layer back to its window
    unsafe { (*root_layer).window = window; }
    println!("[pebble] window_create() -> {:?}", window);
    window
}

#[no_mangle]
pub extern "C" fn pbl_window_destroy(window: *mut PblWindow) {
    if crate::owned::generation(window).is_none() { return; }
    unsafe {
        CLICK_CONFIGS.retain(|c| c.window != window as usize);
        if CURRENT_WINDOW == window {
            CURRENT_WINDOW = std::ptr::null_mut();
            CLICK_HANDLERS = [None; 4];
            CLICK_CONTEXTS = [0; 4];
        }
        dispose_layer((*window).root_layer);
    }
    crate::owned::release(window);
}

#[no_mangle]
pub extern "C" fn pbl_window_set_window_handlers(window: *mut PblWindow, handlers: WindowHandlers) {
    println!("[pebble] window_set_window_handlers(load={}, unload={})",
        handlers.load.is_some(), handlers.unload.is_some());
    unsafe {
        WINDOW_LOAD_HANDLER = handlers.load;
        WINDOW_UNLOAD_HANDLER = handlers.unload;
    }
}

#[no_mangle]
pub extern "C" fn pbl_window_stack_push(window: *mut PblWindow, _animated: bool) {
    if crate::owned::generation(window).is_none() { return; }
    println!("[pebble] window_stack_push({:?})", window);
    unsafe { CURRENT_WINDOW = window; }
    // Trigger window load handler
    unsafe {
        if let Some(load) = WINDOW_LOAD_HANDLER {
            println!("[pebble]   calling window load handler");
            load(window);
        }
    }
    configure_native_clicks();
}

#[no_mangle]
pub extern "C" fn pbl_window_get_root_layer(window: *mut PblWindow) -> *mut PblLayer {
    if window.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*window).root_layer }
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_layer_create(frame: GRect) -> *mut PblLayer {
    let layer = crate::owned::new(PblLayer {
        bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
        frame,
        flags: 1, // clips=true by default (PebbleOS default)
        next_sibling: std::ptr::null_mut(),
        parent: std::ptr::null_mut(),
        first_child: std::ptr::null_mut(),
        window: std::ptr::null_mut(),
        update_proc: None,
    });
    println!("[pebble] layer_create({}x{}+{}+{}) -> {:?}",
        frame.w, frame.h, frame.x, frame.y, layer);
    layer
}

#[no_mangle]
pub extern "C" fn pbl_layer_destroy(layer: *mut PblLayer) { dispose_layer(layer); }

#[no_mangle]
pub extern "C" fn pbl_layer_get_bounds(layer: *mut PblLayer) -> GRect {
    if layer.is_null() {
        return GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 };
    }
    unsafe { (*layer).bounds }
}

#[no_mangle]
pub extern "C" fn pbl_layer_get_frame(layer: *mut PblLayer) -> GRect {
    if layer.is_null() {
        return GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 };
    }
    unsafe { (*layer).frame }
}

#[no_mangle]
pub extern "C" fn pbl_layer_set_bounds(layer: *mut PblLayer, bounds: GRect) {
    if !layer.is_null() {
        unsafe { (*layer).bounds = bounds; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_layer_set_frame(layer: *mut PblLayer, frame: GRect) {
    if !layer.is_null() {
        unsafe { (*layer).frame = frame; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_layer_set_update_proc(layer: *mut PblLayer, proc: Option<LayerUpdateProc>) {
    if crate::owned::generation(layer).is_some() {
        unsafe { (*layer).update_proc = proc; }
        register_draw(layer, proc);
    }
}

#[no_mangle]
pub extern "C" fn pbl_layer_add_child(parent: *mut PblLayer, child: *mut PblLayer) {
    if parent.is_null() || child.is_null() {
        return;
    }
    if crate::owned::generation(parent).is_none() || crate::owned::generation(child).is_none() { return; }
    unsafe {
        let mut ancestor = parent;
        while !ancestor.is_null() {
            if ancestor == child { return; }
            ancestor = (*ancestor).parent;
        }
    }
    pbl_layer_remove_from_parent(child);
    unsafe {
        // If child is already a child of this parent, skip (prevents cycles)
        if !(*parent).first_child.is_null() {
            let mut cur = (*parent).first_child;
            let mut guard = 0;
            while !cur.is_null() && guard < 256 {
                if cur == child {
                    return; // already added
                }
                cur = (*cur).next_sibling;
                guard += 1;
            }
        }

        // Set child's parent and window
        (*child).parent = parent;
        set_subtree_window(child, (*parent).window);
        (*child).next_sibling = std::ptr::null_mut();

        // Append child to end of sibling list
        if (*parent).first_child.is_null() {
            (*parent).first_child = child;
        } else {
            let mut last = (*parent).first_child;
            while !(*last).next_sibling.is_null() {
                last = (*last).next_sibling;
            }
            (*last).next_sibling = child;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_layer_mark_dirty(_layer: *mut PblLayer) {
    unsafe { FRAME_DIRTY = true; }
}

// ---------------------------------------------------------------------------
// Window extras
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_window_set_background_color(_window: *mut PblWindow, color: GColor8) {
    unsafe { WINDOW_BG_COLOR = color.0; }
    // Fill the framebuffer with the background color
    if let Some(fb) = fb() {
        fb.fill(color.0);
    }
}

#[no_mangle]
pub extern "C" fn pbl_window_set_background_color_2bit(_window: *mut PblWindow, _color: u8) {}

#[no_mangle]
pub extern "C" fn pbl_window_is_loaded(_window: *mut PblWindow) -> bool { true }

static mut WINDOW_USER_DATA: usize = 0;

#[no_mangle]
pub extern "C" fn pbl_window_set_user_data(_window: *mut PblWindow, data: usize) {
    unsafe { WINDOW_USER_DATA = data; }
}

#[no_mangle]
pub extern "C" fn pbl_window_get_user_data(_window: *mut PblWindow) -> usize {
    unsafe { WINDOW_USER_DATA }
}

#[no_mangle]
pub extern "C" fn pbl_watch_info_get_model() -> u32 { 4 } // WatchModelPebbleTimeRound

#[no_mangle]
pub extern "C" fn pbl_watch_info_get_color() -> u32 { 1 } // true = color display

#[no_mangle]
pub extern "C" fn pbl_clock_is_24h_style() -> bool { true }

// ---------------------------------------------------------------------------
// Layer extras
// ---------------------------------------------------------------------------

/// Layer with extra data bytes appended (Pebble's layer_create_with_data)
#[no_mangle]
pub extern "C" fn pbl_layer_create_with_data(frame: GRect, data_size: usize) -> *mut PblLayer {
    // Allocate layer + extra data bytes after it
    let Some(size) = std::mem::size_of::<PblLayer>().checked_add(data_size) else { return std::ptr::null_mut(); };
    let Ok(layout) = std::alloc::Layout::from_size_align(size, std::mem::align_of::<PblLayer>()) else { return std::ptr::null_mut(); };
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) as *mut PblLayer };
    if ptr.is_null() { return ptr; }
    crate::owned::register(ptr, layout);
    unsafe {
        (*ptr).bounds = GRect { x: 0, y: 0, w: frame.w, h: frame.h };
        (*ptr).frame = frame;
        (*ptr).flags = 1; // clips=true
        (*ptr).next_sibling = std::ptr::null_mut();
        (*ptr).parent = std::ptr::null_mut();
        (*ptr).first_child = std::ptr::null_mut();
        (*ptr).window = std::ptr::null_mut();
        (*ptr).update_proc = None;
    }
    ptr
}

#[no_mangle]
pub extern "C" fn pbl_layer_get_data(layer: *mut PblLayer) -> *mut u8 {
    if layer.is_null() { return std::ptr::null_mut(); }
    // Data follows immediately after the Layer struct
    unsafe { (layer as *mut u8).add(std::mem::size_of::<PblLayer>()) }
}

#[no_mangle]
pub extern "C" fn pbl_layer_get_hidden(_layer: *mut PblLayer) -> bool { false }

#[no_mangle]
pub extern "C" fn pbl_layer_set_hidden(_layer: *mut PblLayer, _hidden: bool) {}

#[no_mangle]
pub extern "C" fn pbl_layer_remove_from_parent(layer: *mut PblLayer) {
    if crate::owned::generation(layer).is_none() { return; }
    unsafe {
        let parent = (*layer).parent;
        if crate::owned::generation(parent).is_some() {
            let mut link = &mut (*parent).first_child as *mut *mut PblLayer;
            while !(*link).is_null() {
                if *link == layer { *link = (*layer).next_sibling; break; }
                link = &mut (**link).next_sibling;
            }
        }
        (*layer).parent = std::ptr::null_mut();
        (*layer).next_sibling = std::ptr::null_mut();
        set_subtree_window(layer, std::ptr::null_mut());
    }
}

// ---------------------------------------------------------------------------
// Graphics context
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_fill_color(ctx: *mut PblGContext, color: GColor8) {
    if !ctx.is_null() {
        unsafe { (*ctx).fill_color = color.0; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_stroke_color(ctx: *mut PblGContext, color: GColor8) {
    if !ctx.is_null() {
        unsafe { (*ctx).stroke_color = color.0; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_text_color(ctx: *mut PblGContext, color: GColor8) {
    if !ctx.is_null() {
        unsafe { (*ctx).text_color = color.0; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_compositing_mode(_ctx: *mut PblGContext, _mode: u32) {}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_rect(ctx: *mut PblGContext, rect: GRect) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if let Some(fb) = fb() {
        // Top and bottom edges
        for x in rect.x.max(0)..(rect.x + rect.w).min(DISPLAY_WIDTH as i16) {
            set_pixel(fb, x, rect.y, color);
            set_pixel(fb, x, rect.y + rect.h - 1, color);
        }
        // Left and right edges
        for y in rect.y.max(0)..(rect.y + rect.h).min(DISPLAY_HEIGHT as i16) {
            set_pixel(fb, rect.x, y, color);
            set_pixel(fb, rect.x + rect.w - 1, y, color);
        }
    }
}

// ---------------------------------------------------------------------------
// Graphics drawing (writes to the global framebuffer)
// ---------------------------------------------------------------------------

fn fb() -> Option<&'static mut [u8]> {
    unsafe {
        FRAMEBUFFER.map(|ptr| std::slice::from_raw_parts_mut(ptr, DISPLAY_WIDTH * DISPLAY_HEIGHT))
    }
}

fn set_pixel(fb: &mut [u8], x: i16, y: i16, color: u8) {
    if x >= 0 && x < DISPLAY_WIDTH as i16 && y >= 0 && y < DISPLAY_HEIGHT as i16 {
        fb[y as usize * DISPLAY_WIDTH + x as usize] = color;
    }
}

/// Pebble GCornerMask bits
const CORNER_TOP_LEFT: u8 = 1;
const CORNER_TOP_RIGHT: u8 = 2;
const CORNER_BOTTOM_LEFT: u8 = 4;
const CORNER_BOTTOM_RIGHT: u8 = 8;
const CORNER_ALL: u8 = 0xF;

/// Compute corner insets for a rounded rect using integer midpoint circle.
/// Returns a Vec where entry[dy] = x_inset for that row offset from the corner.
fn corner_insets(radius: i16) -> Vec<i16> {
    if radius <= 0 {
        return vec![];
    }
    let mut insets = vec![0i16; radius as usize];
    let mut x = radius;
    let mut y: i16 = 0;
    let mut err: i16 = 1 - x;
    while y <= x {
        // For row y from corner: inset = radius - x
        insets[y as usize] = radius - x;
        // For row x from corner: inset = radius - y (symmetric)
        if (x as usize) < insets.len() {
            insets[x as usize] = radius - y;
        }
        y += 1;
        if err < 0 {
            err += 2 * y + 1;
        } else {
            x -= 1;
            err += 2 * (y - x) + 1;
        }
    }
    insets
}

#[no_mangle]
pub extern "C" fn pbl_graphics_fill_rect(ctx: *mut PblGContext, rect: GRect, corner_radius: u16, corner_mask: u8) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).fill_color } };
    if let Some(fb) = fb() {
        let r = (corner_radius as i16).min(rect.w / 2).min(rect.h / 2);
        let insets = corner_insets(r);
        let x0 = rect.x;
        let x1 = rect.x + rect.w;
        let y0 = rect.y;
        let y1 = rect.y + rect.h;
        for y in y0.max(0)..y1.min(DISPLAY_HEIGHT as i16) {
            let dy_top = (y - y0) as usize;
            let dy_bot = (y1 - 1 - y) as usize;
            let left_inset_top = if dy_top < insets.len() && (corner_mask & CORNER_TOP_LEFT != 0) { insets[dy_top] } else { 0 };
            let right_inset_top = if dy_top < insets.len() && (corner_mask & CORNER_TOP_RIGHT != 0) { insets[dy_top] } else { 0 };
            let left_inset_bot = if dy_bot < insets.len() && (corner_mask & CORNER_BOTTOM_LEFT != 0) { insets[dy_bot] } else { 0 };
            let right_inset_bot = if dy_bot < insets.len() && (corner_mask & CORNER_BOTTOM_RIGHT != 0) { insets[dy_bot] } else { 0 };
            let left_inset = left_inset_top.max(left_inset_bot);
            let right_inset = right_inset_top.max(right_inset_bot);
            let sx = (x0 + left_inset).max(0);
            let ex = (x1 - right_inset).min(DISPLAY_WIDTH as i16);
            for x in sx..ex {
                fb[y as usize * DISPLAY_WIDTH + x as usize] = color;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_fill_circle(ctx: *mut PblGContext, center: GPoint, radius: u16) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).fill_color } };
    let r = radius as i16;
    if let Some(fb) = fb() {
        for y in (center.y - r).max(0)..(center.y + r + 1).min(DISPLAY_HEIGHT as i16) {
            for x in (center.x - r).max(0)..(center.x + r + 1).min(DISPLAY_WIDTH as i16) {
                let dx = x - center.x;
                let dy = y - center.y;
                if dx * dx + dy * dy <= r * r {
                    fb[y as usize * DISPLAY_WIDTH + x as usize] = color;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_circle(ctx: *mut PblGContext, center: GPoint, radius: u16) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if let Some(fb) = fb() {
        // Simple midpoint circle
        let mut x = radius as i16;
        let mut y: i16 = 0;
        let mut err: i16 = 1 - x;
        while x >= y {
            set_pixel(fb, center.x + x, center.y + y, color);
            set_pixel(fb, center.x - x, center.y + y, color);
            set_pixel(fb, center.x + x, center.y - y, color);
            set_pixel(fb, center.x - x, center.y - y, color);
            set_pixel(fb, center.x + y, center.y + x, color);
            set_pixel(fb, center.x - y, center.y + x, color);
            set_pixel(fb, center.x + y, center.y - x, color);
            set_pixel(fb, center.x - y, center.y - x, color);
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_line(ctx: *mut PblGContext, p0: GPoint, p1: GPoint) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if let Some(fb) = fb() {
        // Bresenham
        let dx = (p1.x - p0.x).abs();
        let dy = -(p1.y - p0.y).abs();
        let sx: i16 = if p0.x < p1.x { 1 } else { -1 };
        let sy: i16 = if p0.y < p1.y { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = p0.x;
        let mut y = p0.y;
        loop {
            set_pixel(fb, x, y, color);
            if x == p1.x && y == p1.y { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; x += sx; }
            if e2 <= dx { err += dx; y += sy; }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_text(
    ctx: *mut PblGContext,
    text: *const std::ffi::c_char,
    font_ptr: *const u8,
    box_: GRect,
    _overflow_mode: u8,
    alignment: u8,
    _text_attributes: *const u8,
) {
    if text.is_null() { return; }
    let text_str = unsafe { std::ffi::CStr::from_ptr(text) };
    let text_str = text_str.to_str().unwrap_or("");
    if text_str.is_empty() { return; }

    let color = if !ctx.is_null() {
        unsafe { (*ctx).text_color }
    } else {
        gcolor::colors::WHITE
    };

    if let Some(fb) = fb() {
        font::draw_text(fb, text_str, font_ptr, box_, alignment, color);
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_bitmap_in_rect(
    _ctx: *mut PblGContext,
    bitmap: *const PblGBitmap,
    rect: GRect,
) {
    if bitmap.is_null() {
        return;
    }
    let fb = match get_fb() {
        Some(fb) => fb,
        None => return,
    };
    unsafe {
        let bmp = &*bitmap;
        if bmp.data.is_null() {
            return;
        }
        let bw = bmp.bounds.w as i32;
        let bh = bmp.bounds.h as i32;
        if bw <= 0 || bh <= 0 {
            return;
        }
        let dw = DISPLAY_WIDTH as i32;
        let dh = DISPLAY_HEIGHT as i32;
        let row_bytes = bmp.row_size_bytes as i32;
        let format = bmp.info_flags; // clean format value (0-5)

        for sy in 0..bh.min(rect.h as i32) {
            let dy = rect.y as i32 + sy;
            if dy < 0 || dy >= dh {
                continue;
            }
            for sx in 0..bw.min(rect.w as i32) {
                let dx = rect.x as i32 + sx;
                if dx < 0 || dx >= dw {
                    continue;
                }
                let color = match format {
                    1 => { // GBitmapFormat8Bit
                        let off = sy * row_bytes + sx;
                        *bmp.data.add(off as usize)
                    }
                    0 => { // GBitmapFormat1Bit
                        let byte_off = sy * row_bytes + sx / 8;
                        let bit = (*bmp.data.add(byte_off as usize) >> (sx % 8)) & 1;
                        if bit != 0 { 0xFF } else { 0xC0 } // white : black
                    }
                    2 | 3 | 4 => { // Palette formats (1/2/4-bit)
                        let bpp = match format { 2 => 1, 3 => 2, 4 => 4, _ => 1 };
                        let pixels_per_byte = 8 / bpp;
                        let byte_off = sy * row_bytes + sx / pixels_per_byte as i32;
                        let bit_off = (sx as u32 % pixels_per_byte) * bpp as u32;
                        let mask = (1u8 << bpp) - 1;
                        let palette_idx = (*bmp.data.add(byte_off as usize) >> bit_off) & mask;
                        if !bmp.palette.is_null() {
                            // Use custom palette set via gbitmap_set_palette
                            *bmp.palette.add(palette_idx as usize)
                        } else {
                            // Palette is stored after pixel data: row_size_bytes * height bytes
                            let palette_offset = row_bytes as usize * bh as usize;
                            let palette_entry_off = palette_offset + palette_idx as usize;
                            *bmp.data.add(palette_entry_off)
                        }
                    }
                    _ => continue, // 5=8BitCircular or unknown
                };
                // Skip fully transparent pixels (alpha bits = 0b00)
                if color & 0xC0 == 0 {
                    continue;
                }
                fb[(dy * dw + dx) as usize] = color;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// GRect helpers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_grect_center_point(rect: *const GRect) -> GPoint {
    if rect.is_null() {
        return GPoint { x: 0, y: 0 };
    }
    let r = unsafe { &*rect };
    GPoint {
        x: r.x + r.w / 2,
        y: r.y + r.h / 2,
    }
}

// ---------------------------------------------------------------------------
// Tick timer
// ---------------------------------------------------------------------------

/// TimeUnits bitmask
const SECOND_UNIT: u32 = 1;
const MINUTE_UNIT: u32 = 2;

#[no_mangle]
pub extern "C" fn pbl_tick_timer_service_subscribe(tick_units: u32, handler: TickHandler) {
    println!("[pebble] tick_timer_service_subscribe(units=0x{:x})", tick_units);
    unsafe {
        TICK_HANDLER = Some(handler);
        TICK_UNITS = tick_units;
        TICK_STATE = crate::ticks::TickState::default();
    }
}

#[no_mangle]
pub extern "C" fn pbl_tick_timer_service_unsubscribe() {
    unsafe {
        TICK_HANDLER = None;
        TICK_UNITS = 0;
    }
}

// ---------------------------------------------------------------------------
// App timer
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_app_timer_register(timeout_ms: u32, callback: usize, data: usize) -> usize {
    unsafe {
        let id = NEXT_TIMER_ID;
        NEXT_TIMER_ID += 1;
        let cb: AppTimerCallback = std::mem::transmute(callback);
        APP_TIMERS.push(AppTimer {
            id,
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64),
            callback: cb,
            data,
        });
        id
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_timer_cancel(timer: usize) {
    unsafe {
        APP_TIMERS.retain(|t| t.id != timer);
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_timer_reschedule(timer: usize, new_timeout_ms: u32) -> bool {
    unsafe {
        if let Some(t) = APP_TIMERS.iter_mut().find(|t| t.id == timer) {
            t.deadline = std::time::Instant::now() + std::time::Duration::from_millis(new_timeout_ms as u64);
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Text layer
// ---------------------------------------------------------------------------

/// Internal update proc for text layers — renders the text layer's text.
extern "C" fn text_layer_update_proc(layer: *mut PblLayer, ctx: *mut PblGContext) {
    // The layer is the first field of PblTextLayer, so we can cast back
    let tl = layer as *mut PblTextLayer;
    if tl.is_null() { return; }
    unsafe {
        let text = (*tl).text;
        if text.is_null() { return; }
        let text_str = std::ffi::CStr::from_ptr(text);
        let text_str = match text_str.to_str() {
            Ok(s) => s,
            Err(_) => return,
        };
        if text_str.is_empty() { return; }

        // Draw background if not clear
        if (*tl).bg_color != gcolor::colors::CLEAR {
            if let Some(fb) = fb() {
                let f = (*tl).layer.frame;
                for py in f.y.max(0)..((f.y + f.h).min(DISPLAY_HEIGHT as i16)) {
                    for px in f.x.max(0)..((f.x + f.w).min(DISPLAY_WIDTH as i16)) {
                        fb[py as usize * DISPLAY_WIDTH + px as usize] = (*tl).bg_color;
                    }
                }
            }
        }

        // Draw text
        if let Some(fb) = fb() {
            font::draw_text(
                fb,
                text_str,
                (*tl).font,
                (*tl).layer.frame,
                (*tl).alignment,
                (*tl).color,
            );
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_create(frame: GRect) -> *mut PblTextLayer {
    let tl = crate::owned::new(PblTextLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
            frame,
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: Some(text_layer_update_proc),
        },
        text: std::ptr::null(),
        font: std::ptr::null(),
        color: gcolor::colors::WHITE,
        bg_color: gcolor::colors::CLEAR,
        alignment: 0,
        overflow_mode: 0,
    });

    // Register the internal update proc so it gets called in the tick loop
    unsafe { register_draw(&mut (*tl).layer, Some(text_layer_update_proc)); }

    tl
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_destroy(tl: *mut PblTextLayer) {
    dispose_layer(tl.cast());
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_get_layer(tl: *mut PblTextLayer) -> *mut PblLayer {
    if tl.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*tl).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_text(tl: *mut PblTextLayer, text: *const std::ffi::c_char) {
    if !tl.is_null() { unsafe { (*tl).text = text; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_font(tl: *mut PblTextLayer, font: *const u8) {
    if !tl.is_null() { unsafe { (*tl).font = font; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_text_alignment(tl: *mut PblTextLayer, alignment: u8) {
    if !tl.is_null() { unsafe { (*tl).alignment = alignment; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_background_color(tl: *mut PblTextLayer, color: GColor8) {
    if !tl.is_null() { unsafe { (*tl).bg_color = color.0; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_text_color(tl: *mut PblTextLayer, color: GColor8) {
    if !tl.is_null() { unsafe { (*tl).color = color.0; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_overflow_mode(tl: *mut PblTextLayer, mode: u8) {
    if !tl.is_null() { unsafe { (*tl).overflow_mode = mode; } }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_set_size(tl: *mut PblTextLayer, max_size: GSize) {
    if !tl.is_null() {
        unsafe {
            (*tl).layer.frame.w = max_size.w;
            (*tl).layer.frame.h = max_size.h;
            (*tl).layer.bounds.w = max_size.w;
            (*tl).layer.bounds.h = max_size.h;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_get_content_size(tl: *mut PblTextLayer) -> GSize {
    if tl.is_null() { return GSize { w: 0, h: 0 }; }
    unsafe {
        let text = (*tl).text;
        if text.is_null() { return GSize { w: 0, h: 0 }; }
        let text_str = match std::ffi::CStr::from_ptr(text).to_str() {
            Ok(s) => s,
            Err(_) => return GSize { w: 0, h: 0 },
        };
        let (w, h) = font::measure_text((*tl).font, text_str);
        GSize { w, h }
    }
}

#[no_mangle]
pub extern "C" fn pbl_text_layer_get_text(tl: *mut PblTextLayer) -> *const std::ffi::c_char {
    if tl.is_null() { return std::ptr::null(); }
    unsafe { (*tl).text }
}

// ---------------------------------------------------------------------------
// Bitmap layer
// ---------------------------------------------------------------------------

/// Internal update proc for bitmap layers — draws the bitmap into the layer frame.
extern "C" fn bitmap_layer_update_proc(layer: *mut PblLayer, ctx: *mut PblGContext) {
    let bl = layer as *mut PblBitmapLayer;
    if bl.is_null() { return; }
    unsafe {
        let bitmap = (*bl).bitmap;
        if bitmap.is_null() { return; }
        let frame = (*bl).layer.frame;
        pbl_graphics_draw_bitmap_in_rect(ctx, bitmap as *const PblGBitmap, frame);
    }
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_create(frame: GRect) -> *mut PblBitmapLayer {
    let bl = crate::owned::new(PblBitmapLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
            frame,
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: Some(bitmap_layer_update_proc),
        },
        bitmap: std::ptr::null_mut(),
        compositing_mode: 0,
    });

    // Register so it gets called in the tick loop
    unsafe { register_draw(&mut (*bl).layer, Some(bitmap_layer_update_proc)); }

    bl
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_destroy(bl: *mut PblBitmapLayer) {
    dispose_layer(bl.cast());
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_get_layer(bl: *mut PblBitmapLayer) -> *mut PblLayer {
    if bl.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*bl).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_set_bitmap(bl: *mut PblBitmapLayer, bitmap: *mut u8) {
    if !bl.is_null() { unsafe { (*bl).bitmap = bitmap; } }
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_set_compositing_mode(bl: *mut PblBitmapLayer, mode: u8) {
    if !bl.is_null() { unsafe { (*bl).compositing_mode = mode; } }
}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_set_background_color(_bl: *mut PblBitmapLayer, _color: GColor8) {
    // No-op — our bitmap layer doesn't have a separate background
}

// ---------------------------------------------------------------------------
// Scroll layer
// ---------------------------------------------------------------------------

/// ScrollLayer — wraps a PblLayer with scrollable content state
#[repr(C)]
pub struct PblScrollLayer {
    pub layer: PblLayer,
    pub content_size: GSize,
    pub content_offset: GPoint,
    pub context: *mut u8,
    pub shadow_hidden: bool,
    pub paging: bool,
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_create(frame: GRect) -> *mut PblScrollLayer {
    let sl = crate::owned::new(PblScrollLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
            frame,
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: None,
        },
        content_size: GSize { w: frame.w, h: frame.h },
        content_offset: GPoint { x: 0, y: 0 },
        context: std::ptr::null_mut(),
        shadow_hidden: false,
        paging: false,
    });
    sl
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_destroy(sl: *mut PblScrollLayer) {
    dispose_layer(sl.cast());
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_layer(sl: *mut PblScrollLayer) -> *mut PblLayer {
    if sl.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*sl).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_content_size(sl: *mut PblScrollLayer, size: GSize) {
    if !sl.is_null() { unsafe { (*sl).content_size = size; } }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_content_size(sl: *mut PblScrollLayer) -> GSize {
    if sl.is_null() { return GSize { w: 0, h: 0 }; }
    unsafe { (*sl).content_size }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_content_offset(sl: *mut PblScrollLayer, offset: GPoint, _animated: bool) {
    if !sl.is_null() { unsafe { (*sl).content_offset = offset; } }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_content_offset(sl: *mut PblScrollLayer) -> GPoint {
    if sl.is_null() { return GPoint { x: 0, y: 0 }; }
    unsafe { (*sl).content_offset }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_callbacks(
    _sl: *mut PblScrollLayer, _callbacks: *const u8,
) {
    // No-op — we don't dispatch scroll callbacks
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_click_config_onto_window(
    _sl: *mut PblScrollLayer, _window: *mut PblWindow,
) {
    // No-op — click handling not emulated
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_add_child(sl: *mut PblScrollLayer, child: *mut PblLayer) {
    // Wire the child's parent to our layer (minimal layer tree support)
    if !sl.is_null() && !child.is_null() {
        unsafe {
            (*child).parent = &mut (*sl).layer as *mut PblLayer;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_context(sl: *mut PblScrollLayer, context: *mut u8) {
    if !sl.is_null() { unsafe { (*sl).context = context; } }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_frame(sl: *mut PblScrollLayer, frame: GRect) {
    if !sl.is_null() {
        unsafe {
            (*sl).layer.frame = frame;
            (*sl).layer.bounds = GRect { x: 0, y: 0, w: frame.w, h: frame.h };
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_shadow_hidden(sl: *mut PblScrollLayer) -> bool {
    if sl.is_null() { return false; }
    unsafe { (*sl).shadow_hidden }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_shadow_hidden(sl: *mut PblScrollLayer, hidden: bool) {
    if !sl.is_null() { unsafe { (*sl).shadow_hidden = hidden; } }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_scroll_up_click_handler(
    _recognizer: *mut u8, _context: *mut u8,
) {
    // No-op — click handling not emulated
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_scroll_down_click_handler(
    _recognizer: *mut u8, _context: *mut u8,
) {
    // No-op — click handling not emulated
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_paging(sl: *mut PblScrollLayer) -> bool {
    if sl.is_null() { return false; }
    unsafe { (*sl).paging }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_set_paging(sl: *mut PblScrollLayer, paging: bool) {
    if !sl.is_null() { unsafe { (*sl).paging = paging; } }
}

#[no_mangle]
pub extern "C" fn pbl_scroll_layer_get_content_indicator(_sl: *mut PblScrollLayer) -> *mut u8 {
    std::ptr::null_mut() // no content indicator support
}

// ---------------------------------------------------------------------------
// Action bar layer
// ---------------------------------------------------------------------------

const NUM_ACTION_BAR_ITEMS: usize = 3;
const ACTION_BAR_WIDTH: i16 = 30;

#[repr(C)]
pub struct PblActionBarLayer {
    pub layer: PblLayer,
    pub icons: [*const u8; NUM_ACTION_BAR_ITEMS], // GBitmap pointers for UP/SELECT/DOWN
    pub window: *mut PblWindow,
    pub context: *mut u8,
    pub click_config_provider: Option<extern "C" fn(*mut u8)>,
    pub background_color: u8, // GColor8
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_create() -> *mut PblActionBarLayer {
    crate::owned::new(PblActionBarLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: ACTION_BAR_WIDTH, h: 168 },
            frame: GRect { x: 144 - ACTION_BAR_WIDTH, y: 0, w: ACTION_BAR_WIDTH, h: 168 },
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: None,
        },
        icons: [std::ptr::null(); NUM_ACTION_BAR_ITEMS],
        window: std::ptr::null_mut(),
        context: std::ptr::null_mut(),
        click_config_provider: None,
        background_color: 0xFF, // white
    })
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_destroy(ab: *mut PblActionBarLayer) {
    dispose_layer(ab.cast());
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_get_layer(ab: *mut PblActionBarLayer) -> *mut PblLayer {
    if ab.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*ab).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_add_to_window(ab: *mut PblActionBarLayer, window: *mut PblWindow) {
    if !ab.is_null() { unsafe { (*ab).window = window; } }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_remove_from_window(ab: *mut PblActionBarLayer) {
    if !ab.is_null() { unsafe { (*ab).window = std::ptr::null_mut(); } }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_background_color(ab: *mut PblActionBarLayer, color: u8) {
    if !ab.is_null() { unsafe { (*ab).background_color = color; } }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_click_config_provider(
    ab: *mut PblActionBarLayer, provider: Option<extern "C" fn(*mut u8)>,
) {
    if !ab.is_null() { unsafe { (*ab).click_config_provider = provider; } }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_context(ab: *mut PblActionBarLayer, context: *mut u8) {
    if !ab.is_null() { unsafe { (*ab).context = context; } }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_icon(
    ab: *mut PblActionBarLayer, button_id: u8, icon: *const u8,
) {
    if ab.is_null() { return; }
    // ButtonId: BACK=0, UP=1, SELECT=2, DOWN=3 → icons array uses 0=UP,1=SELECT,2=DOWN
    let idx = match button_id {
        1 => 0, // UP
        2 => 1, // SELECT
        3 => 2, // DOWN
        _ => return,
    };
    unsafe { (*ab).icons[idx] = icon; }
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_icon_animated(
    ab: *mut PblActionBarLayer, button_id: u8, icon: *const u8, _animated: bool,
) {
    pbl_action_bar_layer_set_icon(ab, button_id, icon);
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_clear_icon(ab: *mut PblActionBarLayer, button_id: u8) {
    pbl_action_bar_layer_set_icon(ab, button_id, std::ptr::null());
}

#[no_mangle]
pub extern "C" fn pbl_action_bar_layer_set_icon_press_animation(
    _ab: *mut PblActionBarLayer, _button_id: u8, _animation: u8,
) {
    // No-op — press animations not supported
}

// ---------------------------------------------------------------------------
// Menu layer
// ---------------------------------------------------------------------------

/// MenuIndex: (section, row)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct MenuIndex {
    pub section: u16,
    pub row: u16,
}

/// MenuLayerCallbacks — 13 function pointers stored as raw addresses.
/// In the emulator path these are ARM addresses called via call_callback;
/// in the native path they are host function pointers.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct MenuLayerCallbacks {
    pub get_num_sections: usize,
    pub get_num_rows: usize,
    pub get_cell_height: usize,
    pub get_header_height: usize,
    pub draw_row: usize,
    pub draw_header: usize,
    pub select_click: usize,
    pub select_long_click: usize,
    pub selection_changed: usize,
    pub get_separator_height: usize,
    pub draw_separator: usize,
    pub selection_will_change: usize,
    pub draw_background: usize,
}

#[repr(C)]
pub struct PblMenuLayer {
    pub scroll_layer: PblScrollLayer,
    pub callbacks: MenuLayerCallbacks,
    pub callback_context: *mut u8,
    pub selected: MenuIndex,
    pub num_sections: u16,
    pub num_rows: Vec<u16>, // per-section row counts (host-side convenience)
    pub normal_bg: u8,
    pub normal_fg: u8,
    pub highlight_bg: u8,
    pub highlight_fg: u8,
    pub pad_bottom: bool,
    pub center_focused: bool,
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_create(frame: GRect) -> *mut PblMenuLayer {
    crate::owned::new(PblMenuLayer {
        scroll_layer: PblScrollLayer {
            layer: PblLayer {
                bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
                frame,
                flags: 1,
                next_sibling: std::ptr::null_mut(),
                parent: std::ptr::null_mut(),
                first_child: std::ptr::null_mut(),
                window: std::ptr::null_mut(),
                update_proc: None,
            },
            content_size: GSize { w: frame.w, h: frame.h },
            content_offset: GPoint { x: 0, y: 0 },
            context: std::ptr::null_mut(),
            shadow_hidden: false,
            paging: false,
        },
        callbacks: MenuLayerCallbacks {
            get_num_sections: 0, get_num_rows: 0, get_cell_height: 0,
            get_header_height: 0, draw_row: 0, draw_header: 0,
            select_click: 0, select_long_click: 0, selection_changed: 0,
            get_separator_height: 0, draw_separator: 0,
            selection_will_change: 0, draw_background: 0,
        },
        callback_context: std::ptr::null_mut(),
        selected: MenuIndex { section: 0, row: 0 },
        num_sections: 1,
        num_rows: vec![0],
        normal_bg: 0xFF, // white
        normal_fg: 0xC0, // black
        highlight_bg: 0xC0, // black
        highlight_fg: 0xFF, // white
        pad_bottom: false,
        center_focused: false,
    })
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_destroy(ml: *mut PblMenuLayer) {
    dispose_layer(ml.cast());
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_get_layer(ml: *mut PblMenuLayer) -> *mut PblLayer {
    if ml.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*ml).scroll_layer.layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_get_scroll_layer(ml: *mut PblMenuLayer) -> *mut PblScrollLayer {
    if ml.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*ml).scroll_layer as *mut PblScrollLayer }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_get_selected_index(ml: *mut PblMenuLayer) -> MenuIndex {
    if ml.is_null() { return MenuIndex { section: 0, row: 0 }; }
    unsafe { (*ml).selected }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_selected_index(
    ml: *mut PblMenuLayer, index: MenuIndex, _scroll_align: u8, _animated: bool,
) {
    if !ml.is_null() { unsafe { (*ml).selected = index; } }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_selected_next(
    ml: *mut PblMenuLayer, up: bool, _scroll_align: u8, _animated: bool,
) {
    if ml.is_null() { return; }
    unsafe {
        if up {
            if (*ml).selected.row > 0 {
                (*ml).selected.row -= 1;
            }
        } else {
            (*ml).selected.row += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_reload_data(_ml: *mut PblMenuLayer) {
    // Triggers re-query of callbacks — in our stub this is a no-op
    // Real rendering happens in the emulator event loop
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_callbacks(
    ml: *mut PblMenuLayer, context: *mut u8, callbacks: *const MenuLayerCallbacks,
) {
    if ml.is_null() || callbacks.is_null() { return; }
    unsafe {
        (*ml).callbacks = *callbacks;
        (*ml).callback_context = context;
    }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_click_config_onto_window(
    _ml: *mut PblMenuLayer, _window: *mut PblWindow,
) {
    // No-op — click handling done via event loop
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_highlight_colors(
    ml: *mut PblMenuLayer, background: u8, foreground: u8,
) {
    if !ml.is_null() {
        unsafe {
            (*ml).highlight_bg = background;
            (*ml).highlight_fg = foreground;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_normal_colors(
    ml: *mut PblMenuLayer, background: u8, foreground: u8,
) {
    if !ml.is_null() {
        unsafe {
            (*ml).normal_bg = background;
            (*ml).normal_fg = foreground;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_pad_bottom_enable(ml: *mut PblMenuLayer, enable: bool) {
    if !ml.is_null() { unsafe { (*ml).pad_bottom = enable; } }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_get_center_focused(ml: *mut PblMenuLayer) -> bool {
    if ml.is_null() { return false; }
    unsafe { (*ml).center_focused }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_set_center_focused(ml: *mut PblMenuLayer, center_focused: bool) {
    if !ml.is_null() { unsafe { (*ml).center_focused = center_focused; } }
}

#[no_mangle]
pub extern "C" fn pbl_menu_layer_is_index_selected(ml: *mut PblMenuLayer, index: *const MenuIndex) -> bool {
    if ml.is_null() || index.is_null() { return false; }
    unsafe {
        (*ml).selected.section == (*index).section && (*ml).selected.row == (*index).row
    }
}

// ---------------------------------------------------------------------------
// Simple menu layer
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct PblSimpleMenuLayer {
    pub menu_layer: PblMenuLayer,
    // Simple menu stores section/item data on the host side as raw pointers
    // into emulated memory — we don't interpret them
    pub sections_ptr: *const u8,
    pub num_sections: i32,
    pub callback_context: *mut u8,
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_create(
    frame: GRect, _window: *mut PblWindow, sections: *const u8,
    num_sections: i32, callback_context: *mut u8,
) -> *mut PblSimpleMenuLayer {
    crate::owned::new(PblSimpleMenuLayer {
        menu_layer: PblMenuLayer {
            scroll_layer: PblScrollLayer {
                layer: PblLayer {
                    bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
                    frame,
                    flags: 1,
                    next_sibling: std::ptr::null_mut(),
                    parent: std::ptr::null_mut(),
                    first_child: std::ptr::null_mut(),
                    window: std::ptr::null_mut(),
                    update_proc: None,
                },
                content_size: GSize { w: frame.w, h: frame.h },
                content_offset: GPoint { x: 0, y: 0 },
                context: std::ptr::null_mut(),
                shadow_hidden: false,
                paging: false,
            },
            callbacks: MenuLayerCallbacks {
                get_num_sections: 0, get_num_rows: 0, get_cell_height: 0,
                get_header_height: 0, draw_row: 0, draw_header: 0,
                select_click: 0, select_long_click: 0, selection_changed: 0,
                get_separator_height: 0, draw_separator: 0,
                selection_will_change: 0, draw_background: 0,
            },
            callback_context: std::ptr::null_mut(),
            selected: MenuIndex { section: 0, row: 0 },
            num_sections: if num_sections > 0 { num_sections as u16 } else { 1 },
            num_rows: vec![0],
            normal_bg: 0xFF,
            normal_fg: 0xC0,
            highlight_bg: 0xC0,
            highlight_fg: 0xFF,
            pad_bottom: false,
            center_focused: false,
        },
        sections_ptr: sections,
        num_sections,
        callback_context,
    })
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_destroy(sml: *mut PblSimpleMenuLayer) {
    dispose_layer(sml.cast());
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_get_layer(sml: *mut PblSimpleMenuLayer) -> *mut PblLayer {
    if sml.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*sml).menu_layer.scroll_layer.layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_get_selected_index(sml: *mut PblSimpleMenuLayer) -> i32 {
    if sml.is_null() { return 0; }
    unsafe { (*sml).menu_layer.selected.row as i32 }
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_set_selected_index(
    sml: *mut PblSimpleMenuLayer, index: i32, _animated: bool,
) {
    if !sml.is_null() {
        unsafe { (*sml).menu_layer.selected.row = index.max(0) as u16; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_simple_menu_layer_get_menu_layer(sml: *mut PblSimpleMenuLayer) -> *mut PblMenuLayer {
    if sml.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*sml).menu_layer as *mut PblMenuLayer }
}

// ---------------------------------------------------------------------------
// Status bar layer
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct PblStatusBarLayer {
    pub layer: PblLayer,
    pub bg_color: u8,
    pub fg_color: u8,
    pub separator_mode: u8,
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_create() -> *mut PblStatusBarLayer {
    crate::owned::new(PblStatusBarLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: 180, h: 16 },
            frame: GRect { x: 0, y: 0, w: 180, h: 16 },
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: None,
        },
        bg_color: 0xFF, // white
        fg_color: 0xC0, // black
        separator_mode: 0,
    })
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_destroy(sb: *mut PblStatusBarLayer) {
    dispose_layer(sb.cast());
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_get_layer(sb: *mut PblStatusBarLayer) -> *mut PblLayer {
    if sb.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*sb).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_get_background_color(sb: *mut PblStatusBarLayer) -> u8 {
    if sb.is_null() { return 0xFF; }
    unsafe { (*sb).bg_color }
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_get_foreground_color(sb: *mut PblStatusBarLayer) -> u8 {
    if sb.is_null() { return 0xC0; }
    unsafe { (*sb).fg_color }
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_set_colors(sb: *mut PblStatusBarLayer, bg: u8, fg: u8) {
    if !sb.is_null() {
        unsafe {
            (*sb).bg_color = bg;
            (*sb).fg_color = fg;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_status_bar_layer_set_separator_mode(sb: *mut PblStatusBarLayer, mode: u8) {
    if !sb.is_null() { unsafe { (*sb).separator_mode = mode; } }
}

// ---------------------------------------------------------------------------
// Number window
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct PblNumberWindow {
    pub window: PblWindow,
    pub label: *const std::ffi::c_char,
    pub value: i32,
    pub max_val: i32,
    pub min_val: i32,
    pub step_size: i32,
    pub callback_context: *mut u8,
}

#[no_mangle]
pub extern "C" fn pbl_number_window_create(
    label: *const std::ffi::c_char, _callbacks: *const u8, callback_context: *mut u8,
) -> *mut PblNumberWindow {
    crate::owned::new(PblNumberWindow {
        window: PblWindow {
            root_layer: std::ptr::null_mut(),
        },
        label,
        value: 0,
        max_val: 100,
        min_val: 0,
        step_size: 1,
        callback_context,
    })
}

#[no_mangle]
pub extern "C" fn pbl_number_window_destroy(nw: *mut PblNumberWindow) {
    pbl_window_destroy(nw.cast());
}

#[no_mangle]
pub extern "C" fn pbl_number_window_get_value(nw: *mut PblNumberWindow) -> i32 {
    if nw.is_null() { return 0; }
    unsafe { (*nw).value }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_set_label(nw: *mut PblNumberWindow, label: *const std::ffi::c_char) {
    if !nw.is_null() { unsafe { (*nw).label = label; } }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_set_max(nw: *mut PblNumberWindow, max: i32) {
    if !nw.is_null() { unsafe { (*nw).max_val = max; } }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_set_min(nw: *mut PblNumberWindow, min: i32) {
    if !nw.is_null() { unsafe { (*nw).min_val = min; } }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_set_step_size(nw: *mut PblNumberWindow, step: i32) {
    if !nw.is_null() { unsafe { (*nw).step_size = step; } }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_set_value(nw: *mut PblNumberWindow, value: i32) {
    if !nw.is_null() { unsafe { (*nw).value = value; } }
}

#[no_mangle]
pub extern "C" fn pbl_number_window_get_window(nw: *mut PblNumberWindow) -> *mut PblWindow {
    if nw.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*nw).window as *mut PblWindow }
}

// ---------------------------------------------------------------------------
// GPath
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gpath_create(info: *const GPathInfo) -> *mut GPath {
    if info.is_null() { return std::ptr::null_mut(); }
    let i = unsafe { &*info };
    crate::owned::new(GPath {
        num_points: i.num_points,
        points: i.points,
        rotation: 0,
        offset: GPoint { x: 0, y: 0 },
    })
}

#[no_mangle]
pub extern "C" fn pbl_gpath_destroy(path: *mut GPath) {
    if !path.is_null() {
        unsafe { crate::owned::release(path); }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gpath_rotate_to(path: *mut GPath, angle: i32) {
    if !path.is_null() { unsafe { (*path).rotation = angle; } }
}

#[no_mangle]
pub extern "C" fn pbl_gpath_move_to(path: *mut GPath, point: GPoint) {
    eprintln!("[pebble:gfx] gpath_move_to({:?}, ({},{}))", path, point.x, point.y);
    if !path.is_null() { unsafe { (*path).offset = point; } }
}

/// Pebble trig: angles in "Pebble angle" units where TRIG_MAX_ANGLE = 0x10000 = full circle
const TRIG_MAX_ANGLE: f32 = 65536.0;

fn pbl_angle_to_rad(angle: i32) -> f32 {
    (angle as f32 / TRIG_MAX_ANGLE) * 2.0 * std::f32::consts::PI
}

fn rotate_point(p: GPoint, angle: i32, center: GPoint) -> GPoint {
    let rad = pbl_angle_to_rad(angle);
    let cos = rad.cos();
    let sin = rad.sin();
    let dx = p.x as f32 - center.x as f32;
    let dy = p.y as f32 - center.y as f32;
    GPoint {
        x: (center.x as f32 + dx * cos - dy * sin) as i16,
        y: (center.y as f32 + dx * sin + dy * cos) as i16,
    }
}

#[no_mangle]
pub extern "C" fn pbl_gpath_draw_filled(ctx: *mut PblGContext, path: *const GPath) {
    if path.is_null() { return; }
    let p = unsafe { &*path };
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).fill_color } };
    // Log first few points to see where the shape actually is
    let first_pts: Vec<String> = (0..p.num_points.min(4) as usize).map(|i| {
        let pt = unsafe { *p.points.add(i) };
        format!("({},{})", pt.x, pt.y)
    }).collect();
    eprintln!("[pebble:gfx] gpath_draw_filled pts={} rot={} off=({},{}) color=0x{:02x} first=[{}]",
        p.num_points, p.rotation, p.offset.x, p.offset.y, color, first_pts.join(", "));

    if p.num_points < 3 || p.points.is_null() { return; }

    // Get transformed points
    let points: Vec<GPoint> = (0..p.num_points as usize).map(|i| {
        let pt = unsafe { *p.points.add(i) };
        let mut transformed = GPoint { x: pt.x + p.offset.x, y: pt.y + p.offset.y };
        if p.rotation != 0 {
            transformed = rotate_point(pt, p.rotation, GPoint { x: 0, y: 0 });
            transformed.x += p.offset.x;
            transformed.y += p.offset.y;
        }
        transformed
    }).collect();

    // Scanline fill
    if let Some(fb) = fb() {
        let min_y = points.iter().map(|p| p.y).min().unwrap().max(0);
        let max_y = points.iter().map(|p| p.y).max().unwrap().min(DISPLAY_HEIGHT as i16 - 1);

        for y in min_y..=max_y {
            let mut nodes = Vec::new();
            let n = points.len();
            let mut j = n - 1;
            for i in 0..n {
                let yi = points[i].y as f32;
                let yj = points[j].y as f32;
                if (yi < y as f32 && yj >= y as f32) || (yj < y as f32 && yi >= y as f32) {
                    let xi = points[i].x as f32;
                    let xj = points[j].x as f32;
                    let x = xi + (y as f32 - yi) / (yj - yi) * (xj - xi);
                    nodes.push(x as i16);
                }
                j = i;
            }
            nodes.sort();
            for pair in nodes.chunks(2) {
                if pair.len() == 2 {
                    for x in pair[0].max(0)..=pair[1].min(DISPLAY_WIDTH as i16 - 1) {
                        fb[y as usize * DISPLAY_WIDTH + x as usize] = color;
                    }
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gpath_draw_outline(ctx: *mut PblGContext, path: *const GPath) {
    if path.is_null() { return; }
    let p = unsafe { &*path };
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };

    if p.num_points < 2 || p.points.is_null() { return; }

    let points: Vec<GPoint> = (0..p.num_points as usize).map(|i| {
        let pt = unsafe { *p.points.add(i) };
        let mut transformed = GPoint { x: pt.x + p.offset.x, y: pt.y + p.offset.y };
        if p.rotation != 0 {
            transformed = rotate_point(pt, p.rotation, GPoint { x: 0, y: 0 });
            transformed.x += p.offset.x;
            transformed.y += p.offset.y;
        }
        transformed
    }).collect();

    if let Some(fb) = fb() {
        for i in 0..points.len() {
            let j = (i + 1) % points.len();
            // Draw line segment
            let p0 = points[i];
            let p1 = points[j];
            let dx = (p1.x - p0.x).abs();
            let dy = -(p1.y - p0.y).abs();
            let sx: i16 = if p0.x < p1.x { 1 } else { -1 };
            let sy: i16 = if p0.y < p1.y { 1 } else { -1 };
            let mut err = dx + dy;
            let mut x = p0.x;
            let mut y = p0.y;
            loop {
                set_pixel(fb, x, y, color);
                if x == p1.x && y == p1.y { break; }
                let e2 = 2 * err;
                if e2 >= dy { err += dy; x += sx; }
                if e2 <= dx { err += dx; y += sy; }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Trig (sin_lookup, cos_lookup, atan2_lookup)
// ---------------------------------------------------------------------------

const TRIG_MAX_RATIO: i32 = 0xFFFF;

#[no_mangle]
pub extern "C" fn pbl_sin_lookup(angle: i32) -> i32 {
    let rad = (angle as f32 / TRIG_MAX_ANGLE) * 2.0 * std::f32::consts::PI;
    (rad.sin() * TRIG_MAX_RATIO as f32) as i32
}

#[no_mangle]
pub extern "C" fn pbl_cos_lookup(angle: i32) -> i32 {
    let rad = (angle as f32 / TRIG_MAX_ANGLE) * 2.0 * std::f32::consts::PI;
    (rad.cos() * TRIG_MAX_RATIO as f32) as i32
}

#[no_mangle]
pub extern "C" fn pbl_atan2_lookup(y: i16, x: i16) -> i32 {
    let rad = (y as f32).atan2(x as f32);
    ((rad / (2.0 * std::f32::consts::PI)) * TRIG_MAX_ANGLE) as i32
}

// ---------------------------------------------------------------------------
// Fonts — real .pfo system fonts
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_fonts_get_system_font(key: *const std::ffi::c_char) -> *const u8 {
    if key.is_null() {
        return font::font_for_key("");
    }
    let key_str = unsafe { std::ffi::CStr::from_ptr(key) };
    let key_str = key_str.to_str().unwrap_or("");
    font::font_for_key(key_str)
}

/// Load a custom font from a resource handle (ResHandle = resource ID).
#[no_mangle]
pub extern "C" fn pbl_fonts_load_custom_font(resource_handle: u32) -> *const u8 {
    font::load_custom_font(resource_handle)
}

/// Release custom font storage; active rendering snapshots retain their data.
#[no_mangle]
pub extern "C" fn pbl_fonts_unload_custom_font(font: *const u8) {
    crate::font::unload_custom_font(font);
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_with_data(data_ptr: *const u8) -> *mut PblGBitmap {
    if data_ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let row_size_bytes = u16::from_le_bytes([*data_ptr, *data_ptr.add(1)]);
        let info_flags = u16::from_le_bytes([*data_ptr.add(2), *data_ptr.add(3)]);
        let width = u16::from_le_bytes([*data_ptr.add(8), *data_ptr.add(9)]) as i16;
        let height = u16::from_le_bytes([*data_ptr.add(10), *data_ptr.add(11)]) as i16;
        let format = (info_flags >> 1) & 0x07;

        if format > 5 || width <= 0 || height <= 0 || row_size_bytes == 0 {
            eprintln!("[pebble:res] gbitmap_create_with_data — invalid: {}x{} row={} fmt={}",
                width, height, row_size_bytes, format);
            return std::ptr::null_mut();
        }

        // Point directly to the caller's data (no copy — Pebble semantics)
        let pixel_data = data_ptr.add(12) as *mut u8;

        crate::owned::new(PblGBitmap {
            data: pixel_data,
            row_size_bytes,
            info_flags: format,
            bounds: GRect { x: 0, y: 0, w: width, h: height },
            palette: std::ptr::null_mut(),
            free_palette_on_destroy: false,
            owns_data: false,
        })
    }
}

/// Load a GBitmap from the app's resource pack.
/// Supports both PNG resources (detected by magic bytes) and native Pebble BitmapData
/// (u16 row_size_bytes, u16 info_flags, u16[2] deprecated, u16 width, u16 height, u8[] data).
#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_with_resource(resource_id: u32) -> *mut PblGBitmap {
    let resource = match resource_get_data(resource_id) {
        Some(data) => data,
        None => {
            eprintln!("[pebble:res] gbitmap_create_with_resource({}) — resource not found", resource_id);
            return std::ptr::null_mut();
        }
    };
    unsafe {
        let res = &*resource;
        if res.len() < 12 {
            return std::ptr::null_mut();
        }

        // Detect PNG by magic bytes: \x89PNG
        if res.len() >= 8 && &res[0..4] == b"\x89PNG" {
            return gbitmap_from_png(resource_id, res);
        }

        // Parse PBI (BitmapData) header
        let row_size_bytes = u16::from_le_bytes([res[0], res[1]]);
        let info_flags = u16::from_le_bytes([res[2], res[3]]);
        // res[4..8] = deprecated
        let width = u16::from_le_bytes([res[8], res[9]]) as i16;
        let height = u16::from_le_bytes([res[10], res[11]]) as i16;

        // Extract format from info_flags bitfield:
        //   bit 0: is_bitmap_heap_allocated
        //   bits 1-3: GBitmapFormat (0=1Bit, 1=8Bit, 2=1BitPalette, 3=2BitPalette, 4=4BitPalette, 5=8BitCircular)
        //   bit 4: is_palette_heap_allocated
        let format = (info_flags >> 1) & 0x07;
        if format > 5 {
            let hex: Vec<String> = res.iter().take(16).map(|b| format!("{:02x}", b)).collect();
            eprintln!("[pebble:res] gbitmap_create_with_resource({}) — invalid PBI format {} (flags=0x{:04x}), skipping. raw: [{}]",
                resource_id, format, info_flags, hex.join(" "));
            return std::ptr::null_mut();
        }

        // Validate bitmap metadata consistency (like the real Pebble OS does)
        if width <= 0 || height <= 0 || row_size_bytes == 0 {
            eprintln!("[pebble:res] gbitmap_create_with_resource({}) — invalid PBI dimensions: {}x{} row={}, skipping",
                resource_id, width, height, row_size_bytes);
            return std::ptr::null_mut();
        }

        let pixel_data = &res[12..];
        let expected_pixel_bytes = row_size_bytes as usize * height as usize;
        if expected_pixel_bytes > pixel_data.len() {
            eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PBI data too small: need {} but have {} ({}x{} row={})",
                resource_id, expected_pixel_bytes, pixel_data.len(), width, height, row_size_bytes);
            return std::ptr::null_mut();
        }

        // Copy pixel data (+ optional palette) to heap
        let data_size = pixel_data.len();
        let data = libc::malloc(data_size) as *mut u8;
        if data.is_null() {
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(pixel_data.as_ptr(), data, data_size);

        // Store format as our internal enum value (not the raw bitfield)
        eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PBI {}x{} row={} fmt={} data={}B",
            resource_id, width, height, row_size_bytes, format, data_size);

        crate::owned::new(PblGBitmap {
            data,
            row_size_bytes,
            info_flags: format, // store clean format value
            bounds: GRect { x: 0, y: 0, w: width, h: height },
            palette: std::ptr::null_mut(),
            free_palette_on_destroy: false,
            owns_data: true,
        })
    }
}

/// Try to fix a truncated PNG by padding the IDAT data with zeros.
/// Some Pebble app resources have IDAT data that's a few bytes short.
fn fix_truncated_png(png_data: &[u8]) -> Option<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::{Read, Write};

    if png_data.len() < 8 || &png_data[0..4] != b"\x89PNG" {
        return None;
    }

    // Parse IHDR to get dimensions and compute expected decompressed size
    let mut pos = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut idat_chunks: Vec<(usize, usize)> = Vec::new(); // (offset_of_data, length)

    while pos + 12 <= png_data.len() {
        let chunk_len = u32::from_be_bytes([png_data[pos], png_data[pos+1], png_data[pos+2], png_data[pos+3]]) as usize;
        let chunk_type = &png_data[pos+4..pos+8];
        if pos + 12 + chunk_len > png_data.len() {
            break;
        }
        if chunk_type == b"IHDR" && chunk_len >= 13 {
            let d = pos + 8;
            width = u32::from_be_bytes([png_data[d], png_data[d+1], png_data[d+2], png_data[d+3]]);
            height = u32::from_be_bytes([png_data[d+4], png_data[d+5], png_data[d+6], png_data[d+7]]);
            bit_depth = png_data[d+8];
            color_type = png_data[d+9];
        }
        if chunk_type == b"IDAT" {
            idat_chunks.push((pos + 8, chunk_len));
        }
        pos += 12 + chunk_len;
    }

    if width == 0 || height == 0 || idat_chunks.is_empty() {
        return None;
    }

    // Compute bytes per pixel and expected row size
    let channels: u32 = match color_type {
        0 => 1, // grayscale
        2 => 3, // RGB
        3 => 1, // indexed (palette)
        4 => 2, // grayscale + alpha
        6 => 4, // RGBA
        _ => return None,
    };
    let bits_per_pixel = channels * bit_depth as u32;
    let row_bytes = (bits_per_pixel * width + 7) / 8;
    let expected = (1 + row_bytes) as usize * height as usize; // +1 for filter byte per row

    // Decompress all IDAT data
    let mut compressed = Vec::new();
    for (off, len) in &idat_chunks {
        compressed.extend_from_slice(&png_data[*off..*off + *len]);
    }
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    let _ = decoder.read_to_end(&mut decompressed); // may error on truncated data

    if decompressed.len() >= expected {
        return None; // not truncated, problem is something else
    }

    eprintln!("[pebble:res] PNG repair: decompressed {}/{} bytes, padding {} zeros",
        decompressed.len(), expected, expected - decompressed.len());

    // Pad with zeros
    decompressed.resize(expected, 0);

    // Recompress
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&decompressed).ok()?;
    let new_compressed = encoder.finish().ok()?;

    // Rebuild PNG: copy all chunks except IDAT, replace with single new IDAT
    let mut out = Vec::new();
    out.extend_from_slice(&png_data[0..8]); // PNG signature

    pos = 8;
    let mut idat_written = false;
    while pos + 12 <= png_data.len() {
        let chunk_len = u32::from_be_bytes([png_data[pos], png_data[pos+1], png_data[pos+2], png_data[pos+3]]) as usize;
        let chunk_type = &png_data[pos+4..pos+8];
        if pos + 12 + chunk_len > png_data.len() {
            break;
        }
        if chunk_type == b"IDAT" {
            if !idat_written {
                // Write our fixed IDAT
                let len_bytes = (new_compressed.len() as u32).to_be_bytes();
                out.extend_from_slice(&len_bytes);
                out.extend_from_slice(b"IDAT");
                out.extend_from_slice(&new_compressed);
                // CRC over type + data
                let crc = crc32_png(b"IDAT", &new_compressed);
                out.extend_from_slice(&crc.to_be_bytes());
                idat_written = true;
            }
            // Skip original IDAT chunks
        } else {
            // Copy chunk as-is
            out.extend_from_slice(&png_data[pos..pos + 12 + chunk_len]);
        }
        pos += 12 + chunk_len;
    }

    Some(out)
}

/// CRC32 for PNG chunks (over chunk type + chunk data)
fn crc32_png(chunk_type: &[u8], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFFFFFF
}

/// Parse PNG IHDR to get bit depth and color type.
fn png_ihdr_info(png_data: &[u8]) -> Option<(u32, u32, u8, u8)> {
    // width, height, bit_depth, color_type
    if png_data.len() < 29 || &png_data[0..4] != b"\x89PNG" {
        return None;
    }
    // IHDR is always the first chunk after signature
    let chunk_type = &png_data[12..16];
    if chunk_type != b"IHDR" { return None; }
    let d = 16;
    let w = u32::from_be_bytes([png_data[d], png_data[d+1], png_data[d+2], png_data[d+3]]);
    let h = u32::from_be_bytes([png_data[d+4], png_data[d+5], png_data[d+6], png_data[d+7]]);
    Some((w, h, png_data[d+8], png_data[d+9]))
}

/// Parse PNG PLTE chunk to get palette colors (RGB triplets).
fn png_plte_colors(png_data: &[u8]) -> Vec<[u8; 3]> {
    let mut colors = Vec::new();
    let mut pos = 8usize;
    while pos + 12 <= png_data.len() {
        let chunk_len = u32::from_be_bytes([png_data[pos], png_data[pos+1], png_data[pos+2], png_data[pos+3]]) as usize;
        let chunk_type = &png_data[pos+4..pos+8];
        if pos + 12 + chunk_len > png_data.len() { break; }
        if chunk_type == b"PLTE" {
            for i in (0..chunk_len).step_by(3) {
                if i + 2 < chunk_len {
                    let d = pos + 8 + i;
                    colors.push([png_data[d], png_data[d+1], png_data[d+2]]);
                }
            }
            break;
        }
        pos += 12 + chunk_len;
    }
    colors
}

/// Parse PNG tRNS chunk to get per-palette-entry alpha values.
fn png_trns_alphas(png_data: &[u8]) -> Vec<u8> {
    let mut alphas = Vec::new();
    let mut pos = 8usize;
    while pos + 12 <= png_data.len() {
        let chunk_len = u32::from_be_bytes([png_data[pos], png_data[pos+1], png_data[pos+2], png_data[pos+3]]) as usize;
        let chunk_type = &png_data[pos+4..pos+8];
        if pos + 12 + chunk_len > png_data.len() { break; }
        if chunk_type == b"tRNS" {
            for i in 0..chunk_len {
                alphas.push(png_data[pos + 8 + i]);
            }
            break;
        }
        pos += 12 + chunk_len;
    }
    alphas
}

/// Convert RGBA8888 to GColor8 (ARGB2222).
fn rgba_to_gcolor8(r: u8, g: u8, b: u8, a: u8) -> u8 {
    ((a >> 6) << 6) | ((r >> 6) << 4) | ((g >> 6) << 2) | (b >> 6)
}

/// Convert RGB888 to GColor8 (ARGB2222, fully opaque).
fn rgb_to_gcolor8(r: u8, g: u8, b: u8) -> u8 {
    rgba_to_gcolor8(r, g, b, 255)
}

/// Decode a PNG resource into native Pebble bitmap format.
/// For indexed PNGs with ≤16 colors, preserves the original bit depth (1/2/4-bit palette).
/// For everything else, converts to GColor8 (8-bit).
fn gbitmap_from_png(resource_id: u32, png_data: &[u8]) -> *mut PblGBitmap {
    let img = match image::load_from_memory(png_data) {
        Ok(img) => img,
        Err(e) => {
            eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PNG decode error: {}, attempting repair", resource_id, e);
            if let Some(fixed) = fix_truncated_png(png_data) {
                match image::load_from_memory(&fixed) {
                    Ok(img) => img,
                    Err(e2) => {
                        eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PNG repair also failed: {}", resource_id, e2);
                        return std::ptr::null_mut();
                    }
                }
            } else {
                return std::ptr::null_mut();
            }
        }
    };
    let (w, h) = img.dimensions();

    // Check if this is an indexed PNG that should use a paletted Pebble format
    if let Some((_, _, bit_depth, color_type)) = png_ihdr_info(png_data) {
        if color_type == 3 && (bit_depth == 1 || bit_depth == 2 || bit_depth == 4) {
            let plte = png_plte_colors(png_data);
            let trns = png_trns_alphas(png_data);
            if !plte.is_empty() {
                return gbitmap_from_png_paletted(resource_id, &img, w, h, bit_depth, &plte, &trns);
            }
        }
    }

    // Default path: convert to GColor8
    gbitmap_from_png_gcolor8(resource_id, &img, w, h)
}

/// Create a paletted GBitmap (1/2/4-bit) from a decoded PNG image.
fn gbitmap_from_png_paletted(
    resource_id: u32,
    img: &image::DynamicImage,
    w: u32, h: u32,
    bit_depth: u8,
    plte: &[[u8; 3]],
    trns: &[u8],
) -> *mut PblGBitmap {
    let rgba = img.to_rgba8();

    // Build GColor8 palette from PNG PLTE + tRNS
    let num_palette_entries: usize = match bit_depth {
        1 => 2,
        2 => 4,
        4 => 16,
        _ => return std::ptr::null_mut(),
    };
    let pebble_format: u16 = match bit_depth {
        1 => 2, // GBitmapFormat1BitPalette
        2 => 3, // GBitmapFormat2BitPalette
        4 => 4, // GBitmapFormat4BitPalette
        _ => return std::ptr::null_mut(),
    };

    // Build palette: convert RGBA→GColor8 (using tRNS for alpha)
    let mut gcolor_palette = vec![0u8; num_palette_entries];
    for (i, rgb) in plte.iter().enumerate() {
        if i >= num_palette_entries { break; }
        let alpha = trns.get(i).copied().unwrap_or(255);
        gcolor_palette[i] = rgba_to_gcolor8(rgb[0], rgb[1], rgb[2], alpha);
    }

    // For each pixel, find the closest palette index
    // (For indexed PNGs the image crate expands to RGBA, so we reverse-map)
    let row_size_bytes = ((w as usize * bit_depth as usize) + 7) / 8;
    // Pebble aligns rows to 4 bytes for palette formats
    let row_size_aligned = (row_size_bytes + 3) & !3;
    let data_size = row_size_aligned * h as usize;

    unsafe {
        let data = libc::malloc(data_size) as *mut u8;
        if data.is_null() { return std::ptr::null_mut(); }
        std::ptr::write_bytes(data, 0, data_size);

        for y in 0..h {
            let row_offset = y as usize * row_size_aligned;
            for x in 0..w {
                let pixel = rgba.get_pixel(x, y);
                let r = pixel[0]; let g = pixel[1]; let b = pixel[2];
                let gc = rgb_to_gcolor8(r, g, b);

                // Find closest palette entry
                let mut best_idx = 0u8;
                let mut best_dist = u32::MAX;
                for (i, &pc) in gcolor_palette.iter().enumerate() {
                    let dist = if pc == gc { 0 } else {
                        let dr = ((pc >> 4) & 3) as i32 - ((gc >> 4) & 3) as i32;
                        let dg = ((pc >> 2) & 3) as i32 - ((gc >> 2) & 3) as i32;
                        let db = (pc & 3) as i32 - (gc & 3) as i32;
                        (dr*dr + dg*dg + db*db) as u32
                    };
                    if dist < best_dist {
                        best_dist = dist;
                        best_idx = i as u8;
                    }
                }

                // Pack the index into the bit-packed row (LSB-first, Pebble convention)
                match bit_depth {
                    1 => {
                        let byte_idx = x as usize / 8;
                        let bit_pos = x as usize % 8; // LSB first
                        if best_idx != 0 {
                            *data.add(row_offset + byte_idx) |= 1 << bit_pos;
                        }
                    }
                    2 => {
                        let byte_idx = x as usize / 4;
                        let shift = (x as usize % 4) * 2; // LSB first
                        *data.add(row_offset + byte_idx) |= (best_idx & 0x3) << shift;
                    }
                    4 => {
                        let byte_idx = x as usize / 2;
                        let shift = (x as usize % 2) * 4; // LSB first
                        *data.add(row_offset + byte_idx) |= (best_idx & 0xF) << shift;
                    }
                    _ => {}
                }
            }
        }

        // Allocate palette on heap
        let palette = libc::malloc(num_palette_entries) as *mut u8;
        if !palette.is_null() {
            for (i, &c) in gcolor_palette.iter().enumerate() {
                *palette.add(i) = c;
            }
        }

        eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PNG {}x{} -> {}BitPalette row={} data={}B palette={}",
            resource_id, w, h, bit_depth, row_size_aligned, data_size, num_palette_entries);

        crate::owned::new(PblGBitmap {
            data,
            row_size_bytes: row_size_aligned as u16,
            info_flags: pebble_format,
            bounds: GRect { x: 0, y: 0, w: w as i16, h: h as i16 },
            palette,
            free_palette_on_destroy: true,
            owns_data: true,
        })
    }
}

/// Create a GColor8 (8-bit) GBitmap from a decoded PNG image.
fn gbitmap_from_png_gcolor8(resource_id: u32, img: &image::DynamicImage, w: u32, h: u32) -> *mut PblGBitmap {
    let rgba = img.to_rgba8();
    let row_size_bytes = w as u16;
    let data_size = (w * h) as usize;
    unsafe {
        let data = libc::malloc(data_size) as *mut u8;
        if data.is_null() {
            return std::ptr::null_mut();
        }
        for y in 0..h {
            for x in 0..w {
                let pixel = rgba.get_pixel(x, y);
                let r = pixel[0];
                let g = pixel[1];
                let b = pixel[2];
                let a = pixel[3];
                let gc = ((a >> 6) << 6) | ((r >> 6) << 4) | ((g >> 6) << 2) | (b >> 6);
                *data.add((y * w + x) as usize) = gc;
            }
        }
        eprintln!("[pebble:res] gbitmap_create_with_resource({}) — PNG {}x{} -> GColor8 {}B",
            resource_id, w, h, data_size);

        let palette = libc::malloc(64) as *mut u8;
        if !palette.is_null() {
            for i in 0u8..64 {
                *palette.add(i as usize) = 0xC0 | i;
            }
        }

        crate::owned::new(PblGBitmap {
            data,
            row_size_bytes,
            info_flags: 1, // GBitmapFormat8Bit
            bounds: GRect { x: 0, y: 0, w: w as i16, h: h as i16 },
            palette,
            free_palette_on_destroy: true,
            owns_data: true,
        })
    }
}

/// Create a sub-bitmap referencing a rectangular region of a parent bitmap.
/// The sub-bitmap gets its own copy of the pixel data (not a true reference,
/// but sufficient for Pebble apps that use sub-bitmaps as sprite sheet slices).
#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_as_sub_bitmap(
    base: *const PblGBitmap,
    sub_rect: GRect,
) -> *mut PblGBitmap {
    if base.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let parent = &*base;
        if parent.data.is_null() {
            return std::ptr::null_mut();
        }

        let left = (sub_rect.x as i32).max(parent.bounds.x as i32);
        let top = (sub_rect.y as i32).max(parent.bounds.y as i32);
        let right = (sub_rect.x as i32 + sub_rect.w.max(0) as i32)
            .min(parent.bounds.x as i32 + parent.bounds.w.max(0) as i32);
        let bottom = (sub_rect.y as i32 + sub_rect.h.max(0) as i32)
            .min(parent.bounds.y as i32 + parent.bounds.h.max(0) as i32);
        if right <= left || bottom <= top { return std::ptr::null_mut(); }
        let px = left - parent.bounds.x as i32;
        let py = top - parent.bounds.y as i32;
        let pw = right - left;
        let ph = bottom - top;
        let parent_row = parent.row_size_bytes as i32;
        let is_8bit = (parent.info_flags & 0x0F) == 1;
        let is_1bit = (parent.info_flags & 0x0F) == 0;

        if is_8bit {
            // 1 byte per pixel — copy the sub-rect rows
            let row_bytes = pw as u16;
            let data_size = (pw * ph) as usize;
            let data = libc::malloc(data_size) as *mut u8;
            if data.is_null() {
                return std::ptr::null_mut();
            }
            for y in 0..ph {
                let src_off = (py + y) * parent_row + px;
                let dst_off = y * pw;
                std::ptr::copy_nonoverlapping(
                    parent.data.add(src_off as usize),
                    data.add(dst_off as usize),
                    pw as usize,
                );
            }
            crate::owned::new(PblGBitmap {
                data,
                row_size_bytes: row_bytes,
                info_flags: parent.info_flags,
                bounds: GRect { x: 0, y: 0, w: pw as i16, h: ph as i16 },
                palette: std::ptr::null_mut(),
                free_palette_on_destroy: false,
                owns_data: true,
            })
        } else if is_1bit {
            // 1 bit per pixel — re-pack into new rows
            let row_bytes = ((pw + 7) / 8) as u16;
            let data_size = row_bytes as usize * ph as usize;
            let data = libc::calloc(1, data_size) as *mut u8;
            if data.is_null() {
                return std::ptr::null_mut();
            }
            for y in 0..ph {
                for x in 0..pw {
                    let src_byte = (py + y) * parent_row + (px + x) / 8;
                    let src_bit = ((px + x) % 8) as u8;
                    let bit = (*parent.data.add(src_byte as usize) >> src_bit) & 1;
                    let dst_byte = y * row_bytes as i32 + x / 8;
                    let dst_bit = (x % 8) as u8;
                    *data.add(dst_byte as usize) |= bit << dst_bit;
                }
            }
            crate::owned::new(PblGBitmap {
                data,
                row_size_bytes: row_bytes,
                info_flags: parent.info_flags,
                bounds: GRect { x: 0, y: 0, w: pw as i16, h: ph as i16 },
                palette: std::ptr::null_mut(),
                free_palette_on_destroy: false,
                owns_data: true,
            })
        } else {
            eprintln!("[pebble:res] gbitmap_create_as_sub_bitmap — unsupported format flags=0x{:04x}", parent.info_flags);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_destroy(bitmap: *mut PblGBitmap) {
    unsafe { if CAPTURED_BITMAP == Some(bitmap) { CAPTURED_BITMAP = None; } }
    crate::owned::release(bitmap);
}

impl Drop for PblGBitmap {
    fn drop(&mut self) {
        unsafe {
            if self.owns_data && !self.data.is_null() { libc::free(self.data.cast()); }
            if self.free_palette_on_destroy && !self.palette.is_null() && !crate::guest_heap::free_if_owned(self.palette) {
                libc::free(self.palette.cast());
            }
        }
    }
}

/// Get the palette pointer from a GBitmap.
/// Returns the palette field directly — Pebble always provides a palette,
/// even for 8-bit format (64-color identity palette).
#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_palette(bitmap: *mut PblGBitmap) -> *mut u8 {
    if bitmap.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let bmp = &*bitmap;
        if !bmp.palette.is_null() {
            return bmp.palette;
        }
        // For palettized formats without explicit palette, inline palette is after pixel data
        let format = bmp.info_flags;
        match format {
            2 | 3 | 4 => {
                let palette_offset = bmp.row_size_bytes as usize * bmp.bounds.h as usize;
                bmp.data.add(palette_offset)
            }
            _ => std::ptr::null_mut(),
        }
    }
}

/// Set a custom color palette on a GBitmap (for palettized 1/2/4-bit formats).
/// `palette` is an array of GColor8 values. `free_on_destroy` controls whether
/// the palette pointer is freed when the bitmap is destroyed.
#[no_mangle]
pub extern "C" fn pbl_gbitmap_set_palette(bitmap: *mut PblGBitmap, palette: *mut u8, free_on_destroy: bool) {
    if bitmap.is_null() {
        return;
    }
    unsafe {
        // Free the old custom palette if we own it
        if (*bitmap).palette != palette && (*bitmap).free_palette_on_destroy && !(*bitmap).palette.is_null() && !crate::guest_heap::free_if_owned((*bitmap).palette) {
            libc::free((*bitmap).palette as *mut libc::c_void);
        }
        (*bitmap).palette = palette;
        (*bitmap).free_palette_on_destroy = free_on_destroy;
    }
}

// ---------------------------------------------------------------------------
// Resource API (lower-level)
// ---------------------------------------------------------------------------

/// ResHandle is just the resource ID in Pebble's API
#[no_mangle]
pub extern "C" fn pbl_resource_get_handle(resource_id: u32) -> u32 {
    // In Pebble, this returns an opaque handle. We just use the resource_id directly.
    resource_id
}

#[no_mangle]
pub extern "C" fn pbl_resource_size(handle: u32) -> u32 {
    resource_get_data(handle).map_or(0, |data| data.len() as u32)
}

#[no_mangle]
pub extern "C" fn pbl_resource_load(handle: u32, buffer: *mut u8, max_length: u32) -> u32 {
    if buffer.is_null() { return 0; }
    match resource_get_data(handle) {
        Some(data) => unsafe {
            let to_copy = data.len().min(max_length as usize);
            std::ptr::copy_nonoverlapping(data.as_ptr(), buffer, to_copy);
            to_copy as u32
        },
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn pbl_resource_load_byte_range(handle: u32, start: u32, buffer: *mut u8, num_bytes: u32) -> u32 {
    if buffer.is_null() { return 0; }
    match resource_get_data(handle) {
        Some(data) => unsafe {
            let start = start as usize;
            if start >= data.len() { return 0; }
            let to_copy = (data.len() - start).min(num_bytes as usize);
            std::ptr::copy_nonoverlapping(data[start..].as_ptr(), buffer, to_copy);
            to_copy as u32
        },
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Clock / time
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_clock_to_timestamp(_weekday: u8, _hour: u8, _minute: u8) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn pbl_i18n_get_system_locale() -> *const u8 {
    static LOCALE: &[u8] = b"en_US\0";
    LOCALE.as_ptr()
}

/// setlocale — return a static locale string (C standard library function)
#[no_mangle]
pub extern "C" fn pbl_setlocale(_category: i32, _locale: *const u8) -> *const u8 {
    static C_LOCALE: &[u8] = b"C\0";
    C_LOCALE.as_ptr()
}

/// _localeconv_r — newlib reentrant locale conversion, return a static struct pointer
#[no_mangle]
pub extern "C" fn pbl_localeconv_r(_reent: usize) -> usize {
    // Return a pointer to a minimal static lconv struct
    // Apps typically only check if it's non-NULL
    static LCONV: [u8; 64] = [0; 64];
    LCONV.as_ptr() as usize
}

#[no_mangle]
pub extern "C" fn pbl_clock_get_timezone(buffer: *mut u8, size: usize) {
    if buffer.is_null() || size == 0 { return; }
    let zone = timezone_name();
    let count = zone.len().min(size - 1);
    unsafe { std::ptr::copy_nonoverlapping(zone.as_ptr(), buffer, count); *buffer.add(count) = 0; }
}

pub fn timezone_name() -> String {
    if let Ok(zone) = std::env::var("TZ") {
        if !zone.is_empty() { return zone.trim_start_matches(':').to_owned(); }
    }
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        if let Some((_, zone)) = link.to_string_lossy().split_once("zoneinfo/") { return zone.to_owned(); }
    }
    std::fs::read_to_string("/etc/timezone").ok().map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty()).unwrap_or_else(|| "UTC".into())
}

/// quiet_time_is_active — always return false (quiet time / DND not supported)
#[no_mangle]
pub extern "C" fn pbl_quiet_time_is_active() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Persist: durable per-application settings (UUID namespace)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_persist_exists(key: u32) -> bool { crate::persist::read(key).is_some() }
#[no_mangle]
pub extern "C" fn pbl_persist_get_size(key: u32) -> i32 {
    crate::persist::read(key).map_or(crate::persist::MISSING, |b| b.len() as i32)
}
#[no_mangle]
pub extern "C" fn pbl_persist_delete(key: u32) -> i32 { crate::persist::delete(key) }
#[no_mangle]
pub extern "C" fn pbl_persist_read_bool(key: u32) -> bool {
    crate::persist::read(key).is_some_and(|b| b.first().is_some_and(|v| *v != 0))
}
#[no_mangle]
pub extern "C" fn pbl_persist_read_int(key: u32) -> i32 {
    crate::persist::read(key).and_then(|b| b.get(..4)?.try_into().ok()).map(i32::from_le_bytes).unwrap_or(0)
}
#[no_mangle]
pub extern "C" fn pbl_persist_write_bool(key: u32, value: bool) -> i32 { crate::persist::write(key, &[value as u8]) }
#[no_mangle]
pub extern "C" fn pbl_persist_write_int(key: u32, value: i32) -> i32 { crate::persist::write(key, &value.to_le_bytes()) }
#[no_mangle]
pub extern "C" fn pbl_persist_read_data(key: u32, buffer: *mut u8, size: i32) -> i32 {
    if size < 0 || (size > 0 && buffer.is_null()) { return crate::persist::INVALID; }
    let Some(bytes) = crate::persist::read(key) else { return crate::persist::MISSING; };
    let count = bytes.len().min(size as usize);
    if count > 0 { unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, count); } }
    count as i32
}
#[no_mangle]
pub extern "C" fn pbl_persist_write_data(key: u32, data: *const u8, size: i32) -> i32 {
    if !(1..=256).contains(&size) { return crate::persist::RANGE; }
    if data.is_null() { return crate::persist::INVALID; }
    crate::persist::write(key, unsafe { std::slice::from_raw_parts(data, size as usize) })
}
#[no_mangle]
pub extern "C" fn pbl_persist_write_string(key: u32, string: *const u8) -> i32 {
    if string.is_null() { return crate::persist::INVALID; }
    for n in 0..256 {
        if unsafe { *string.add(n) } == 0 {
            return pbl_persist_write_data(key, string, (n + 1) as i32);
        }
    }
    crate::persist::RANGE
}
#[no_mangle]
pub extern "C" fn pbl_persist_read_string(key: u32, buffer: *mut u8, size: i32) -> i32 {
    if size <= 0 || buffer.is_null() { return crate::persist::INVALID; }
    let count = pbl_persist_read_data(key, buffer, size);
    if count > 0 { unsafe { *buffer.add(count as usize - 1) = 0; } }
    count
}
// Old SDK entry points put size before the pointer.
pub extern "C" fn pbl_persist_read_data_deprecated(key: u32, size: i32, buffer: *mut u8) -> i32 {
    pbl_persist_read_data(key, buffer, size)
}
pub extern "C" fn pbl_persist_write_data_deprecated(key: u32, size: i32, data: *const u8) -> i32 {
    pbl_persist_write_data(key, data, size)
}

// ---------------------------------------------------------------------------
// Light / vibes / hardware stubs
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_light_enable(_enable: bool) {}

#[no_mangle]
pub extern "C" fn pbl_light_enable_interaction() {}

#[no_mangle]
pub extern "C" fn pbl_vibes_cancel() {}

#[no_mangle]
pub extern "C" fn pbl_vibes_short_pulse() {}

#[no_mangle]
pub extern "C" fn pbl_vibes_long_pulse() {}

#[no_mangle]
pub extern "C" fn pbl_vibes_double_pulse() {}

#[no_mangle]
pub extern "C" fn pbl_vibes_enqueue_custom_pattern(_durations: *const u32, _num: u32) {}

// ---------------------------------------------------------------------------
// Native single-click handling. Long/multiple/raw gesture recognition is separate.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_window_set_click_config_provider(window: *mut PblWindow, provider: Option<ClickProvider>) {
    pbl_window_set_click_config_provider_with_context(window, provider, window.cast());
}

#[no_mangle]
pub extern "C" fn pbl_window_set_click_config_provider_with_context(window: *mut PblWindow, provider: Option<ClickProvider>, context: *mut u8) {
    if crate::owned::generation(window).is_none() { return; }
    unsafe {
        CLICK_CONFIGS.retain(|c| c.window != window as usize);
        CLICK_CONFIGS.push(ClickConfig { window: window as usize, provider, context: context as usize });
        if CURRENT_WINDOW == window { configure_native_clicks(); }
    }
}

#[no_mangle]
pub extern "C" fn pbl_window_single_click_subscribe(button: u8, handler: Option<ClickHandler>) {
    if button < 4 { unsafe { CLICK_HANDLERS[button as usize] = handler; } }
}

#[no_mangle]
pub extern "C" fn pbl_window_set_click_context(button: u8, context: *mut u8) {
    if button < 4 { unsafe { CLICK_CONTEXTS[button as usize] = context as usize; } }
}

#[no_mangle]
pub extern "C" fn pbl_window_multi_click_subscribe(_button: u8, _min: u8, _max: u8, _timeout: u16, _last_only: bool, _handler: Option<extern "C" fn(usize, *mut u8)>) {}

#[no_mangle]
pub extern "C" fn pbl_window_long_click_subscribe(_button: u8, _delay: u16, _down: Option<extern "C" fn(usize, *mut u8)>, _up: Option<extern "C" fn(usize, *mut u8)>) {}

#[no_mangle]
pub extern "C" fn pbl_window_raw_click_subscribe(_button: u8, _down: Option<extern "C" fn(usize, *mut u8)>, _up: Option<extern "C" fn(usize, *mut u8)>, _context: *mut u8) {}

#[no_mangle]
pub extern "C" fn pbl_window_get_click_config_provider(window: *mut PblWindow) -> usize {
    unsafe { CLICK_CONFIGS.iter().find(|c| c.window == window as usize).and_then(|c| c.provider).map_or(0, |p| p as usize) }
}

// ---------------------------------------------------------------------------
// Window navigation stubs
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_window_stack_pop(_animated: bool) -> *mut PblWindow {
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn pbl_window_stack_pop_all(_animated: bool) {}

#[no_mangle]
pub extern "C" fn pbl_window_stack_remove(_window: *mut PblWindow, _animated: bool) {}

#[no_mangle]
pub extern "C" fn pbl_window_get_fullscreen(_window: *mut PblWindow) -> bool { true }

#[no_mangle]
pub extern "C" fn pbl_window_set_fullscreen(_window: *mut PblWindow, _fullscreen: bool) {}

#[no_mangle]
pub extern "C" fn pbl_window_set_status_bar_icon(_window: *mut PblWindow, _icon: *const u8) {}

// ---------------------------------------------------------------------------
// GRect helpers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_grect_standardize(rect: *mut GRect) {
    if rect.is_null() { return; }
    let r = unsafe { &mut *rect };
    if r.w < 0 { r.x += r.w; r.w = -r.w; }
    if r.h < 0 { r.y += r.h; r.h = -r.h; }
}

#[no_mangle]
pub extern "C" fn pbl_grect_clip(rect: *mut GRect, clip: *const GRect) {
    if rect.is_null() || clip.is_null() { return; }
    let r = unsafe { &mut *rect };
    let c = unsafe { &*clip };
    let x1 = r.x.max(c.x);
    let y1 = r.y.max(c.y);
    let x2 = (r.x + r.w).min(c.x + c.w);
    let y2 = (r.y + r.h).min(c.y + c.h);
    r.x = x1;
    r.y = y1;
    r.w = (x2 - x1).max(0);
    r.h = (y2 - y1).max(0);
}

#[no_mangle]
pub extern "C" fn pbl_grect_contains_point(rect: *const GRect, point: *const GPoint) -> bool {
    if rect.is_null() || point.is_null() { return false; }
    let r = unsafe { &*rect };
    let p = unsafe { &*point };
    p.x >= r.x && p.x < r.x + r.w && p.y >= r.y && p.y < r.y + r.h
}

#[no_mangle]
pub extern "C" fn pbl_grect_crop(rect: GRect, inset: i16) -> GRect {
    GRect {
        x: rect.x + inset,
        y: rect.y + inset,
        w: (rect.w - 2 * inset).max(0),
        h: (rect.h - 2 * inset).max(0),
    }
}

#[no_mangle]
pub extern "C" fn pbl_grect_equal(a: *const GRect, b: *const GRect) -> bool {
    if a.is_null() || b.is_null() { return false; }
    let a = unsafe { &*a };
    let b = unsafe { &*b };
    a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h
}

#[no_mangle]
pub extern "C" fn pbl_grect_is_empty(rect: *const GRect) -> bool {
    if rect.is_null() { return true; }
    let r = unsafe { &*rect };
    r.w <= 0 || r.h <= 0
}

#[no_mangle]
pub extern "C" fn pbl_grect_inset(rect: GRect, insets: GRect) -> GRect {
    // insets is actually EdgeInsets { top, right, bottom, left } packed as i16x4
    GRect {
        x: rect.x + insets.y, // left = insets.y in this packing
        y: rect.y + insets.x, // top = insets.x
        w: (rect.w - insets.y - insets.h).max(0), // left + right
        h: (rect.h - insets.x - insets.w).max(0), // top + bottom
    }
}

// ---------------------------------------------------------------------------
// Graphics extras
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_pixel(ctx: *mut PblGContext, point: GPoint) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if let Some(fb) = fb() {
        set_pixel(fb, point.x, point.y, color);
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_round_rect(ctx: *mut PblGContext, rect: GRect, radius: u16) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if let Some(fb) = fb() {
        let r = (radius as i16).min(rect.w / 2).min(rect.h / 2);
        if r <= 0 {
            // No rounding, draw a regular rect
            pbl_graphics_draw_rect(ctx, rect);
            return;
        }
        let x0 = rect.x;
        let x1 = rect.x + rect.w - 1;
        let y0 = rect.y;
        let y1 = rect.y + rect.h - 1;
        // Top and bottom straight edges
        for x in (x0 + r)..=(x1 - r) {
            set_pixel(fb, x, y0, color);
            set_pixel(fb, x, y1, color);
        }
        // Left and right straight edges
        for y in (y0 + r)..=(y1 - r) {
            set_pixel(fb, x0, y, color);
            set_pixel(fb, x1, y, color);
        }
        // Corner arcs using midpoint circle
        let mut cx = r;
        let mut cy: i16 = 0;
        let mut err: i16 = 1 - cx;
        while cy <= cx {
            // Top-right corner (center at x1-r, y0+r)
            set_pixel(fb, x1 - r + cx, y0 + r - cy, color);
            set_pixel(fb, x1 - r + cy, y0 + r - cx, color);
            // Top-left corner (center at x0+r, y0+r)
            set_pixel(fb, x0 + r - cx, y0 + r - cy, color);
            set_pixel(fb, x0 + r - cy, y0 + r - cx, color);
            // Bottom-right corner (center at x1-r, y1-r)
            set_pixel(fb, x1 - r + cx, y1 - r + cy, color);
            set_pixel(fb, x1 - r + cy, y1 - r + cx, color);
            // Bottom-left corner (center at x0+r, y1-r)
            set_pixel(fb, x0 + r - cx, y1 - r + cy, color);
            set_pixel(fb, x0 + r - cy, y1 - r + cx, color);
            cy += 1;
            if err < 0 {
                err += 2 * cy + 1;
            } else {
                cx -= 1;
                err += 2 * (cy - cx) + 1;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_fill_round_rect(ctx: *mut PblGContext, rect: GRect, radius: u16, corner_mask: u8) {
    pbl_graphics_fill_rect(ctx, rect, radius, corner_mask);
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_stroke_width(ctx: *mut PblGContext, width: u8) {
    if !ctx.is_null() {
        unsafe { (*ctx).stroke_width = width; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_antialiased(_ctx: *mut PblGContext, _enable: bool) {}

// ---------------------------------------------------------------------------
// Arc / radial / polar drawing
// ---------------------------------------------------------------------------

/// GOvalScaleMode: 0 = FitCircle (diameter = shortest side), 1 = FillCircle (diameter = longest side)
fn oval_params(rect: &GRect, scale_mode: u8) -> (f32, f32, f32) {
    let cx = rect.x as f32 + rect.w as f32 / 2.0;
    let cy = rect.y as f32 + rect.h as f32 / 2.0;
    let r = if scale_mode == 0 {
        rect.w.min(rect.h) as f32 / 2.0
    } else {
        rect.w.max(rect.h) as f32 / 2.0
    };
    (cx, cy, r)
}

/// Convert Pebble angle (0 = 12 o'clock, clockwise, 0x10000 = 360°) to radians
fn pebble_angle_to_rad(angle: i32) -> f32 {
    // Pebble: 0 = top (12 o'clock), increases clockwise
    // Math: 0 = right (3 o'clock), increases counter-clockwise
    // So: rad = PI/2 - (angle / 0x10000) * 2*PI  ... but we want clockwise
    // Actually: just convert to standard radians where 0 = top, CW
    (angle as f32 / TRIG_MAX_ANGLE) * 2.0 * std::f32::consts::PI - std::f32::consts::FRAC_PI_2
}

/// graphics_draw_arc(ctx, rect, scale_mode, angle_start, angle_end)
/// Draws a line arc clockwise from angle_start to angle_end
#[no_mangle]
pub extern "C" fn pbl_graphics_draw_arc(
    ctx: *mut PblGContext,
    rect: GRect,
    scale_mode: u8,
    angle_start: i32,
    angle_end: i32,
) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    let (cx, cy, r) = oval_params(&rect, scale_mode);
    if r < 1.0 { return; }

    let Some(fb) = fb() else { return };

    // Walk the arc in small steps
    let total_angle = angle_end - angle_start;
    let steps = ((total_angle.abs() as f32 / TRIG_MAX_ANGLE) * r * 8.0).max(16.0) as i32;
    let mut prev_x = -1i16;
    let mut prev_y = -1i16;

    for i in 0..=steps {
        let angle = angle_start + (total_angle as i64 * i as i64 / steps as i64) as i32;
        let rad = pebble_angle_to_rad(angle);
        let px = (cx + r * rad.cos()) as i16;
        let py = (cy + r * rad.sin()) as i16;

        if i > 0 && (px != prev_x || py != prev_y) {
            // Draw line segment from prev to current
            let dx = (px - prev_x).abs();
            let dy = -(py - prev_y).abs();
            let sx: i16 = if prev_x < px { 1 } else { -1 };
            let sy: i16 = if prev_y < py { 1 } else { -1 };
            let mut err = dx + dy;
            let mut x = prev_x;
            let mut y = prev_y;
            loop {
                set_pixel(fb, x, y, color);
                if x == px && y == py { break; }
                let e2 = 2 * err;
                if e2 >= dy { err += dy; x += sx; }
                if e2 <= dx { err += dx; y += sy; }
            }
        } else {
            set_pixel(fb, px, py, color);
        }
        prev_x = px;
        prev_y = py;
    }
}

/// graphics_fill_radial(ctx, rect, scale_mode, inset_thickness, angle_start, angle_end)
/// Fills a wedge/ring between angle_start and angle_end
#[no_mangle]
pub extern "C" fn pbl_graphics_fill_radial(
    ctx: *mut PblGContext,
    rect: GRect,
    scale_mode: u8,
    inset_thickness: u16,
    angle_start: i32,
    angle_end: i32,
) {
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).fill_color } };
    let (cx, cy, r_outer) = oval_params(&rect, scale_mode);
    let r_inner = if inset_thickness > 0 {
        (r_outer - inset_thickness as f32).max(0.0)
    } else {
        0.0
    };

    if r_outer < 1.0 { return; }
    let Some(fb) = fb() else { return };

    // Normalize angles
    let a_start = pebble_angle_to_rad(angle_start);
    let a_end = pebble_angle_to_rad(angle_end);

    // Scan every pixel in the bounding box
    let min_x = (cx - r_outer).floor().max(0.0) as i16;
    let max_x = (cx + r_outer).ceil().min(DISPLAY_WIDTH as f32 - 1.0) as i16;
    let min_y = (cy - r_outer).floor().max(0.0) as i16;
    let max_y = (cy + r_outer).ceil().min(DISPLAY_HEIGHT as f32 - 1.0) as i16;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist_sq = dx * dx + dy * dy;

            // Check radius bounds
            if dist_sq > r_outer * r_outer || dist_sq < r_inner * r_inner {
                continue;
            }

            // Check angle bounds
            let mut pixel_angle = dy.atan2(dx);
            // Normalize to check if pixel is within the arc
            let mut a_s = a_start;
            let mut a_e = a_end;
            // Normalize all to [0, 2*PI)
            let two_pi = 2.0 * std::f32::consts::PI;
            while a_s < 0.0 { a_s += two_pi; }
            while a_e < 0.0 { a_e += two_pi; }
            while pixel_angle < 0.0 { pixel_angle += two_pi; }
            a_s %= two_pi;
            a_e %= two_pi;
            pixel_angle %= two_pi;

            let in_arc = if a_s <= a_e {
                pixel_angle >= a_s && pixel_angle <= a_e
            } else {
                // Wraps around 0
                pixel_angle >= a_s || pixel_angle <= a_e
            };

            // Handle full circle case
            let total = (angle_end - angle_start).abs();
            let in_arc = if total >= 0x10000 { true } else { in_arc };

            if in_arc {
                fb[y as usize * DISPLAY_WIDTH + x as usize] = color;
            }
        }
    }
}

/// gpoint_from_polar(bounds, scale_mode, angle) -> GPoint
/// Returns the point on the ellipse/circle inscribed/circumscribed in `bounds` at `angle`
#[no_mangle]
pub extern "C" fn pbl_gpoint_from_polar(
    rect: GRect,
    scale_mode: u8,
    angle: i32,
) -> GPoint {
    let (cx, cy, r) = oval_params(&rect, scale_mode);
    let rad = pebble_angle_to_rad(angle);
    GPoint {
        x: (cx + r * rad.cos()) as i16,
        y: (cy + r * rad.sin()) as i16,
    }
}

/// grect_centered_from_polar(rect, scale_mode, angle, size) -> GRect
/// Returns a rect of `size` centered on the perimeter of the circle at `angle`
#[no_mangle]
pub extern "C" fn pbl_grect_centered_from_polar(
    rect: GRect,
    scale_mode: u8,
    angle: i32,
    size: GPoint, // GSize is same layout as GPoint: {w: i16, h: i16}
) -> GRect {
    let (cx, cy, r) = oval_params(&rect, scale_mode);
    let rad = pebble_angle_to_rad(angle);
    let px = cx + r * rad.cos();
    let py = cy + r * rad.sin();
    GRect {
        x: (px - size.x as f32 / 2.0) as i16,
        y: (py - size.y as f32 / 2.0) as i16,
        w: size.x,
        h: size.y,
    }
}

/// gpath_draw_outline_open — like gpath_draw_outline but first/last points NOT connected
#[no_mangle]
pub extern "C" fn pbl_gpath_draw_outline_open(ctx: *mut PblGContext, path: *const GPath) {
    if path.is_null() { return; }
    let p = unsafe { &*path };
    let color = if ctx.is_null() { gcolor::colors::WHITE } else { unsafe { (*ctx).stroke_color } };
    if p.num_points < 2 || p.points.is_null() { return; }

    let points: Vec<GPoint> = (0..p.num_points as usize).map(|i| {
        let pt = unsafe { *p.points.add(i) };
        let mut transformed = GPoint { x: pt.x + p.offset.x, y: pt.y + p.offset.y };
        if p.rotation != 0 {
            transformed = rotate_point(pt, p.rotation, GPoint { x: 0, y: 0 });
            transformed.x += p.offset.x;
            transformed.y += p.offset.y;
        }
        transformed
    }).collect();

    if let Some(fb) = fb() {
        // Draw line segments but do NOT connect last to first
        for i in 0..points.len() - 1 {
            let p0 = points[i];
            let p1 = points[i + 1];
            let dx = (p1.x - p0.x).abs();
            let dy = -(p1.y - p0.y).abs();
            let sx: i16 = if p0.x < p1.x { 1 } else { -1 };
            let sy: i16 = if p0.y < p1.y { 1 } else { -1 };
            let mut err = dx + dy;
            let mut x = p0.x;
            let mut y = p0.y;
            loop {
                set_pixel(fb, x, y, color);
                if x == p1.x && y == p1.y { break; }
                let e2 = 2 * err;
                if e2 >= dy { err += dy; x += sx; }
                if e2 <= dx { err += dx; y += sy; }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gcolor_legible_over(bg: GColor8) -> GColor8 {
    // Extract RGB, compute luminance, return black or white
    let r = (bg.0 >> 4) & 0x3;
    let g = (bg.0 >> 2) & 0x3;
    let b = bg.0 & 0x3;
    let lum = r as u16 * 3 + g as u16 * 6 + b as u16; // weighted luminance (0-27)
    if lum > 13 {
        GColor8(gcolor::colors::BLACK)
    } else {
        GColor8(gcolor::colors::WHITE)
    }
}

#[no_mangle]
pub extern "C" fn pbl_gcolor_equal(a: GColor8, b: GColor8) -> bool {
    // Equal if same value, or both invisible (alpha = 0)
    if a.0 == b.0 { return true; }
    let a_alpha = (a.0 >> 6) & 0x3;
    let b_alpha = (b.0 >> 6) & 0x3;
    a_alpha == 0 && b_alpha == 0
}

// ---------------------------------------------------------------------------
// GSize / GRect extras
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gsize_equal(a: *const GPoint, b: *const GPoint) -> bool {
    // GSize has same layout as GPoint: {w, h} = {x, y}
    if a.is_null() || b.is_null() { return false; }
    let a = unsafe { &*a };
    let b = unsafe { &*b };
    a.x == b.x && a.y == b.y
}

#[no_mangle]
pub extern "C" fn pbl_grect_align(rect: *mut GRect, inside: *const GRect, alignment: u8, clip: bool) {
    if rect.is_null() || inside.is_null() { return; }
    let r = unsafe { &mut *rect };
    let i = unsafe { &*inside };
    match alignment {
        0 => { // GAlignCenter
            r.x = i.x + (i.w - r.w) / 2;
            r.y = i.y + (i.h - r.h) / 2;
        }
        1 => { r.x = i.x; r.y = i.y; }                                    // TopLeft
        2 => { r.x = i.x + i.w - r.w; r.y = i.y; }                        // TopRight
        3 => { r.x = i.x + (i.w - r.w) / 2; r.y = i.y; }                 // Top
        4 => { r.x = i.x; r.y = i.y + (i.h - r.h) / 2; }                 // Left
        5 => { r.x = i.x + (i.w - r.w) / 2; r.y = i.y + i.h - r.h; }    // Bottom
        6 => { r.x = i.x + i.w - r.w; r.y = i.y + (i.h - r.h) / 2; }    // Right
        7 => { r.x = i.x + i.w - r.w; r.y = i.y + i.h - r.h; }          // BottomRight
        8 => { r.x = i.x; r.y = i.y + i.h - r.h; }                       // BottomLeft
        _ => {}
    }
    if clip {
        // Clip to inside_rect
        let x2 = (r.x + r.w).min(i.x + i.w);
        let y2 = (r.y + r.h).min(i.y + i.h);
        r.x = r.x.max(i.x);
        r.y = r.y.max(i.y);
        r.w = (x2 - r.x).max(0);
        r.h = (y2 - r.y).max(0);
    }
}

// 2-bit color setters (legacy — convert 2-bit to GColor8)
#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_fill_color_2bit(ctx: *mut PblGContext, color: u8) {
    // 2-bit: 0=clear, 1=black, 2=white. Map to GColor8.
    let gc = match color {
        0 => gcolor::colors::CLEAR,
        1 => gcolor::colors::BLACK,
        _ => gcolor::colors::WHITE,
    };
    if !ctx.is_null() { unsafe { (*ctx).fill_color = gc; } }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_stroke_color_2bit(ctx: *mut PblGContext, color: u8) {
    let gc = match color {
        0 => gcolor::colors::CLEAR,
        1 => gcolor::colors::BLACK,
        _ => gcolor::colors::WHITE,
    };
    if !ctx.is_null() { unsafe { (*ctx).stroke_color = gc; } }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_context_set_text_color_2bit(ctx: *mut PblGContext, color: u8) {
    let gc = match color {
        0 => gcolor::colors::CLEAR,
        1 => gcolor::colors::BLACK,
        _ => gcolor::colors::WHITE,
    };
    if !ctx.is_null() { unsafe { (*ctx).text_color = gc; } }
}

// ---------------------------------------------------------------------------
// Framebuffer capture (for apps that draw directly to the framebuffer)
// ---------------------------------------------------------------------------

/// GBitmap-like struct returned by graphics_capture_frame_buffer
#[repr(C)]
pub struct PblGBitmap {
    pub data: *mut u8,
    pub row_size_bytes: u16,
    pub info_flags: u16,
    pub bounds: GRect,
    /// Custom palette set via gbitmap_set_palette (overrides inline palette in data buffer)
    pub palette: *mut u8,
    /// Whether to free the palette pointer when the bitmap is destroyed
    pub free_palette_on_destroy: bool,
    pub owns_data: bool,
}

static mut CAPTURED_BITMAP: Option<*mut PblGBitmap> = None;

// App timers
type AppTimerCallback = extern "C" fn(usize); // callback(data)
struct AppTimer {
    id: usize,
    deadline: std::time::Instant,
    callback: AppTimerCallback,
    data: usize,
}
static mut APP_TIMERS: Vec<AppTimer> = Vec::new();
static mut NEXT_TIMER_ID: usize = 1;

// Resource pack data (app_resources.pbpack)
static RESOURCE_PACK: Mutex<Option<crate::resources::ResourcePack>> = Mutex::new(None);

/// Get a resource view retaining its immutable pack across replacement/reset.
pub fn resource_get_data(resource_id: u32) -> Option<crate::resources::ResourceData> {
    RESOURCE_PACK.lock().unwrap().as_ref()?.get(resource_id)
}

/// Convert PNG resource data to PBI (Pebble BitmapData) format.
/// PBI header: [row_size_bytes: u16, info_flags: u16, deprecated: u32, width: u16, height: u16]
/// followed by pixel data (and optionally palette for paletted formats).
fn png_to_pbi(resource_id: u32, png_data: &[u8]) -> Option<Vec<u8>> {
    // Try to decode the PNG (with truncation repair if needed)
    let img = match image::load_from_memory(png_data) {
        Ok(img) => img,
        Err(_) => {
            if let Some(fixed) = fix_truncated_png(png_data) {
                image::load_from_memory(&fixed).ok()?
            } else {
                return None;
            }
        }
    };
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    // Determine format based on PNG IHDR
    let (bit_depth, color_type) = png_ihdr_info(png_data)
        .map(|(_, _, bd, ct)| (bd, ct))
        .unwrap_or((8, 6));

    let is_paletted = color_type == 3 && (bit_depth == 1 || bit_depth == 2 || bit_depth == 4);
    let plte = if is_paletted { png_plte_colors(png_data) } else { Vec::new() };

    if is_paletted && !plte.is_empty() {
        // Paletted format
        let pebble_format: u16 = match bit_depth {
            1 => 2, 2 => 3, 4 => 4, _ => 1,
        };
        let num_palette: usize = match bit_depth {
            1 => 2, 2 => 4, 4 => 16, _ => 0,
        };
        let row_size = ((w as usize * bit_depth as usize) + 7) / 8;
        let row_aligned = (row_size + 3) & !3;
        let pixel_bytes = row_aligned * h as usize;

        // Build GColor8 palette
        let mut palette = vec![0u8; num_palette];
        for (i, rgb) in plte.iter().enumerate() {
            if i >= num_palette { break; }
            palette[i] = rgb_to_gcolor8(rgb[0], rgb[1], rgb[2]);
        }

        // Pack pixel data (LSB-first, Pebble convention)
        let mut pixels = vec![0u8; pixel_bytes];
        for y in 0..h {
            let row_off = y as usize * row_aligned;
            for x in 0..w {
                let px = rgba.get_pixel(x, y);
                let gc = rgb_to_gcolor8(px[0], px[1], px[2]);
                let mut best_idx = 0u8;
                let mut best_dist = u32::MAX;
                for (i, &pc) in palette.iter().enumerate() {
                    let dist = if pc == gc { 0 } else {
                        let dr = ((pc >> 4) & 3) as i32 - ((gc >> 4) & 3) as i32;
                        let dg = ((pc >> 2) & 3) as i32 - ((gc >> 2) & 3) as i32;
                        let db = (pc & 3) as i32 - (gc & 3) as i32;
                        (dr*dr + dg*dg + db*db) as u32
                    };
                    if dist < best_dist { best_dist = dist; best_idx = i as u8; }
                }
                match bit_depth {
                    1 => {
                        let byte_idx = x as usize / 8;
                        let bit_pos = x as usize % 8;
                        if best_idx != 0 { pixels[row_off + byte_idx] |= 1 << bit_pos; }
                    }
                    2 => {
                        let byte_idx = x as usize / 4;
                        let shift = (x as usize % 4) * 2;
                        pixels[row_off + byte_idx] |= (best_idx & 0x3) << shift;
                    }
                    4 => {
                        let byte_idx = x as usize / 2;
                        let shift = (x as usize % 2) * 4;
                        pixels[row_off + byte_idx] |= (best_idx & 0xF) << shift;
                    }
                    _ => {}
                }
            }
        }

        // info_flags: bit 0 = heap_allocated, bits 1-3 = format
        let info_flags = 1u16 | ((pebble_format & 0x7) << 1);
        let mut pbi = Vec::with_capacity(12 + pixel_bytes + num_palette);
        pbi.extend_from_slice(&(row_aligned as u16).to_le_bytes());
        pbi.extend_from_slice(&info_flags.to_le_bytes());
        pbi.extend_from_slice(&0u32.to_le_bytes()); // deprecated
        pbi.extend_from_slice(&(w as u16).to_le_bytes());
        pbi.extend_from_slice(&(h as u16).to_le_bytes());
        pbi.extend_from_slice(&pixels);
        pbi.extend_from_slice(&palette);

        eprintln!("[pebble:res] png_to_pbi({}) — {}x{} {}bit palette -> PBI {}B",
            resource_id, w, h, bit_depth, pbi.len());
        Some(pbi)
    } else {
        // GColor8 format
        let row_size = w as usize;
        let pixel_bytes = row_size * h as usize;
        let info_flags = 1u16 | (1u16 << 1); // heap_allocated | GBitmapFormat8Bit
        let mut pbi = Vec::with_capacity(12 + pixel_bytes);
        pbi.extend_from_slice(&(row_size as u16).to_le_bytes());
        pbi.extend_from_slice(&info_flags.to_le_bytes());
        pbi.extend_from_slice(&0u32.to_le_bytes());
        pbi.extend_from_slice(&(w as u16).to_le_bytes());
        pbi.extend_from_slice(&(h as u16).to_le_bytes());
        for y in 0..h {
            for x in 0..w {
                let px = rgba.get_pixel(x, y);
                let gc = ((px[3] >> 6) << 6) | ((px[0] >> 6) << 4) | ((px[1] >> 6) << 2) | (px[2] >> 6);
                pbi.push(gc);
            }
        }
        eprintln!("[pebble:res] png_to_pbi({}) — {}x{} -> GColor8 PBI {}B",
            resource_id, w, h, pbi.len());
        Some(pbi)
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_capture_frame_buffer(ctx: *mut PblGContext) -> *mut PblGBitmap {
    pbl_graphics_capture_frame_buffer_format(ctx, 1) // GBitmapFormat8Bit
}

#[no_mangle]
pub extern "C" fn pbl_graphics_capture_frame_buffer_format(_ctx: *mut PblGContext, _format: u8) -> *mut PblGBitmap {
    if let Some(bitmap) = unsafe { CAPTURED_BITMAP } { return bitmap; }
    if let Some(fb_ptr) = unsafe { FRAMEBUFFER } {
        let bitmap = crate::owned::new(PblGBitmap {
            data: fb_ptr,
            row_size_bytes: DISPLAY_WIDTH as u16,
            info_flags: 1, // GBitmapFormat8Bit (clean format value)
            bounds: GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 },
            palette: std::ptr::null_mut(),
            free_palette_on_destroy: false,
            owns_data: false,
        });
        unsafe { CAPTURED_BITMAP = Some(bitmap); }
        bitmap
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_release_frame_buffer(_ctx: *mut PblGContext, _bitmap: *mut PblGBitmap) {
    unsafe {
        if let Some(bmp) = CAPTURED_BITMAP.take() {
            crate::owned::release(bmp);
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_graphics_frame_buffer_is_captured(_ctx: *mut PblGContext) -> bool {
    unsafe { CAPTURED_BITMAP.is_some() }
}

// ---------------------------------------------------------------------------
// GBitmap accessors
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_data(bitmap: *mut PblGBitmap) -> *mut u8 {
    if bitmap.is_null() { return std::ptr::null_mut(); }
    unsafe { (*bitmap).data }
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_bytes_per_row(bitmap: *mut PblGBitmap) -> u16 {
    if bitmap.is_null() { return 0; }
    unsafe { (*bitmap).row_size_bytes }
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_bounds(bitmap: *mut PblGBitmap) -> GRect {
    if bitmap.is_null() {
        return GRect { x: 0, y: 0, w: DISPLAY_WIDTH as i16, h: DISPLAY_HEIGHT as i16 };
    }
    unsafe { (*bitmap).bounds }
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_format(bitmap: *mut PblGBitmap) -> u8 {
    if bitmap.is_null() { return 1; } // default to 8Bit
    unsafe { (*bitmap).info_flags as u8 }
}

/// GBitmapDataRowInfo: per-row info for bitmap rendering (8 bytes on ARM).
/// On Chalk (round display), rows can have different start/end x coordinates.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GBitmapDataRowInfo {
    pub data: *mut u8,  // pointer to start of row's pixel data
    pub min_x: i16,     // first visible pixel x
    pub max_x: i16,     // last visible pixel x
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_get_data_row_info(bitmap: *const PblGBitmap, y: u16) -> GBitmapDataRowInfo {
    if bitmap.is_null() {
        return GBitmapDataRowInfo { data: std::ptr::null_mut(), min_x: 0, max_x: 0 };
    }
    unsafe {
        let bmp = &*bitmap;
        let row_offset = (y as usize) * (bmp.row_size_bytes as usize);
        let data = bmp.data.add(row_offset);
        let width = bmp.bounds.w;
        GBitmapDataRowInfo {
            data,
            min_x: 0,
            max_x: if width > 0 { width - 1 } else { 0 },
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_blank(w: i16, h: i16) -> *mut PblGBitmap {
    if w <= 0 || h <= 0 { return std::ptr::null_mut(); }
    let size = (w as usize) * (h as usize);
    let data = unsafe { libc::calloc(size, 1) as *mut u8 };
    if data.is_null() { return std::ptr::null_mut(); }
    crate::owned::new(PblGBitmap {
        data,
        row_size_bytes: w as u16,
        info_flags: 1, // GBitmapFormat8Bit (clean format value)
        bounds: GRect { x: 0, y: 0, w, h },
        palette: std::ptr::null_mut(),
        free_palette_on_destroy: false,
        owns_data: true,
    })
}

// ---------------------------------------------------------------------------
// Layer extras
// ---------------------------------------------------------------------------

static mut CURRENT_WINDOW: *mut PblWindow = std::ptr::null_mut();
static mut WINDOW_BG_COLOR: u8 = 0x00; // BLACK (GColor8)

#[no_mangle]
pub extern "C" fn pbl_layer_get_window(_layer: *mut PblLayer) -> *mut PblWindow {
    unsafe { CURRENT_WINDOW }
}

#[no_mangle]
pub extern "C" fn pbl_layer_insert_above_sibling(_layer: *mut PblLayer, _above: *mut PblLayer) {}

#[no_mangle]
pub extern "C" fn pbl_layer_insert_below_sibling(_layer: *mut PblLayer, _below: *mut PblLayer) {}

#[no_mangle]
pub extern "C" fn pbl_layer_remove_child_layers(parent: *mut PblLayer) {
    if crate::owned::generation(parent).is_none() { return; }
    unsafe {
        while !(*parent).first_child.is_null() { pbl_layer_remove_from_parent((*parent).first_child); }
    }
}

#[no_mangle]
pub extern "C" fn pbl_layer_set_clips(_layer: *mut PblLayer, _clips: bool) {}

// ---------------------------------------------------------------------------
// GPoint helpers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gpoint_equal(a: *const GPoint, b: *const GPoint) -> bool {
    if a.is_null() || b.is_null() { return false; }
    let a = unsafe { &*a };
    let b = unsafe { &*b };
    a.x == b.x && a.y == b.y
}

// ---------------------------------------------------------------------------
// Connection / battery
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_connection_service_peek_pebble_app_connection() -> bool { false }

#[no_mangle]
pub extern "C" fn pbl_connection_service_peek_pebblekit_connection() -> bool { false }

/// Read actual battery state from sysfs.
/// Returns a packed BatteryChargeState (little-endian):
///   byte 0: charge_percent (0-100)
///   byte 1: is_charging (0 or 1)
///   byte 2: is_plugged (0 or 1)
fn read_battery_state() -> u32 {
    let base = std::env::var_os("HOKI_SIM_STATE")
        .map(std::path::PathBuf::from).unwrap_or_else(|| "/".into())
        .join("sys/class/power_supply/battery");
    let capacity = std::fs::read_to_string(base.join("capacity"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(100) as u8;

    let status = std::fs::read_to_string(base.join("status"))
        .unwrap_or_default();
    let status = status.trim();
    let is_charging: u8 = if status == "Charging" { 1 } else { 0 };
    let is_plugged: u8 = if status == "Charging" || status == "Full" || status == "Not charging" { 1 } else { 0 };

    (capacity as u32) | ((is_charging as u32) << 8) | ((is_plugged as u32) << 16)
}

static mut BATTERY_HANDLER: Option<extern "C" fn(u32)> = None;
static mut LAST_BATTERY_STATE: u32 = u32::MAX; // sentinel so first check always fires

#[no_mangle]
pub extern "C" fn pbl_battery_state_service_peek() -> u32 {
    read_battery_state()
}

#[no_mangle]
pub extern "C" fn pbl_battery_state_service_subscribe(handler: Option<extern "C" fn(u32)>) {
    unsafe { BATTERY_HANDLER = handler; }
}

#[no_mangle]
pub extern "C" fn pbl_battery_state_service_unsubscribe() {
    unsafe { BATTERY_HANDLER = None; }
}

/// Called from the event loop to fire battery callbacks when state changes.
pub fn poll_battery() {
    let state = read_battery_state();
    unsafe {
        if state != LAST_BATTERY_STATE {
            LAST_BATTERY_STATE = state;
            if let Some(handler) = BATTERY_HANDLER {
                handler(state);
                FRAME_DIRTY = true;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_connection_service_subscribe(_handlers: usize) {}

#[no_mangle]
pub extern "C" fn pbl_connection_service_unsubscribe() {}

#[no_mangle]
pub extern "C" fn pbl_bluetooth_connection_service_peek() -> bool { crate::bluetooth::peek() }

#[no_mangle]
pub extern "C" fn pbl_bluetooth_connection_service_subscribe(handler: Option<extern "C" fn(bool)>) { crate::bluetooth::subscribe(handler); }

#[no_mangle]
pub extern "C" fn pbl_bluetooth_connection_service_unsubscribe() { crate::bluetooth::reset(); }

// ---------------------------------------------------------------------------
// Health stubs
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_health_service_peek_current_value(_metric: u32) -> i32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_health_service_sum_today(_metric: u32) -> i32 { 0 }

/// health_service_events_subscribe(handler, context) -> bool
/// No health hardware — silently accept the subscription and return true.
#[no_mangle]
pub extern "C" fn pbl_health_service_events_subscribe(_handler: usize, _context: usize) -> u32 {
    1 // true — pretend subscription succeeded
}

/// health_service_metric_accessible(metric, time_start, time_end) -> HealthServiceAccessibilityMask
/// Return 0 = HealthServiceAccessibilityMaskNotAvailable.
#[no_mangle]
pub extern "C" fn pbl_health_service_metric_accessible(
    _metric: u32,
    _time_start: u32,
    _time_end: u32,
) -> u32 {
    0 // not available
}

/// time_start_of_today() -> time_t (seconds since epoch at midnight local time today)
#[no_mangle]
pub extern "C" fn pbl_time_start_of_today() -> i32 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let tm = &*libc::localtime(&now);
        // Build a tm struct for midnight today
        let mut midnight = *tm;
        midnight.tm_sec = 0;
        midnight.tm_min = 0;
        midnight.tm_hour = 0;
        midnight.tm_isdst = -1; // let mktime figure out DST
        libc::mktime(&mut midnight) as i32
    }
}

// ---------------------------------------------------------------------------
// Unobstructed area stubs
// ---------------------------------------------------------------------------

/// layer_get_unobstructed_bounds — since we have no timeline peek, just return layer_get_bounds.
#[no_mangle]
pub extern "C" fn pbl_layer_get_unobstructed_bounds(layer: *mut PblLayer) -> GRect {
    pbl_layer_get_bounds(layer)
}

/// unobstructed_area_service_subscribe — no-op stub.
#[no_mangle]
pub extern "C" fn pbl_unobstructed_area_service_subscribe(
    _handler_change: usize,
    _handler_will_change: usize,
    _handler_did_change: usize,
    _context: usize,
) {
    eprintln!("[pebble] unobstructed_area_service_subscribe: no-op stub");
}

// ---------------------------------------------------------------------------
// Helpers for emulated path (emu.rs)
// ---------------------------------------------------------------------------

/// Get the framebuffer as a mutable slice (public for emu.rs)
pub fn get_fb() -> Option<&'static mut [u8]> {
    fb()
}

/// Look up a system font by name (takes &str instead of *const c_char)
pub fn pbl_fonts_get_system_font_by_name(key: &str) -> *const u8 {
    font::font_for_key(key)
}

// ---------------------------------------------------------------------------
// New stubs: layer, window, text layout, clock, click, bitmap, focus
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_layer_get_clips(_layer: *mut PblLayer) -> bool { true }

#[no_mangle]
pub extern "C" fn pbl_graphics_text_layout_get_max_used_size(
    _ctx: *mut PblGContext, text: *const u8, font: *const u8, box_rect: GRect,
    _overflow: u32, _alignment: u32, _layout: *mut u8,
) -> GSize {
    pbl_graphics_text_layout_get_content_size(text, font, box_rect, _overflow, _alignment)
}

#[no_mangle]
pub extern "C" fn pbl_graphics_text_layout_get_content_size(
    text: *const u8, font: *const u8, box_rect: GRect, _overflow: u32, _alignment: u32,
) -> GSize {
    if text.is_null() { return GSize { w: 0, h: 0 }; }
    let text_str = unsafe { std::ffi::CStr::from_ptr(text as *const std::ffi::c_char) };
    let text_str = match text_str.to_str() {
        Ok(s) => s,
        Err(_) => return GSize { w: 0, h: 0 },
    };
    let (w, h) = font::measure_text(font, text_str);
    // Clamp to box dimensions
    GSize {
        w: w.min(box_rect.w),
        h: h.min(box_rect.h),
    }
}

/// Measure text content size from a &str (for emulator path)
pub fn measure_text_content(text: &str, font: *const u8, box_rect: GRect) -> GSize {
    let (w, h) = font::measure_text(font, text);
    GSize {
        w: w.min(box_rect.w),
        h: h.min(box_rect.h),
    }
}

#[no_mangle]
pub extern "C" fn pbl_time_ms(time_out: *mut i32, ms_out: *mut u16) {
    unsafe {
        let mut tv: libc::timeval = std::mem::zeroed();
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        if !time_out.is_null() { *time_out = tv.tv_sec as i32; }
        if !ms_out.is_null() { *ms_out = (tv.tv_usec / 1000) as u16; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_window_stack_contains_window(_window: *mut PblWindow) -> bool { true }

#[no_mangle]
pub extern "C" fn pbl_window_stack_get_top_window() -> *mut PblWindow {
    unsafe { CURRENT_WINDOW }
}

#[no_mangle]
pub extern "C" fn pbl_app_focus_service_subscribe(_handler: Option<extern "C" fn(bool)>) {}

#[no_mangle]
pub extern "C" fn pbl_app_focus_service_unsubscribe() {}

#[no_mangle]
pub extern "C" fn pbl_app_focus_service_subscribe_handlers(
    _will_focus: Option<extern "C" fn(bool)>,
    _did_focus: Option<extern "C" fn(bool)>,
) {}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_set_alignment(_bl: *mut PblBitmapLayer, _alignment: u8) {}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_set_background_color_2bit(_bl: *mut PblBitmapLayer, _color: u8) {}

#[no_mangle]
pub extern "C" fn pbl_bitmap_layer_get_bitmap(bl: *mut PblBitmapLayer) -> *mut u8 {
    if bl.is_null() { return std::ptr::null_mut(); }
    unsafe { (*bl).bitmap }
}

/// Pebble-compatible strftime wrapper.
/// Tries the format as-is first. If glibc returns 0 (buffer too small),
/// retries with `%T` → `%H:%M` and `%r` → `%I:%M %p` (drop seconds)
/// so watchfaces with tight buffers still get a usable time string.
#[no_mangle]
pub extern "C" fn pbl_strftime(
    buf: *mut libc::c_char,
    maxsize: libc::size_t,
    fmt: *const libc::c_char,
    tm: *const libc::tm,
) -> libc::size_t {
    unsafe {
        // Try the original format first
        let ret = libc::strftime(buf, maxsize, fmt, tm);
        if ret != 0 {
            return ret;
        }

        // Overflow — retry with seconds stripped from %T and %r
        let fmt_cstr = std::ffi::CStr::from_ptr(fmt);
        let fmt_str = fmt_cstr.to_bytes();
        let mut has_patchable = false;
        for w in fmt_str.windows(2) {
            if w[0] == b'%' && (w[1] == b'T' || w[1] == b'r') {
                has_patchable = true;
                break;
            }
        }
        if !has_patchable {
            return 0; // nothing we can fix, propagate the failure
        }

        let mut patched = Vec::with_capacity(fmt_str.len() + 8);
        let mut i = 0;
        while i < fmt_str.len() {
            if fmt_str[i] == b'%' && i + 1 < fmt_str.len() {
                match fmt_str[i + 1] {
                    b'T' => { patched.extend_from_slice(b"%H:%M"); i += 2; continue; }
                    b'r' => { patched.extend_from_slice(b"%I:%M %p"); i += 2; continue; }
                    _ => {}
                }
            }
            patched.push(fmt_str[i]);
            i += 1;
        }
        patched.push(0);

        libc::strftime(buf, maxsize, patched.as_ptr() as *const libc::c_char, tm)
    }
}

#[no_mangle]
pub extern "C" fn pbl_clock_copy_time_string(buffer: *mut u8, size: u8) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let tm = libc::localtime(&t);
        libc::strftime(
            buffer as *mut libc::c_char, size as usize,
            if pbl_clock_is_24h_style() { b"%H:%M\0".as_ptr() as *const _ }
            else { b"%I:%M\0".as_ptr() as *const _ },
            tm,
        );
    }
}

#[no_mangle]
pub extern "C" fn pbl_click_number_of_clicks_counted(_recognizer: *mut u8) -> u8 { 1 }

#[no_mangle]
pub extern "C" fn pbl_click_recognizer_get_button_id(recognizer: *mut u8) -> u8 {
    if recognizer.is_null() { 0 } else { unsafe { *recognizer } }
}

// ---------------------------------------------------------------------------
// Animation system
// ---------------------------------------------------------------------------

// Animation curves
const ANIMATION_CURVE_LINEAR: u8 = 0;
const ANIMATION_CURVE_EASE_IN: u8 = 1;
const ANIMATION_CURVE_EASE_OUT: u8 = 2;
const ANIMATION_CURVE_EASE_IN_OUT: u8 = 3;

type AnimationStartedHandler = extern "C" fn(*mut PblAnimation, *mut u8);
type AnimationStoppedHandler = extern "C" fn(*mut PblAnimation, bool, *mut u8);

#[derive(Clone, Copy, PartialEq)]
pub enum AnimType {
    Single,
    Sequence,
    Spawn,
}

#[repr(C)]
pub struct PblAnimation {
    pub duration_ms: u32,
    pub delay_ms: u32,
    pub curve: u8,
    pub scheduled: bool,
    pub start_time: Option<std::time::Instant>,
    pub context: usize,
    pub started_handler: Option<AnimationStartedHandler>,
    pub stopped_handler: Option<AnimationStoppedHandler>,
    // For property animations: target layer and from/to frames
    pub target_layer: *mut PblLayer,
    pub from_frame: GRect,
    pub to_frame: GRect,
    pub is_property_anim: bool,
    pub is_bounds_anim: bool,
    // Composite animations
    pub anim_type: AnimType,
    pub children: Vec<*mut PblAnimation>,
    pub current_child_idx: usize,
    // Emulator support: emulated callback addresses (0 = none)
    pub emu_started_handler: u32,
    pub emu_stopped_handler: u32,
    pub emu_layer_handle: u32,
}

/// Events returned by tick_animations for the caller to dispatch
pub enum AnimEvent {
    Started(*mut PblAnimation, u64),
    Stopped(*mut PblAnimation, bool, u64),
}

static mut ANIMATIONS: Vec<*mut PblAnimation> = Vec::new();
static mut ALL_ANIMATIONS: Vec<*mut PblAnimation> = Vec::new();

/// Returns true if any animations are currently scheduled (for sleep optimization)
pub fn has_active_animations() -> bool {
    unsafe {
        ANIMATIONS.iter().any(|&a| !a.is_null() && (*a).scheduled)
    }
}

/// Expose animation list for emulator layer syncing
pub unsafe fn get_animations() -> &'static Vec<*mut PblAnimation> {
    &ANIMATIONS
}

/// Apply animation curve to linear progress t (0.0..1.0)
fn apply_curve(t: f32, curve: u8) -> f32 {
    match curve {
        ANIMATION_CURVE_EASE_IN => t * t,
        ANIMATION_CURVE_EASE_OUT => t * (2.0 - t),
        ANIMATION_CURVE_EASE_IN_OUT => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                -1.0 + (4.0 - 2.0 * t) * t
            }
        }
        _ => t, // LINEAR or unknown
    }
}

/// Tick a single animation, returns true if it finished this tick
unsafe fn tick_single_anim(
    anim_ptr: *mut PblAnimation,
    now: std::time::Instant,
    events: &mut Vec<AnimEvent>,
) -> bool {
    if anim_ptr.is_null() { return false; }
    let anim = &mut *anim_ptr;
    if !anim.scheduled { return false; }

    let start = match anim.start_time {
        Some(t) => t,
        None => {
            anim.start_time = Some(now);
            events.push(AnimEvent::Started(anim_ptr, crate::owned::generation(anim_ptr).unwrap()));
            now
        }
    };

    let elapsed = now.duration_since(start);
    let delay = std::time::Duration::from_millis(anim.delay_ms as u64);
    if elapsed < delay { return false; }

    let active_elapsed = elapsed - delay;
    let duration = std::time::Duration::from_millis(anim.duration_ms.max(1) as u64);
    let t_linear = (active_elapsed.as_millis() as f32 / duration.as_millis().max(1) as f32).min(1.0);
    let t = apply_curve(t_linear, anim.curve);

    // Interpolate property animation
    if anim.is_property_anim && !anim.target_layer.is_null() {
        FRAME_DIRTY = true;
        let layer = &mut *anim.target_layer;
        let interpolated = GRect {
            x: lerp_i16(anim.from_frame.x, anim.to_frame.x, t),
            y: lerp_i16(anim.from_frame.y, anim.to_frame.y, t),
            w: lerp_i16(anim.from_frame.w, anim.to_frame.w, t),
            h: lerp_i16(anim.from_frame.h, anim.to_frame.h, t),
        };
        if anim.is_bounds_anim {
            layer.bounds = interpolated;
        } else {
            layer.frame = interpolated;
        }
    }

    if t_linear >= 1.0 {
        anim.scheduled = false;
        events.push(AnimEvent::Stopped(anim_ptr, true, crate::owned::generation(anim_ptr).unwrap()));
        return true;
    }
    false
}

pub fn tick_animations() -> Vec<AnimEvent> {
    unsafe {
        let now = std::time::Instant::now();
        let mut events = Vec::new();

        // Collect pointers to avoid borrow issues (callbacks may modify ANIMATIONS)
        let anim_ptrs: Vec<*mut PblAnimation> = ANIMATIONS.iter().copied().collect();

        for &anim_ptr in &anim_ptrs {
            if anim_ptr.is_null() { continue; }
            let anim = &mut *anim_ptr;
            if !anim.scheduled { continue; }

            match anim.anim_type {
                AnimType::Single => {
                    tick_single_anim(anim_ptr, now, &mut events);
                }
                AnimType::Sequence => {
                    // Initialize: start the sequence
                    if anim.start_time.is_none() {
                        anim.start_time = Some(now);
                        events.push(AnimEvent::Started(anim_ptr, crate::owned::generation(anim_ptr).unwrap()));
                        // Schedule first child
                        if let Some(&child) = anim.children.first() {
                            if !child.is_null() {
                                (*child).scheduled = true;
                                (*child).start_time = None;
                            }
                        }
                    }
                    // Tick current child
                    let idx = anim.current_child_idx;
                    if idx < anim.children.len() {
                        let child = anim.children[idx];
                        let child_done = tick_single_anim(child, now, &mut events);
                        if child_done {
                            anim.current_child_idx += 1;
                            // Schedule next child
                            if anim.current_child_idx < anim.children.len() {
                                let next = anim.children[anim.current_child_idx];
                                if !next.is_null() {
                                    (*next).scheduled = true;
                                    (*next).start_time = None;
                                }
                            } else {
                                // Sequence complete
                                anim.scheduled = false;
                                events.push(AnimEvent::Stopped(anim_ptr, true, crate::owned::generation(anim_ptr).unwrap()));
                            }
                        }
                    } else {
                        // No children or past end
                        anim.scheduled = false;
                        events.push(AnimEvent::Stopped(anim_ptr, true, crate::owned::generation(anim_ptr).unwrap()));
                    }
                }
                AnimType::Spawn => {
                    // Initialize: start all children
                    if anim.start_time.is_none() {
                        anim.start_time = Some(now);
                        events.push(AnimEvent::Started(anim_ptr, crate::owned::generation(anim_ptr).unwrap()));
                        for &child in &anim.children {
                            if !child.is_null() {
                                (*child).scheduled = true;
                                (*child).start_time = None;
                            }
                        }
                    }
                    // Tick all children
                    let mut all_done = true;
                    for &child in &anim.children {
                        tick_single_anim(child, now, &mut events);
                        if !child.is_null() && (*child).scheduled {
                            all_done = false;
                        }
                    }
                    if all_done && !anim.children.is_empty() {
                        anim.scheduled = false;
                        events.push(AnimEvent::Stopped(anim_ptr, true, crate::owned::generation(anim_ptr).unwrap()));
                    }
                }
            }
        }

        events
    }
}

/// Fire native callbacks for animation events (used by native/on-device path)
pub fn dispatch_native_anim_events(events: &[AnimEvent]) {
    unsafe {
        for event in events {
            match event {
                AnimEvent::Started(anim_ptr, generation) => {
                    if crate::owned::generation(*anim_ptr) != Some(*generation) { continue; }
                    let anim = &**anim_ptr;
                    if let Some(handler) = anim.started_handler {
                        handler(*anim_ptr, anim.context as *mut u8);
                    }
                }
                AnimEvent::Stopped(anim_ptr, finished, generation) => {
                    if crate::owned::generation(*anim_ptr) != Some(*generation) { continue; }
                    let anim = &**anim_ptr;
                    if let Some(handler) = anim.stopped_handler {
                        handler(*anim_ptr, *finished, anim.context as *mut u8);
                    }
                }
            }
        }
    }
}

fn lerp_i16(a: i16, b: i16, t: f32) -> i16 {
    (a as f32 + (b as f32 - a as f32) * t) as i16
}

pub fn reset_animations() {
    unsafe {
        while let Some(&anim) = ALL_ANIMATIONS.last() { pbl_animation_destroy(anim); }
        ANIMATIONS.clear();
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_create() -> *mut PblAnimation {
    let anim = crate::owned::new(PblAnimation {
        duration_ms: 250,
        delay_ms: 0,
        curve: ANIMATION_CURVE_LINEAR,
        scheduled: false,
        start_time: None,
        context: 0,
        started_handler: None,
        stopped_handler: None,
        target_layer: std::ptr::null_mut(),
        from_frame: GRect { x: 0, y: 0, w: 0, h: 0 },
        to_frame: GRect { x: 0, y: 0, w: 0, h: 0 },
        is_property_anim: false,
        is_bounds_anim: false,
        anim_type: AnimType::Single,
        children: Vec::new(),
        current_child_idx: 0,
        emu_started_handler: 0,
        emu_stopped_handler: 0,
        emu_layer_handle: 0,
    });
    unsafe { ANIMATIONS.push(anim); ALL_ANIMATIONS.push(anim); }
    anim
}

#[no_mangle]
pub extern "C" fn pbl_animation_destroy(anim: *mut PblAnimation) {
    if crate::owned::generation(anim).is_none() { return; }
    unsafe {
        let children = std::mem::take(&mut (*anim).children);
        ANIMATIONS.retain(|&a| a != anim);
        ALL_ANIMATIONS.retain(|&a| a != anim);
        for &parent in &ALL_ANIMATIONS { (*parent).children.retain(|&a| a != anim); }
        crate::owned::release(anim);
        for child in children { pbl_animation_destroy(child); }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_set_duration(anim: *mut PblAnimation, duration_ms: u32) {
    if !anim.is_null() {
        unsafe { (*anim).duration_ms = duration_ms; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_set_delay(anim: *mut PblAnimation, delay_ms: u32) {
    if !anim.is_null() {
        unsafe { (*anim).delay_ms = delay_ms; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_set_curve(anim: *mut PblAnimation, curve: u8) {
    if !anim.is_null() {
        unsafe { (*anim).curve = curve; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_set_custom_curve(_anim: *mut PblAnimation, _curve_fn: usize) {}

#[no_mangle]
pub extern "C" fn pbl_animation_set_handlers(
    anim: *mut PblAnimation,
    started: Option<AnimationStartedHandler>,
    stopped: Option<AnimationStoppedHandler>,
    context: *mut u8,
) {
    if !anim.is_null() {
        unsafe {
            (*anim).started_handler = started;
            (*anim).stopped_handler = stopped;
            (*anim).context = context as usize;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_set_implementation(_anim: *mut PblAnimation, _impl_ptr: *const u8) {}

#[no_mangle]
pub extern "C" fn pbl_animation_schedule(anim: *mut PblAnimation) {
    if !anim.is_null() {
        unsafe {
            (*anim).scheduled = true;
            (*anim).start_time = None;
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_unschedule(anim: *mut PblAnimation) {
    if !anim.is_null() {
        unsafe { (*anim).scheduled = false; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_unschedule_all() {
    unsafe {
        for &anim_ptr in ANIMATIONS.iter() {
            if !anim_ptr.is_null() {
                (*anim_ptr).scheduled = false;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_is_scheduled(anim: *mut PblAnimation) -> bool {
    if anim.is_null() { return false; }
    unsafe { (*anim).scheduled }
}

#[no_mangle]
pub extern "C" fn pbl_animation_get_context(anim: *mut PblAnimation) -> *mut u8 {
    if anim.is_null() { return std::ptr::null_mut(); }
    unsafe { (*anim).context as *mut u8 }
}

#[no_mangle]
pub extern "C" fn pbl_animation_clone(anim: *mut PblAnimation) -> *mut PblAnimation {
    if anim.is_null() { return pbl_animation_create(); }
    unsafe {
        let new = pbl_animation_create();
        (*new).duration_ms = (*anim).duration_ms;
        (*new).delay_ms = (*anim).delay_ms;
        (*new).curve = (*anim).curve;
        (*new).context = (*anim).context;
        (*new).started_handler = (*anim).started_handler;
        (*new).stopped_handler = (*anim).stopped_handler;
        (*new).is_property_anim = (*anim).is_property_anim;
        (*new).is_bounds_anim = (*anim).is_bounds_anim;
        (*new).target_layer = (*anim).target_layer;
        (*new).from_frame = (*anim).from_frame;
        (*new).to_frame = (*anim).to_frame;
        (*new).emu_started_handler = (*anim).emu_started_handler;
        (*new).emu_stopped_handler = (*anim).emu_stopped_handler;
        (*new).emu_layer_handle = (*anim).emu_layer_handle;
        new
    }
}

#[no_mangle]
pub extern "C" fn pbl_animation_get_delay(anim: *mut PblAnimation) -> u32 {
    if anim.is_null() { return 0; }
    unsafe { (*anim).delay_ms }
}

#[no_mangle]
pub extern "C" fn pbl_animation_get_duration(anim: *mut PblAnimation) -> u32 {
    if anim.is_null() { return 0; }
    unsafe { (*anim).duration_ms }
}

#[no_mangle]
pub extern "C" fn pbl_animation_get_play_count(_anim: *mut PblAnimation) -> u32 { 1 }

#[no_mangle]
pub extern "C" fn pbl_animation_set_play_count(_anim: *mut PblAnimation, _count: u32) {}

#[no_mangle]
pub extern "C" fn pbl_animation_get_elapsed(_anim: *mut PblAnimation, _elapsed: *mut i32) -> bool { false }

#[no_mangle]
pub extern "C" fn pbl_animation_set_elapsed(_anim: *mut PblAnimation, _elapsed: i32) {}

#[no_mangle]
pub extern "C" fn pbl_animation_get_reverse(_anim: *mut PblAnimation) -> bool { false }

#[no_mangle]
pub extern "C" fn pbl_animation_set_reverse(_anim: *mut PblAnimation, _reverse: bool) {}

#[no_mangle]
pub extern "C" fn pbl_animation_get_curve(anim: *mut PblAnimation) -> u8 {
    if anim.is_null() { return 0; }
    unsafe { (*anim).curve }
}

#[no_mangle]
pub extern "C" fn pbl_animation_get_custom_curve(_anim: *mut PblAnimation) -> usize { 0 }

#[no_mangle]
pub extern "C" fn pbl_animation_get_implementation(_anim: *mut PblAnimation) -> *const u8 {
    std::ptr::null()
}

/// Collect non-null varargs animations. The Pebble API uses NULL as sentinel.
unsafe fn collect_varargs(a: *mut PblAnimation, b: *mut PblAnimation, end: *mut PblAnimation) -> Vec<*mut PblAnimation> {
    let mut children = Vec::new();
    if !a.is_null() { children.push(a); }
    if !b.is_null() { children.push(b); }
    // 'end' is typically NULL sentinel; if not null, it's another animation
    if !end.is_null() { children.push(end); }
    children
}

#[no_mangle]
pub extern "C" fn pbl_animation_sequence_create(
    anim_a: *mut PblAnimation, anim_b: *mut PblAnimation, end: *mut PblAnimation,
) -> *mut PblAnimation {
    let children = unsafe { collect_varargs(anim_a, anim_b, end) };
    let seq = pbl_animation_create();
    if !seq.is_null() {
        unsafe {
            // Remove children from top-level ANIMATIONS (they're owned by the sequence now)
            for &child in &children {
                ANIMATIONS.retain(|&a| a != child);
            }
            (*seq).anim_type = AnimType::Sequence;
            (*seq).children = children;
            // Total duration = sum of children (for get_duration queries)
            let total: u32 = (*seq).children.iter().map(|&c| {
                if c.is_null() { 0 } else { (*c).duration_ms + (*c).delay_ms }
            }).sum();
            (*seq).duration_ms = total;
        }
    }
    seq
}

#[no_mangle]
pub extern "C" fn pbl_animation_sequence_create_from_array(
    array: *const *mut PblAnimation, count: u32,
) -> *mut PblAnimation {
    if count == 0 || array.is_null() { return pbl_animation_create(); }
    let seq = pbl_animation_create();
    if !seq.is_null() {
        unsafe {
            (*seq).anim_type = AnimType::Sequence;
            for i in 0..count as usize {
                let child = *array.add(i);
                if !child.is_null() {
                    ANIMATIONS.retain(|&a| a != child);
                    (*seq).children.push(child);
                }
            }
            let total: u32 = (*seq).children.iter().map(|&c| {
                if c.is_null() { 0 } else { (*c).duration_ms + (*c).delay_ms }
            }).sum();
            (*seq).duration_ms = total;
        }
    }
    seq
}

#[no_mangle]
pub extern "C" fn pbl_animation_spawn_create(
    anim_a: *mut PblAnimation, anim_b: *mut PblAnimation, end: *mut PblAnimation,
) -> *mut PblAnimation {
    let children = unsafe { collect_varargs(anim_a, anim_b, end) };
    let spawn = pbl_animation_create();
    if !spawn.is_null() {
        unsafe {
            for &child in &children {
                ANIMATIONS.retain(|&a| a != child);
            }
            (*spawn).anim_type = AnimType::Spawn;
            (*spawn).children = children;
            // Duration = max of children
            let max_dur: u32 = (*spawn).children.iter().map(|&c| {
                if c.is_null() { 0 } else { (*c).duration_ms + (*c).delay_ms }
            }).max().unwrap_or(0);
            (*spawn).duration_ms = max_dur;
        }
    }
    spawn
}

#[no_mangle]
pub extern "C" fn pbl_animation_spawn_create_from_array(
    array: *const *mut PblAnimation, count: u32,
) -> *mut PblAnimation {
    if count == 0 || array.is_null() { return pbl_animation_create(); }
    let spawn = pbl_animation_create();
    if !spawn.is_null() {
        unsafe {
            (*spawn).anim_type = AnimType::Spawn;
            for i in 0..count as usize {
                let child = *array.add(i);
                if !child.is_null() {
                    ANIMATIONS.retain(|&a| a != child);
                    (*spawn).children.push(child);
                }
            }
            let max_dur: u32 = (*spawn).children.iter().map(|&c| {
                if c.is_null() { 0 } else { (*c).duration_ms + (*c).delay_ms }
            }).max().unwrap_or(0);
            (*spawn).duration_ms = max_dur;
        }
    }
    spawn
}

// Property animation

#[no_mangle]
pub extern "C" fn pbl_property_animation_create_layer_frame(
    layer: *mut PblLayer, from: *const GRect, to: *const GRect,
) -> *mut PblAnimation {
    let anim = pbl_animation_create();
    if !anim.is_null() {
        unsafe {
            (*anim).is_property_anim = true;
            (*anim).target_layer = layer;
            if !from.is_null() {
                (*anim).from_frame = *from;
            } else if !layer.is_null() {
                (*anim).from_frame = (*layer).frame;
            }
            if !to.is_null() {
                (*anim).to_frame = *to;
            } else if !layer.is_null() {
                (*anim).to_frame = (*layer).frame;
            }
        }
    }
    anim
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_create(
    _impl_ptr: *const u8, _subject: *mut u8, _from: *const u8, _to: *const u8,
) -> *mut PblAnimation {
    pbl_animation_create()
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_create_bounds_origin(
    layer: *mut PblLayer, from: *const GPoint, to: *const GPoint,
) -> *mut PblAnimation {
    let anim = pbl_animation_create();
    if !anim.is_null() && !layer.is_null() {
        unsafe {
            (*anim).is_property_anim = true;
            (*anim).is_bounds_anim = true;
            (*anim).target_layer = layer;
            let bounds = (*layer).bounds;
            if !from.is_null() {
                (*anim).from_frame = GRect { x: (*from).x, y: (*from).y, w: bounds.w, h: bounds.h };
            } else {
                (*anim).from_frame = bounds;
            }
            if !to.is_null() {
                (*anim).to_frame = GRect { x: (*to).x, y: (*to).y, w: bounds.w, h: bounds.h };
            } else {
                (*anim).to_frame = bounds;
            }
        }
    }
    anim
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_destroy(anim: *mut PblAnimation) {
    pbl_animation_destroy(anim);
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_get_animation(anim: *mut PblAnimation) -> *mut PblAnimation {
    anim // Animation IS the property animation in our impl
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_subject(_anim: *mut PblAnimation) -> *mut u8 {
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn pbl_property_animation_from(_anim: *mut PblAnimation, _from: *mut u8) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_to(_anim: *mut PblAnimation, _to: *mut u8) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_update_gpoint(_anim: *mut PblAnimation, _val: u32) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_update_grect(_anim: *mut PblAnimation, _val: *const GRect) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_update_int16(_anim: *mut PblAnimation, _val: i16) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_update_uint32(_anim: *mut PblAnimation, _val: u32) {}

#[no_mangle]
pub extern "C" fn pbl_property_animation_update_gcolor8(_anim: *mut PblAnimation, _val: u8) {}

// Legacy animation (indices 17-28) — delegate to modern
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_create() -> *mut PblAnimation { pbl_animation_create() }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_destroy(a: *mut PblAnimation) { pbl_animation_destroy(a); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_get_context(a: *mut PblAnimation) -> *mut u8 { pbl_animation_get_context(a) }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_is_scheduled(a: *mut PblAnimation) -> bool { pbl_animation_is_scheduled(a) }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_schedule(a: *mut PblAnimation) { pbl_animation_schedule(a); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_curve(a: *mut PblAnimation, c: u8) { pbl_animation_set_curve(a, c); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_delay(a: *mut PblAnimation, d: u32) { pbl_animation_set_delay(a, d); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_duration(a: *mut PblAnimation, d: u32) { pbl_animation_set_duration(a, d); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_handlers(
    a: *mut PblAnimation, s: Option<AnimationStartedHandler>, e: Option<AnimationStoppedHandler>, c: *mut u8,
) { pbl_animation_set_handlers(a, s, e, c); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_implementation(a: *mut PblAnimation, i: *const u8) { pbl_animation_set_implementation(a, i); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_unschedule(a: *mut PblAnimation) { pbl_animation_unschedule(a); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_unschedule_all() { pbl_animation_unschedule_all(); }
#[no_mangle]
pub extern "C" fn pbl_animation_legacy2_set_custom_curve(_a: *mut PblAnimation, _f: usize) {}

// Legacy property animation (indices 198-203)
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_create(
    _impl_ptr: *const u8, _subject: *mut u8, _from: *const u8, _to: *const u8,
) -> *mut PblAnimation { pbl_animation_create() }
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_create_layer_frame(
    l: *mut PblLayer, f: *const GRect, t: *const GRect,
) -> *mut PblAnimation { pbl_property_animation_create_layer_frame(l, f, t) }
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_destroy(a: *mut PblAnimation) { pbl_animation_destroy(a); }
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_update_gpoint(a: *mut PblAnimation, v: u32) { pbl_property_animation_update_gpoint(a, v); }
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_update_grect(a: *mut PblAnimation, v: *const GRect) { pbl_property_animation_update_grect(a, v); }
#[no_mangle]
pub extern "C" fn pbl_property_animation_legacy2_update_int16(a: *mut PblAnimation, v: i16) { pbl_property_animation_update_int16(a, v); }

// ---------------------------------------------------------------------------
// App Message (no-op — we have no phone connection)
// ---------------------------------------------------------------------------

static mut APP_MSG_CONTEXT: usize = 0;
static mut OUTBOX_BUFFER: *mut u8 = std::ptr::null_mut();
static mut APP_MSG_INBOX_RECEIVED: Option<extern "C" fn(*mut u8, *mut u8)> = None;
static mut APP_MSG_INBOX_DROPPED: Option<extern "C" fn(u32, *mut u8)> = None;
static mut APP_MSG_OUTBOX_SENT: Option<extern "C" fn(*mut u8, *mut u8)> = None;
static mut APP_MSG_OUTBOX_FAILED: Option<extern "C" fn(*mut u8, u32, *mut u8)> = None;

pub fn reset_app_message() {
    unsafe {
        APP_MSG_CONTEXT = 0;
        APP_MSG_INBOX_RECEIVED = None;
        APP_MSG_INBOX_DROPPED = None;
        APP_MSG_OUTBOX_SENT = None;
        APP_MSG_OUTBOX_FAILED = None;
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_open(_size_inbound: u32, _size_outbound: u32) -> u32 {
    0 // APP_MSG_OK
}

#[no_mangle]
pub extern "C" fn pbl_app_message_deregister_callbacks() {
    reset_app_message();
}

#[no_mangle]
pub extern "C" fn pbl_app_message_register_inbox_received(
    callback: Option<extern "C" fn(*mut u8, *mut u8)>,
) -> Option<extern "C" fn(*mut u8, *mut u8)> {
    unsafe {
        let old = APP_MSG_INBOX_RECEIVED;
        APP_MSG_INBOX_RECEIVED = callback;
        old
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_register_inbox_dropped(
    callback: Option<extern "C" fn(u32, *mut u8)>,
) -> Option<extern "C" fn(u32, *mut u8)> {
    unsafe {
        let old = APP_MSG_INBOX_DROPPED;
        APP_MSG_INBOX_DROPPED = callback;
        old
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_register_outbox_sent(
    callback: Option<extern "C" fn(*mut u8, *mut u8)>,
) -> Option<extern "C" fn(*mut u8, *mut u8)> {
    unsafe {
        let old = APP_MSG_OUTBOX_SENT;
        APP_MSG_OUTBOX_SENT = callback;
        old
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_register_outbox_failed(
    callback: Option<extern "C" fn(*mut u8, u32, *mut u8)>,
) -> Option<extern "C" fn(*mut u8, u32, *mut u8)> {
    unsafe {
        let old = APP_MSG_OUTBOX_FAILED;
        APP_MSG_OUTBOX_FAILED = callback;
        old
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_set_context(context: *mut u8) -> *mut u8 {
    unsafe {
        let old = APP_MSG_CONTEXT as *mut u8;
        APP_MSG_CONTEXT = context as usize;
        old
    }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_get_context() -> *mut u8 {
    unsafe { APP_MSG_CONTEXT as *mut u8 }
}

#[no_mangle]
pub extern "C" fn pbl_app_message_inbox_size_maximum() -> u32 { 8200 }

#[no_mangle]
pub extern "C" fn pbl_app_message_outbox_size_maximum() -> u32 { 8200 }

/// outbox_begin returns a pointer to a DictionaryIterator.
/// We return a dummy allocation since messages are never actually sent.
#[no_mangle]
pub extern "C" fn pbl_app_message_outbox_begin(iter_out: *mut *mut u8) -> u32 {
    if !iter_out.is_null() {
        // Allocate a small dummy buffer for the DictionaryIterator
        unsafe {
            if OUTBOX_BUFFER.is_null() { OUTBOX_BUFFER = crate::owned::new([0u64; 32]).cast(); }
            OUTBOX_BUFFER.write_bytes(0, 256);
            *iter_out = OUTBOX_BUFFER;
        }
    }
    0 // APP_MSG_OK
}

#[no_mangle]
pub extern "C" fn pbl_app_message_outbox_send() -> u32 {
    0 // APP_MSG_OK
}

// ---------------------------------------------------------------------------
// Dictionary API (no-op stubs — messages never go anywhere)
// ---------------------------------------------------------------------------

/// dict_calc_buffer_size(tuple_count, ...) — variadic function.
/// We only read tuple_count (first arg) and return a generous overestimate.
/// Extra varargs are harmless (caller cleans up the stack per ARM AAPCS).
#[no_mangle]
pub extern "C" fn pbl_dict_calc_buffer_size(tuple_count: u32) -> u32 {
    let size = 256 * tuple_count + 64;
    size
}

#[no_mangle]
pub extern "C" fn pbl_dict_write_begin(_iter: *mut u8, _buffer: *mut u8, _size: u32) -> u32 {
    0 // DICT_OK
}

#[no_mangle]
pub extern "C" fn pbl_dict_write_end(_iter: *mut u8) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn pbl_dict_write_tuplet(_iter: *mut u8, _tuplet: *const u8) -> u32 {
    0 // DICT_OK
}

#[no_mangle]
pub extern "C" fn pbl_dict_write_cstring(_iter: *mut u8, _key: u32, _cstring: *const u8) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_data(_iter: *mut u8, _key: u32, _data: *const u8, _size: u16) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_int(_iter: *mut u8, _key: u32, _value: *const u8, _width: u8, _signed: bool) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_int8(_iter: *mut u8, _key: u32, _value: i8) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_int16(_iter: *mut u8, _key: u32, _value: i16) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_int32(_iter: *mut u8, _key: u32, _value: i32) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_uint8(_iter: *mut u8, _key: u32, _value: u8) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_uint16(_iter: *mut u8, _key: u32, _value: u16) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn pbl_dict_write_uint32(_iter: *mut u8, _key: u32, _value: u32) -> u32 { 0 }

// ---------------------------------------------------------------------------
// AppSync (no-op — wraps AppMessage which we stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_app_sync_init(
    _sync: *mut u8, _buffer: *mut u8, _buffer_size: u32,
    _keys_and_initial_values: *const u8, _count: u8,
    _tuple_changed_callback: usize, _error_callback: usize, _context: *mut u8,
) {}

#[no_mangle]
pub extern "C" fn pbl_app_sync_deinit(_sync: *mut u8) {}

#[no_mangle]
pub extern "C" fn pbl_app_sync_set(_sync: *mut u8, _keys_and_values: *const u8, _count: u8) -> u32 {
    0 // APP_MSG_OK
}

#[no_mangle]
pub extern "C" fn pbl_app_sync_get(_sync: *const u8, _key: u32) -> *const u8 {
    std::ptr::null()
}

// ---------------------------------------------------------------------------
// Menu cell drawing helpers
// ---------------------------------------------------------------------------

/// Draw a basic menu cell with title, optional subtitle, optional icon
#[no_mangle]
pub extern "C" fn pbl_menu_cell_basic_draw(
    _ctx: *mut PblGContext, cell_layer: *mut PblLayer,
    title: *const std::ffi::c_char, subtitle: *const std::ffi::c_char,
    _icon: *const u8,
) {
    let fb = match fb() { Some(f) => f, None => return };
    let frame = if cell_layer.is_null() {
        GRect { x: 0, y: 0, w: 180, h: 44 }
    } else {
        unsafe { (*cell_layer).frame }
    };

    // Draw title
    if !title.is_null() {
        if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(title) }.to_str() {
            let title_box = GRect { x: frame.x + 5, y: frame.y + 2, w: frame.w - 10, h: 20 };
            font::draw_text(fb, s, font::encode_font_handle(font::GOTHIC_24_BOLD_INDEX), title_box, 0, gcolor::colors::WHITE);
        }
    }

    // Draw subtitle
    if !subtitle.is_null() {
        if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(subtitle) }.to_str() {
            if !s.is_empty() {
                let sub_box = GRect { x: frame.x + 5, y: frame.y + 24, w: frame.w - 10, h: 18 };
                font::draw_text(fb, s, font::encode_font_handle(font::GOTHIC_18_INDEX), sub_box, 0, gcolor::colors::WHITE);
            }
        }
    }
}

/// Draw a title-only menu cell
#[no_mangle]
pub extern "C" fn pbl_menu_cell_title_draw(
    ctx: *mut PblGContext, cell_layer: *mut PblLayer,
    title: *const std::ffi::c_char,
) {
    pbl_menu_cell_basic_draw(ctx, cell_layer, title, std::ptr::null(), std::ptr::null());
}

/// Draw a section header cell
#[no_mangle]
pub extern "C" fn pbl_menu_cell_basic_header_draw(
    _ctx: *mut PblGContext, cell_layer: *mut PblLayer,
    title: *const std::ffi::c_char,
) {
    let fb = match fb() { Some(f) => f, None => return };
    let frame = if cell_layer.is_null() {
        GRect { x: 0, y: 0, w: 180, h: 16 }
    } else {
        unsafe { (*cell_layer).frame }
    };
    if !title.is_null() {
        if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(title) }.to_str() {
            let box_ = GRect { x: frame.x + 5, y: frame.y, w: frame.w - 10, h: frame.h };
            font::draw_text(fb, s, font::encode_font_handle(font::GOTHIC_14_INDEX), box_, 0, gcolor::colors::WHITE);
        }
    }
}

/// Compare two MenuIndex values. Returns negative if a < b, 0 if equal, positive if a > b.
#[no_mangle]
pub extern "C" fn pbl_menu_index_compare(a: *const MenuIndex, b: *const MenuIndex) -> i16 {
    if a.is_null() || b.is_null() { return 0; }
    unsafe {
        let sa = (*a).section as i32;
        let sb = (*b).section as i32;
        if sa != sb { return (sa - sb) as i16; }
        ((*a).row as i32 - (*b).row as i32) as i16
    }
}

/// Check if a cell layer is the highlighted (selected) row
#[no_mangle]
pub extern "C" fn pbl_menu_cell_layer_is_highlighted(_cell_layer: *const PblLayer) -> bool {
    false // We don't track highlight state per-cell-layer
}

// ---------------------------------------------------------------------------
// Dictionary read/find API
// ---------------------------------------------------------------------------

/// Packed Tuple header: key(4) + type(1) + length(2) = 7 bytes, then value data
const TUPLE_HEADER_SIZE: u32 = 7;

/// Find a Tuple by key in a serialized dictionary buffer.
/// Returns pointer to the Tuple, or null if not found.
#[no_mangle]
pub extern "C" fn pbl_dict_find(iter: *const u8, key: u32) -> *const u8 {
    if iter.is_null() { return std::ptr::null(); }
    // DictionaryIterator: { dictionary, end, cursor } — we walk the raw buffer
    // In our stub implementation, iter points to a DictionaryIterator on the stack
    // We can't easily walk emulated memory here, so return null
    std::ptr::null()
}

/// No-op merge
#[no_mangle]
pub extern "C" fn pbl_dict_merge(
    _dest: *mut u8, _dest_max_size: *mut u32, _source: *mut u8,
    _update_existing: bool, _callback: *const u8, _context: *mut u8,
) -> u32 {
    0 // DICT_OK
}

/// Initialize a DictionaryIterator to read from a buffer
#[no_mangle]
pub extern "C" fn pbl_dict_read_begin_from_buffer(
    _iter: *mut u8, _buffer: *const u8, _size: u16,
) -> *const u8 {
    std::ptr::null() // Return null Tuple (empty iteration)
}

/// Return the first Tuple in iteration
#[no_mangle]
pub extern "C" fn pbl_dict_read_first(_iter: *mut u8) -> *const u8 {
    std::ptr::null()
}

/// Return the next Tuple in iteration
#[no_mangle]
pub extern "C" fn pbl_dict_read_next(_iter: *mut u8) -> *const u8 {
    std::ptr::null()
}

// dict_calc_buffer_size is defined above (app_sync section)

/// Calculate buffer size from Tuplet array
#[no_mangle]
pub extern "C" fn pbl_dict_calc_buffer_size_from_tuplets(_tuplets: *const u8, tuplets_count: u8) -> u32 {
    (tuplets_count as u32) * 256 + 64
}

/// Serialize tuplets via callback — no-op
#[no_mangle]
pub extern "C" fn pbl_dict_serialize_tuplets(
    _callback: *const u8, _context: *mut u8, _tuplets: *const u8, _count: u8,
) -> u32 {
    0 // DICT_OK
}

/// Serialize tuplets to buffer — no-op
#[no_mangle]
pub extern "C" fn pbl_dict_serialize_tuplets_to_buffer(
    _tuplets: *const u8, _count: u8, _buffer: *mut u8, _size: *mut u32,
) -> u32 {
    0 // DICT_OK
}

/// Serialize tuplets to buffer with iterator — no-op
#[no_mangle]
pub extern "C" fn pbl_dict_serialize_tuplets_to_buffer_with_iter(
    _iter: *mut u8, _tuplets: *const u8, _count: u8, _buffer: *mut u8, _size: *mut u32,
) -> u32 {
    0 // DICT_OK
}

// ---------------------------------------------------------------------------
// Inverter layer (deprecated in SDK 3+, but some legacy apps use it)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct PblInverterLayer {
    pub layer: PblLayer,
}

#[no_mangle]
pub extern "C" fn pbl_inverter_layer_create(frame: GRect) -> *mut PblInverterLayer {
    crate::owned::new(PblInverterLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: frame.w, h: frame.h },
            frame,
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: None,
        },
    })
}

#[no_mangle]
pub extern "C" fn pbl_inverter_layer_destroy(il: *mut PblInverterLayer) {
    dispose_layer(il.cast());
}

#[no_mangle]
pub extern "C" fn pbl_inverter_layer_get_layer(il: *mut PblInverterLayer) -> *mut PblLayer {
    if il.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*il).layer as *mut PblLayer }
}

// ---------------------------------------------------------------------------
// Rot bitmap layer
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct PblRotBitmapLayer {
    pub layer: PblLayer,
    pub bitmap: *mut u8,
    pub angle: i32,
    pub src_ic: GPoint,
    pub compositing_mode: u8,
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_create(bitmap: *mut u8) -> *mut PblRotBitmapLayer {
    crate::owned::new(PblRotBitmapLayer {
        layer: PblLayer {
            bounds: GRect { x: 0, y: 0, w: 180, h: 180 },
            frame: GRect { x: 0, y: 0, w: 180, h: 180 },
            flags: 1,
            next_sibling: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            update_proc: None,
        },
        bitmap,
        angle: 0,
        src_ic: GPoint { x: 0, y: 0 },
        compositing_mode: 0,
    })
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_destroy(rb: *mut PblRotBitmapLayer) {
    dispose_layer(rb.cast());
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_get_layer(rb: *mut PblRotBitmapLayer) -> *mut PblLayer {
    if rb.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*rb).layer as *mut PblLayer }
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_increment_angle(rb: *mut PblRotBitmapLayer, angle_change: i32) {
    if !rb.is_null() { unsafe { (*rb).angle += angle_change; } }
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_set_angle(rb: *mut PblRotBitmapLayer, angle: i32) {
    if !rb.is_null() { unsafe { (*rb).angle = angle; } }
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_layer_set_corner_clip_color(rb: *mut PblRotBitmapLayer, _color: u8) {
    // No-op — we don't render rotated bitmaps
    let _ = rb;
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_set_compositing_mode(rb: *mut PblRotBitmapLayer, mode: u8) {
    if !rb.is_null() { unsafe { (*rb).compositing_mode = mode; } }
}

#[no_mangle]
pub extern "C" fn pbl_rot_bitmap_set_src_ic(rb: *mut PblRotBitmapLayer, ic: GPoint) {
    if !rb.is_null() { unsafe { (*rb).src_ic = ic; } }
}

// ---------------------------------------------------------------------------
// Compass service (stub — no compass on hoki)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_compass_service_peek(data: *mut crate::compass::Heading) -> i32 { crate::compass::peek(data) }
#[no_mangle]
pub extern "C" fn pbl_compass_service_set_heading_filter(filter: i32) -> i32 { crate::compass::set_filter(filter) }
#[no_mangle]
pub extern "C" fn pbl_compass_service_subscribe(handler: Option<extern "C" fn(crate::compass::Heading)>) { crate::compass::subscribe(handler); }
#[no_mangle]
pub extern "C" fn pbl_compass_service_unsubscribe() { crate::compass::reset(); }

// ---------------------------------------------------------------------------
// Content indicator (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_content_indicator_create() -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_content_indicator_destroy(_ci: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_content_indicator_configure_direction(_ci: *mut u8, _dir: u8, _config: *const u8) -> bool { true }
#[no_mangle]
pub extern "C" fn pbl_content_indicator_get_content_available(_ci: *mut u8, _dir: u8) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_content_indicator_set_content_available(_ci: *mut u8, _dir: u8, _available: bool) {}

// ---------------------------------------------------------------------------
// Launch reason, args
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_launch_reason() -> u32 { 0 } // APP_LAUNCH_SYSTEM
#[no_mangle]
pub extern "C" fn pbl_launch_get_args() -> u32 { 0 }

// ---------------------------------------------------------------------------
// UUID
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_uuid_equal(a: *const u8, b: *const u8) -> bool {
    if a.is_null() || b.is_null() { return false; }
    unsafe { std::slice::from_raw_parts(a, 16) == std::slice::from_raw_parts(b, 16) }
}

#[no_mangle]
pub extern "C" fn pbl_uuid_to_string(uuid: *const u8, buffer: *mut u8) {
    if uuid.is_null() || buffer.is_null() { return; }
    // Write "00000000-0000-0000-0000-000000000000\0"
    let u = unsafe { std::slice::from_raw_parts(uuid, 16) };
    let s = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0],u[1],u[2],u[3],u[4],u[5],u[6],u[7],u[8],u[9],u[10],u[11],u[12],u[13],u[14],u[15]
    );
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buffer, s.len());
        *buffer.add(s.len()) = 0;
    }
}

// ---------------------------------------------------------------------------
// Heap info
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_heap_bytes_free() -> u32 { 256 * 1024 } // Report 256KB free
#[no_mangle]
pub extern "C" fn pbl_heap_bytes_used() -> u32 { 64 * 1024 } // Report 64KB used

// ---------------------------------------------------------------------------
// psleep
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_psleep(millis: i32) {
    if millis > 0 {
        std::thread::sleep(std::time::Duration::from_millis(millis as u64));
    }
}

// ---------------------------------------------------------------------------
// Watch info
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_watch_info_get_firmware_version() -> u32 {
    // Return version 4.3.0 packed as: major(8) | minor(8) | patch(8) | suffix(8)
    (4 << 24) | (3 << 16) | (0 << 8)
}

// ---------------------------------------------------------------------------
// Clock timezone
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_clock_is_timezone_set() -> bool { true }

// ---------------------------------------------------------------------------
// Exit reason
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_exit_reason_set(_reason: u32) {}

// ---------------------------------------------------------------------------
// Preferred content size / display duration
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_preferred_result_display_duration() -> u32 { 2000 } // 2 seconds
#[no_mangle]
pub extern "C" fn pbl_preferred_content_size() -> u32 { 1 } // PreferredContentSizeMedium

// ---------------------------------------------------------------------------
// Click helpers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_window_single_repeating_click_subscribe(
    _button_id: u8, _repeat_interval_ms: u16, _handler: *const u8,
) {
    // No-op — repeating clicks not yet implemented
}

#[no_mangle]
pub extern "C" fn pbl_window_get_click_config_context(window: *mut PblWindow) -> *mut u8 {
    unsafe { CLICK_CONFIGS.iter().find(|c| c.window == window as usize).map_or(window.cast(), |c| c.context as *mut u8) }
}

#[no_mangle]
pub extern "C" fn pbl_click_recognizer_is_repeating(_recognizer: *const u8) -> bool { false }

// ---------------------------------------------------------------------------
// Data logging (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_data_logging_create(_tag: u32, _item_type: u8, _item_length: u16, _buffered: bool) -> *mut u8 {
    // Return a non-null fake handle
    1 as *mut u8
}
#[no_mangle]
pub extern "C" fn pbl_data_logging_finish(_session: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_data_logging_log(_session: *mut u8, _data: *const u8, _num_items: u32) -> u32 { 0 }

// ---------------------------------------------------------------------------
// Wakeup (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_wakeup_cancel(_wakeup_id: i32) {}
#[no_mangle]
pub extern "C" fn pbl_wakeup_cancel_all() {}
#[no_mangle]
pub extern "C" fn pbl_wakeup_get_launch_event(_wakeup_id: *mut i32, _cookie: *mut i32) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_wakeup_query(_wakeup_id: i32, _scheduled_time: *mut i32) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_wakeup_schedule(_timestamp: i32, _cookie: i32, _notify_if_missed: bool) -> i32 { -1 }
#[no_mangle]
pub extern "C" fn pbl_wakeup_service_subscribe(_handler: *const u8) {}

// ---------------------------------------------------------------------------
// Dictation session (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_dictation_session_create(_size: u32, _callback: *const u8, _context: *mut u8) -> *mut u8 {
    std::ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn pbl_dictation_session_destroy(_session: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_dictation_session_enable_confirmation(_session: *mut u8, _enabled: bool) {}
#[no_mangle]
pub extern "C" fn pbl_dictation_session_start(_session: *mut u8) -> i32 { -1 } // error
#[no_mangle]
pub extern "C" fn pbl_dictation_session_stop(_session: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_dictation_session_enable_error_dialogs(_session: *mut u8, _enabled: bool) {}

// ---------------------------------------------------------------------------
// Smartstrap (stub — no hardware)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_begin_write(_attr: *mut u8, _buf: *mut *mut u8, _len: *mut u32) -> i32 { -1 }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_create(_service: u16, _attr_id: u16, _length: u16) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_destroy(_attr: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_end_write(_attr: *mut u8, _length: u32, _request_read: bool) -> i32 { -1 }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_get_attribute_id(_attr: *const u8) -> u16 { 0 }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_get_service_id(_attr: *const u8) -> u16 { 0 }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_attribute_read(_attr: *mut u8) -> i32 { -1 }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_service_is_available(_service: u16) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_smartstrap_set_timeout(_timeout_ms: u16) {}
#[no_mangle]
pub extern "C" fn pbl_smartstrap_subscribe(_handlers: *const u8) {}
#[no_mangle]
pub extern "C" fn pbl_smartstrap_unsubscribe() {}

// ---------------------------------------------------------------------------
// App comm / worker (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_app_comm_get_sniff_interval() -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_app_comm_set_sniff_interval(_interval: u32) {}
#[no_mangle]
pub extern "C" fn pbl_app_worker_is_running() -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_app_worker_kill() -> i32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_app_worker_launch() -> i32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_app_worker_message_subscribe(_handler: *const u8) -> bool { true }
#[no_mangle]
pub extern "C" fn pbl_app_worker_message_unsubscribe() -> bool { true }
#[no_mangle]
pub extern "C" fn pbl_app_worker_send_message(_type: u8, _data: *const u8) {}
#[no_mangle]
pub extern "C" fn pbl_worker_event_loop() {}
#[no_mangle]
pub extern "C" fn pbl_worker_launch_app() {}

// ---------------------------------------------------------------------------
// Health service (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_health_service_activities_iterate(_activity: u32, _time_start: i32, _time_end: i32, _direction: u32, _callback: *const u8, _context: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_health_service_any_activity_accessible(_activity: u32, _time_start: i32, _time_end: i32) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_health_service_events_unsubscribe() {}
#[no_mangle]
pub extern "C" fn pbl_health_service_get_minute_history(_data: *mut u8, _max: u32, _time_start: *mut i32, _time_end: *mut i32) -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_health_service_peek_current_activities() -> u32 { 0 }
// health_service functions defined above (health service section)
// New additions only:
#[no_mangle]
pub extern "C" fn pbl_health_service_register_metric_alert(_metric: u32, _alert: u32) -> i32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_health_service_set_heart_rate_sample_period(_seconds: u16) {}
#[no_mangle]
pub extern "C" fn pbl_health_service_sum(_metric: u32, _time_start: i32, _time_end: i32) -> i32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_health_service_metric_averaged_accessible(_metric: u32, _time_start: i32, _time_end: i32, _scope: u32) -> i32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_health_service_sum_averaged(_metric: u32, _time_start: i32, _time_end: i32, _scope: u32) -> i32 { 0 }

// ---------------------------------------------------------------------------
// Accel tap (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_accel_tap_service_unsubscribe() {}
#[no_mangle]
pub extern "C" fn pbl_accel_raw_data_service_subscribe(_samples_per_update: u32, _handler: *const u8) {}

// ---------------------------------------------------------------------------
// Action menu (stub — full-screen action picker not implemented)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_action_menu_close(_menu: *mut u8, _animated: bool) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_freeze(_menu: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_get_context(_menu: *mut u8) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_get_root_level(_menu: *mut u8) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_hierarchy_destroy(_root: *mut u8, _each_cb: *const u8, _context: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_level_add_action(_level: *mut u8, _label: *const u8, _cb: *const u8, _context: *mut u8) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_level_add_child(_parent: *mut u8, _child: *mut u8, _label: *const u8) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_level_create(_max_items: u16) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_level_set_display_mode(_level: *mut u8, _mode: u8) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_open(_config: *const u8) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_action_menu_set_align(_menu: *mut u8, _align: u8) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_set_result_window(_menu: *mut u8, _window: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_action_menu_unfreeze(_menu: *mut u8) {}

// ---------------------------------------------------------------------------
// GBitmap extended functions (stubs)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_blank_2bit(_size_w: i16, _size_h: i16) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_blank_with_palette(_size_w: i16, _size_h: i16, _format: u8, _palette: *const u8, _free_on_destroy: bool) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_set_bounds(_bmp: *mut u8, _bounds: GRect) {}
#[no_mangle]
pub extern "C" fn pbl_gbitmap_set_data(_bmp: *mut u8, _data: *mut u8, _format: u8, _row_size: u16) {}
// gbitmap_set_palette defined above (GBitmap section)

// ---------------------------------------------------------------------------
// GBitmap sequence (animated PNG — stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_create_with_resource(_resource_id: u32) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_destroy(_seq: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_get_bitmap_size(_seq: *mut u8) -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_get_current_frame_idx(_seq: *mut u8) -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_get_play_count(_seq: *mut u8) -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_get_total_num_frames(_seq: *mut u8) -> u32 { 0 }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_restart(_seq: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_set_play_count(_seq: *mut u8, _count: u32) {}
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_update_bitmap_by_elapsed(_seq: *mut u8, _bmp: *mut u8, _elapsed_ms: u32) -> bool { false }
#[no_mangle]
pub extern "C" fn pbl_gbitmap_sequence_update_bitmap_next_frame(_seq: *mut u8, _bmp: *mut u8, _delay_ms: *mut u32) -> bool { false }

// ---------------------------------------------------------------------------
// Graphics text attributes (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_create() -> *mut u8 { std::ptr::null_mut() }
#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_destroy(_attrs: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_enable_paging(_attrs: *mut u8, _origin: GPoint, _frame: GRect) {}
#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_enable_screen_text_flow(_attrs: *mut u8, _inset: u8) {}
#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_restore_default_paging(_attrs: *mut u8) {}
#[no_mangle]
pub extern "C" fn pbl_graphics_text_attributes_restore_default_text_flow(_attrs: *mut u8) {}

// ---------------------------------------------------------------------------
// Graphics draw rotated bitmap (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_graphics_draw_rotated_bitmap(
    _ctx: *mut PblGContext, _bmp: *const u8, _src_ic: GPoint, _angle: i32, _dest_pt: GPoint,
) {}

// ---------------------------------------------------------------------------
// GDraw Command (PDC vector graphics)
// ---------------------------------------------------------------------------

/// GDrawCommandType
const GDRAW_CMD_TYPE_PATH: u8 = 1;
const GDRAW_CMD_TYPE_CIRCLE: u8 = 2;
const GDRAW_CMD_TYPE_PRECISE_PATH: u8 = 3;

/// A single draw command (path or circle)
pub struct GDrawCommand {
    pub cmd_type: u8,
    pub hidden: bool,
    pub stroke_color: u8, // GColor8
    pub stroke_width: u8,
    pub fill_color: u8,   // GColor8
    pub path_open: bool,
    pub radius: u16,
    pub points: Vec<GPoint>,
}

/// A list of draw commands
pub struct GDrawCommandList {
    pub commands: Vec<GDrawCommand>,
}

/// A static vector image (PDCI format)
pub struct GDrawCommandImage {
    pub size: GSize,
    pub command_list: GDrawCommandList,
}

/// A single animation frame
pub struct GDrawCommandFrame {
    pub duration: u16,
    pub command_list: GDrawCommandList,
}

/// An animated sequence (PDCS format)
pub struct GDrawCommandSequence {
    pub size: GSize,
    pub play_count: u16,
    pub frames: Vec<GDrawCommandFrame>,
}

/// Parse a GDrawCommand from packed binary data. Returns (command, bytes_consumed).
fn parse_gdraw_command(data: &[u8]) -> Option<(GDrawCommand, usize)> {
    if data.len() < 8 { return None; }
    let cmd_type = data[0];
    let hidden = (data[1] & 1) != 0;
    let stroke_color = data[2];
    let stroke_width = data[3];
    let fill_color = data[4];
    let path_open = data[5] != 0;
    let radius = u16::from_le_bytes([data[5], data[6]]);
    let num_points = u16::from_le_bytes([data[7], data[8]]) as usize;

    // Header is 9 bytes, then num_points * 4 bytes (GPoint = 2xi16)
    let point_size = if cmd_type == GDRAW_CMD_TYPE_PRECISE_PATH { 4 } else { 4 }; // GPoint = 4 bytes, GPointPrecise = 4 bytes
    let total = 9 + num_points * point_size;
    if data.len() < total { return None; }

    let mut points = Vec::with_capacity(num_points);
    for i in 0..num_points {
        let off = 9 + i * 4;
        let x = i16::from_le_bytes([data[off], data[off + 1]]);
        let y = i16::from_le_bytes([data[off + 2], data[off + 3]]);
        if cmd_type == GDRAW_CMD_TYPE_PRECISE_PATH {
            // GPointPrecise uses 13.3 fixed-point — shift right by 3
            points.push(GPoint { x: x >> 3, y: y >> 3 });
        } else {
            points.push(GPoint { x, y });
        }
    }

    Some((GDrawCommand {
        cmd_type,
        hidden,
        stroke_color,
        stroke_width,
        fill_color,
        path_open,
        radius,
        points,
    }, total))
}

/// Parse a GDrawCommandList from packed binary data.
fn parse_gdraw_command_list(data: &[u8]) -> Option<(GDrawCommandList, usize)> {
    if data.len() < 2 { return None; }
    let num_commands = u16::from_le_bytes([data[0], data[1]]) as usize;
    let mut offset = 2;
    let mut commands = Vec::with_capacity(num_commands);
    for _ in 0..num_commands {
        if offset >= data.len() { break; }
        let (cmd, consumed) = parse_gdraw_command(&data[offset..])?;
        commands.push(cmd);
        offset += consumed;
    }
    Some((GDrawCommandList { commands }, offset))
}

/// Parse a GDrawCommandImage from PDCI resource data (after 8-byte PDCI header).
fn parse_gdraw_command_image(data: &[u8]) -> Option<GDrawCommandImage> {
    // Skip PDCI signature (4 bytes) + size (4 bytes) if present
    let start = if data.len() >= 8 && &data[0..4] == b"PDCI" { 8 } else { 0 };
    let d = &data[start..];
    if d.len() < 6 { return None; }
    let _version = d[0];
    let _reserved = d[1];
    let w = i16::from_le_bytes([d[2], d[3]]);
    let h = i16::from_le_bytes([d[4], d[5]]);
    let (command_list, _) = parse_gdraw_command_list(&d[6..])?;
    Some(GDrawCommandImage {
        size: GSize { w, h },
        command_list,
    })
}

/// Parse a GDrawCommandSequence from PDCS resource data.
fn parse_gdraw_command_sequence(data: &[u8]) -> Option<GDrawCommandSequence> {
    let start = if data.len() >= 8 && &data[0..4] == b"PDCS" { 8 } else { 0 };
    let d = &data[start..];
    if d.len() < 10 { return None; }
    let _version = d[0];
    let _reserved = d[1];
    let w = i16::from_le_bytes([d[2], d[3]]);
    let h = i16::from_le_bytes([d[4], d[5]]);
    let play_count = u16::from_le_bytes([d[6], d[7]]);
    let num_frames = u16::from_le_bytes([d[8], d[9]]) as usize;

    let mut offset = 10;
    let mut frames = Vec::with_capacity(num_frames);
    for _ in 0..num_frames {
        if offset + 2 > d.len() { break; }
        let duration = u16::from_le_bytes([d[offset], d[offset + 1]]);
        offset += 2;
        if let Some((command_list, consumed)) = parse_gdraw_command_list(&d[offset..]) {
            frames.push(GDrawCommandFrame { duration, command_list });
            offset += consumed;
        } else {
            break;
        }
    }

    Some(GDrawCommandSequence {
        size: GSize { w, h },
        play_count,
        frames,
    })
}

/// Draw a single GDrawCommand to the framebuffer at the given offset.
fn draw_gdraw_command(fb: &mut [u8], cmd: &GDrawCommand, offset: GPoint) {
    if cmd.hidden { return; }

    match cmd.cmd_type {
        GDRAW_CMD_TYPE_CIRCLE => {
            if cmd.points.is_empty() { return; }
            let cx = cmd.points[0].x + offset.x;
            let cy = cmd.points[0].y + offset.y;
            let r = cmd.radius as i16;

            // Fill circle
            if cmd.fill_color != 0 && cmd.fill_color != gcolor::colors::CLEAR {
                for dy in -r..=r {
                    let dx_max = ((r as f32).powi(2) - (dy as f32).powi(2)).sqrt() as i16;
                    for dx in -dx_max..=dx_max {
                        set_pixel(fb, cx + dx, cy + dy, cmd.fill_color);
                    }
                }
            }
            // Stroke circle (midpoint algorithm)
            if cmd.stroke_color != 0 && cmd.stroke_color != gcolor::colors::CLEAR && cmd.stroke_width > 0 {
                let mut x = r;
                let mut y: i16 = 0;
                let mut err = 1 - r;
                while x >= y {
                    for &(px, py) in &[(cx+x,cy+y),(cx-x,cy+y),(cx+x,cy-y),(cx-x,cy-y),
                                       (cx+y,cy+x),(cx-y,cy+x),(cx+y,cy-x),(cx-y,cy-x)] {
                        set_pixel(fb, px, py, cmd.stroke_color);
                    }
                    y += 1;
                    if err < 0 {
                        err += 2 * y + 1;
                    } else {
                        x -= 1;
                        err += 2 * (y - x) + 1;
                    }
                }
            }
        }
        GDRAW_CMD_TYPE_PATH | GDRAW_CMD_TYPE_PRECISE_PATH => {
            if cmd.points.len() < 2 { return; }
            let pts: Vec<(i16, i16)> = cmd.points.iter()
                .map(|p| (p.x + offset.x, p.y + offset.y))
                .collect();

            // Fill closed path (scanline)
            if !cmd.path_open && cmd.fill_color != 0 && cmd.fill_color != gcolor::colors::CLEAR {
                let min_y = pts.iter().map(|p| p.1).min().unwrap_or(0).max(0);
                let max_y = pts.iter().map(|p| p.1).max().unwrap_or(0).min(179);
                for y in min_y..=max_y {
                    let mut intersections = Vec::new();
                    let n = pts.len();
                    for i in 0..n {
                        let j = (i + 1) % n;
                        let (x0, y0) = pts[i];
                        let (x1, y1) = pts[j];
                        if (y0 <= y && y1 > y) || (y1 <= y && y0 > y) {
                            let t = (y - y0) as f32 / (y1 - y0) as f32;
                            intersections.push((x0 as f32 + t * (x1 - x0) as f32) as i16);
                        }
                    }
                    intersections.sort();
                    for pair in intersections.chunks(2) {
                        if pair.len() == 2 {
                            for x in pair[0].max(0)..=pair[1].min(179) {
                                set_pixel(fb, x, y, cmd.fill_color);
                            }
                        }
                    }
                }
            }

            // Stroke path (Bresenham lines)
            if cmd.stroke_color != 0 && cmd.stroke_color != gcolor::colors::CLEAR && cmd.stroke_width > 0 {
                let n = if cmd.path_open { pts.len() - 1 } else { pts.len() };
                for i in 0..n {
                    let j = (i + 1) % pts.len();
                    draw_line_bresenham(fb, pts[i].0, pts[i].1, pts[j].0, pts[j].1, cmd.stroke_color);
                }
            }
        }
        _ => {}
    }
}

/// Bresenham line drawing
fn draw_line_bresenham(fb: &mut [u8], x0: i16, y0: i16, x1: i16, y1: i16, color: u8) {
    let mut x0 = x0 as i32;
    let mut y0 = y0 as i32;
    let x1 = x1 as i32;
    let y1 = y1 as i32;
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        set_pixel(fb, x0 as i16, y0 as i16, color);
        if x0 == x1 && y0 == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x0 += sx; }
        if e2 <= dx { err += dx; y0 += sy; }
    }
}

/// Draw all commands in a command list
fn draw_gdraw_command_list(fb: &mut [u8], list: &GDrawCommandList, offset: GPoint) {
    for cmd in &list.commands {
        draw_gdraw_command(fb, cmd, offset);
    }
}

// --- Public API ---

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_create_with_resource(resource_id: u32) -> *mut GDrawCommandImage {
    match resource_get_data(resource_id) {
        Some(data) => {
            match parse_gdraw_command_image(&data) {
                Some(img) => crate::owned::new(img),
                None => {
                    eprintln!("[pdc] Failed to parse PDCI resource {}", resource_id);
                    std::ptr::null_mut()
                }
            }
        }
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_clone(image: *mut GDrawCommandImage) -> *mut GDrawCommandImage {
    if image.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let src = &*image;
        let clone = GDrawCommandImage {
            size: src.size,
            command_list: GDrawCommandList {
                commands: src.command_list.commands.iter().map(|c| GDrawCommand {
                    cmd_type: c.cmd_type, hidden: c.hidden,
                    stroke_color: c.stroke_color, stroke_width: c.stroke_width,
                    fill_color: c.fill_color, path_open: c.path_open,
                    radius: c.radius, points: c.points.clone(),
                }).collect(),
            },
        };
        crate::owned::new(clone)
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_destroy(image: *mut GDrawCommandImage) {
    if !image.is_null() { unsafe { crate::owned::release(image); } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_draw(
    _ctx: *mut PblGContext, image: *mut GDrawCommandImage, offset: GPoint,
) {
    if image.is_null() { return; }
    if let Some(fb) = fb() {
        let img = unsafe { &*image };
        draw_gdraw_command_list(fb, &img.command_list, offset);
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_get_bounds_size(image: *mut GDrawCommandImage) -> GSize {
    if image.is_null() { return GSize { w: 0, h: 0 }; }
    unsafe { (*image).size }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_set_bounds_size(image: *mut GDrawCommandImage, size: GSize) {
    if !image.is_null() { unsafe { (*image).size = size; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_image_get_command_list(image: *mut GDrawCommandImage) -> *mut GDrawCommandList {
    if image.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*image).command_list as *mut GDrawCommandList }
}

// --- Command List API ---

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_list_draw(
    _ctx: *mut PblGContext, list: *mut GDrawCommandList,
) {
    if list.is_null() { return; }
    if let Some(fb) = fb() {
        let l = unsafe { &*list };
        draw_gdraw_command_list(fb, l, GPoint { x: 0, y: 0 });
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_list_get_num_commands(list: *mut GDrawCommandList) -> u32 {
    if list.is_null() { return 0; }
    unsafe { (*list).commands.len() as u32 }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_list_get_command(list: *mut GDrawCommandList, idx: u16) -> *mut GDrawCommand {
    if list.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let cmds = &mut (*list).commands;
        if (idx as usize) < cmds.len() {
            &mut cmds[idx as usize] as *mut GDrawCommand
        } else {
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_list_iterate(
    _list: *mut GDrawCommandList, _callback: *const u8, _context: *mut u8,
) {
    // No-op — callbacks require emulated ARM execution
}

// --- Single Command API ---

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_draw(
    _ctx: *mut PblGContext, command: *mut GDrawCommand,
) {
    if command.is_null() { return; }
    if let Some(fb) = fb() {
        let cmd = unsafe { &*command };
        draw_gdraw_command(fb, cmd, GPoint { x: 0, y: 0 });
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_type(cmd: *mut GDrawCommand) -> u8 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).cmd_type }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_fill_color(cmd: *mut GDrawCommand) -> u8 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).fill_color }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_fill_color(cmd: *mut GDrawCommand, color: u8) {
    if !cmd.is_null() { unsafe { (*cmd).fill_color = color; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_stroke_color(cmd: *mut GDrawCommand) -> u8 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).stroke_color }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_stroke_color(cmd: *mut GDrawCommand, color: u8) {
    if !cmd.is_null() { unsafe { (*cmd).stroke_color = color; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_stroke_width(cmd: *mut GDrawCommand) -> u8 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).stroke_width }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_stroke_width(cmd: *mut GDrawCommand, width: u8) {
    if !cmd.is_null() { unsafe { (*cmd).stroke_width = width; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_num_points(cmd: *mut GDrawCommand) -> u16 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).points.len() as u16 }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_point(cmd: *mut GDrawCommand, idx: u16) -> GPoint {
    if cmd.is_null() { return GPoint { x: 0, y: 0 }; }
    unsafe {
        let pts = &(*cmd).points;
        if (idx as usize) < pts.len() { pts[idx as usize] } else { GPoint { x: 0, y: 0 } }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_point(cmd: *mut GDrawCommand, idx: u16, point: GPoint) {
    if cmd.is_null() { return; }
    unsafe {
        let pts = &mut (*cmd).points;
        if (idx as usize) < pts.len() { pts[idx as usize] = point; }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_radius(cmd: *mut GDrawCommand) -> u16 {
    if cmd.is_null() { return 0; }
    unsafe { (*cmd).radius }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_radius(cmd: *mut GDrawCommand, radius: u16) {
    if !cmd.is_null() { unsafe { (*cmd).radius = radius; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_path_open(cmd: *mut GDrawCommand) -> bool {
    if cmd.is_null() { return false; }
    unsafe { (*cmd).path_open }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_path_open(cmd: *mut GDrawCommand, open: bool) {
    if !cmd.is_null() { unsafe { (*cmd).path_open = open; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_get_hidden(cmd: *mut GDrawCommand) -> bool {
    if cmd.is_null() { return false; }
    unsafe { (*cmd).hidden }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_set_hidden(cmd: *mut GDrawCommand, hidden: bool) {
    if !cmd.is_null() { unsafe { (*cmd).hidden = hidden; } }
}

// --- Frame API ---

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_frame_draw(
    _ctx: *mut PblGContext, _sequence: *mut GDrawCommandSequence,
    frame: *mut GDrawCommandFrame, offset: GPoint,
) {
    if frame.is_null() { return; }
    if let Some(fb) = fb() {
        let f = unsafe { &*frame };
        draw_gdraw_command_list(fb, &f.command_list, offset);
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_frame_get_duration(frame: *mut GDrawCommandFrame) -> u32 {
    if frame.is_null() { return 0; }
    unsafe { (*frame).duration as u32 }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_frame_set_duration(frame: *mut GDrawCommandFrame, duration: u32) {
    if !frame.is_null() { unsafe { (*frame).duration = duration as u16; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_frame_get_command_list(frame: *mut GDrawCommandFrame) -> *mut GDrawCommandList {
    if frame.is_null() { return std::ptr::null_mut(); }
    unsafe { &mut (*frame).command_list as *mut GDrawCommandList }
}

// --- Sequence API ---

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_create_with_resource(resource_id: u32) -> *mut GDrawCommandSequence {
    match resource_get_data(resource_id) {
        Some(data) => {
            match parse_gdraw_command_sequence(&data) {
                Some(seq) => crate::owned::new(seq),
                None => {
                    eprintln!("[pdc] Failed to parse PDCS resource {}", resource_id);
                    std::ptr::null_mut()
                }
            }
        }
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_clone(seq: *mut GDrawCommandSequence) -> *mut GDrawCommandSequence {
    if seq.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let src = &*seq;
        let clone = GDrawCommandSequence {
            size: src.size,
            play_count: src.play_count,
            frames: src.frames.iter().map(|f| GDrawCommandFrame {
                duration: f.duration,
                command_list: GDrawCommandList {
                    commands: f.command_list.commands.iter().map(|c| GDrawCommand {
                        cmd_type: c.cmd_type, hidden: c.hidden,
                        stroke_color: c.stroke_color, stroke_width: c.stroke_width,
                        fill_color: c.fill_color, path_open: c.path_open,
                        radius: c.radius, points: c.points.clone(),
                    }).collect(),
                },
            }).collect(),
        };
        crate::owned::new(clone)
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_destroy(seq: *mut GDrawCommandSequence) {
    if !seq.is_null() { unsafe { crate::owned::release(seq); } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_bounds_size(seq: *mut GDrawCommandSequence) -> GSize {
    if seq.is_null() { return GSize { w: 0, h: 0 }; }
    unsafe { (*seq).size }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_set_bounds_size(seq: *mut GDrawCommandSequence, size: GSize) {
    if !seq.is_null() { unsafe { (*seq).size = size; } }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_frame_by_index(seq: *mut GDrawCommandSequence, index: u32) -> *mut GDrawCommandFrame {
    if seq.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let frames = &mut (*seq).frames;
        if (index as usize) < frames.len() {
            &mut frames[index as usize] as *mut GDrawCommandFrame
        } else {
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_frame_by_elapsed(seq: *mut GDrawCommandSequence, elapsed_ms: u32) -> *mut GDrawCommandFrame {
    if seq.is_null() { return std::ptr::null_mut(); }
    unsafe {
        let frames = &mut (*seq).frames;
        if frames.is_empty() { return std::ptr::null_mut(); }
        let total: u32 = frames.iter().map(|f| f.duration as u32).sum();
        if total == 0 { return &mut frames[0] as *mut GDrawCommandFrame; }
        let t = elapsed_ms % total;
        let mut acc = 0u32;
        let len = frames.len();
        for i in 0..len {
            acc += frames[i].duration as u32;
            if t < acc {
                return &mut frames[i] as *mut GDrawCommandFrame;
            }
        }
        &mut frames[len - 1] as *mut GDrawCommandFrame
    }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_num_frames(seq: *mut GDrawCommandSequence) -> u32 {
    if seq.is_null() { return 0; }
    unsafe { (*seq).frames.len() as u32 }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_total_duration(seq: *mut GDrawCommandSequence) -> u32 {
    if seq.is_null() { return 0; }
    unsafe { (*seq).frames.iter().map(|f| f.duration as u32).sum() }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_get_play_count(seq: *mut GDrawCommandSequence) -> u32 {
    if seq.is_null() { return 0; }
    unsafe { (*seq).play_count as u32 }
}

#[no_mangle]
pub extern "C" fn pbl_gdraw_command_sequence_set_play_count(seq: *mut GDrawCommandSequence, count: u32) {
    if !seq.is_null() { unsafe { (*seq).play_count = count as u16; } }
}

// --- GBitmap create from PNG data ---

#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_from_png_data(png_data: *const u8, png_data_size: u32) -> *mut PblGBitmap {
    if png_data.is_null() || png_data_size == 0 { return std::ptr::null_mut(); }
    // Delegate to existing PNG bitmap creation (pbl_gbitmap_from_png uses the same path)
    // For now, return null — full PNG decoding requires a PNG library
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn pbl_gbitmap_create_palettized_from_1bit(_src: *const u8) -> *mut PblGBitmap {
    // Conversion from 1-bit to palettized — stub
    std::ptr::null_mut()
}

// ---------------------------------------------------------------------------
// Persist read string (deprecated, stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_persist_read_string_deprecated(key: u32, size: i32, buffer: *mut u8) -> i32 { pbl_persist_read_string(key, buffer, size) }

// ---------------------------------------------------------------------------
// Layer coordinate conversion (stub)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_layer_convert_point_to_screen(layer: *mut PblLayer, point: GPoint) -> GPoint {
    if layer.is_null() { return point; }
    unsafe { GPoint { x: point.x + (*layer).frame.x, y: point.y + (*layer).frame.y } }
}

#[no_mangle]
pub extern "C" fn pbl_layer_convert_rect_to_screen(layer: *mut PblLayer, rect: GRect) -> GRect {
    if layer.is_null() { return rect; }
    unsafe { GRect { x: rect.x + (*layer).frame.x, y: rect.y + (*layer).frame.y, w: rect.w, h: rect.h } }
}

// ---------------------------------------------------------------------------
// Unobstructed area unsubscribe
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_unobstructed_area_service_unsubscribe() {}

// ---------------------------------------------------------------------------
// Memory cache flush
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pbl_memory_cache_flush() {}

/// Draw text with a &str instead of *const c_char (for emulated path)
pub fn pbl_graphics_draw_text_direct(
    ctx: *mut PblGContext,
    text: &str,
    font_ptr: *const u8,
    box_: GRect,
    alignment: u8,
) {
    if text.is_empty() { return; }
    let color = if !ctx.is_null() {
        unsafe { (*ctx).text_color }
    } else {
        gcolor::colors::WHITE
    };
    if let Some(fb) = fb() {
        font::draw_text(fb, text, font_ptr, box_, alignment, color);
    }
}

