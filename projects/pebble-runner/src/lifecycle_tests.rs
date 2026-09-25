//! Regression tests serialize access to the legacy single-session API.
use crate::{guest_heap, owned, pebble_api::*, runtime};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
static TEST: Mutex<()> = Mutex::new(());

#[test]
fn native_resource_reads_share_the_current_owned_pack() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let mut pack = vec![0; 12 + 256 * 16];
    pack[..4].copy_from_slice(&1u32.to_le_bytes());
    pack[12..16].copy_from_slice(&7u32.to_le_bytes());
    pack[20..24].copy_from_slice(&5u32.to_le_bytes());
    pack.extend_from_slice(b"hello");
    set_resource_pack(pack);
    assert_eq!(pbl_resource_size(7), 5);
    let mut buffer = [0u8; 8];
    assert_eq!(pbl_resource_load(7, buffer.as_mut_ptr(), 3), 3);
    assert_eq!(&buffer[..3], b"hel");
    assert_eq!(pbl_resource_load_byte_range(7, 3, buffer.as_mut_ptr(), 8), 2);
    assert_eq!(&buffer[..2], b"lo");
    assert_eq!(pbl_resource_load_byte_range(7, 5, buffer.as_mut_ptr(), 8), 0);
    assert_eq!(pbl_resource_size(8), 0);
    reset_state();
    assert_eq!(pbl_resource_size(7), 0);
}
static CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn draw(_: *mut PblLayer, _: *mut PblGContext) {
    CALLS.fetch_add(1, Ordering::Relaxed);
}
extern "C" fn battery(_: u32) {
    CALLS.fetch_add(1, Ordering::Relaxed);
}
fn frame() -> GRect {
    GRect {
        x: 0,
        y: 0,
        w: 20,
        h: 20,
    }
}
fn context() -> PblGContext {
    PblGContext {
        fill_color: 0,
        stroke_color: 0,
        text_color: 0,
        stroke_width: 1,
    }
}

#[test]
fn borrowed_bitmap_and_capture_keep_caller_memory_alive() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let mut bytes = vec![0u8; 13];
    bytes[0..2].copy_from_slice(&1u16.to_le_bytes());
    bytes[2..4].copy_from_slice(&2u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    let bmp = pbl_gbitmap_create_with_data(bytes.as_ptr());
    assert!(!bmp.is_null());
    pbl_gbitmap_destroy(bmp);
    bytes[12] = 42;
    let mut fb = vec![0u8; runtime::DISPLAY_WIDTH * runtime::DISPLAY_HEIGHT];
    set_framebuffer_ptr(fb.as_mut_ptr());
    let mut ctx = context();
    let first = pbl_graphics_capture_frame_buffer(&mut ctx);
    assert_eq!(first, pbl_graphics_capture_frame_buffer(&mut ctx));
    pbl_gbitmap_destroy(first);
    assert!(!pbl_graphics_frame_buffer_is_captured(&mut ctx));
    fb[0] = 7;
    reset_state();
    assert_eq!(bytes[12], 42);
    assert_eq!(fb[0], 7);
}

#[test]
fn replacing_removing_and_destroying_draw_registrations() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let layer = pbl_layer_create(frame());
    for _ in 0..30 {
        pbl_layer_set_update_proc(layer, Some(draw));
    }
    assert_eq!(host_layer_count(), 1);
    call_host_layer_update_procs(&mut context());
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    pbl_layer_set_update_proc(layer, None);
    assert_eq!(host_layer_count(), 0);
    pbl_layer_set_update_proc(layer, Some(draw));
    pbl_layer_destroy(layer);
    assert_eq!(host_layer_count(), 0);
    for _ in 0..32 {
        let text = pbl_text_layer_create(frame());
        let bitmap = pbl_bitmap_layer_create(frame());
        pbl_text_layer_destroy(text);
        pbl_bitmap_layer_destroy(bitmap);
    }
    assert_eq!(host_layer_count(), 0);
    reset_state();
}

static VICTIM: AtomicUsize = AtomicUsize::new(0);
extern "C" fn destroy_next(_: *mut PblLayer, _: *mut PblGContext) {
    pbl_layer_destroy(VICTIM.swap(0, Ordering::Relaxed) as *mut PblLayer);
}
#[test]
fn callback_can_destroy_a_later_layer_in_the_same_frame() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let first = pbl_layer_create(frame());
    let second = pbl_layer_create(frame());
    VICTIM.store(second as usize, Ordering::Relaxed);
    pbl_layer_set_update_proc(first, Some(destroy_next));
    pbl_layer_set_update_proc(second, Some(draw));
    call_host_layer_update_procs(&mut context());
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    reset_state();
}

#[test]
fn tree_reparent_destroy_and_extra_data_use_correct_lifetimes() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let window = pbl_window_create();
    let root = pbl_window_get_root_layer(window);
    let other = pbl_layer_create(frame());
    let child = pbl_layer_create_with_data(frame(), 137);
    assert!(!child.is_null());
    unsafe {
        pbl_layer_get_data(child).write_bytes(0xa5, 137);
    }
    pbl_layer_add_child(root, child);
    pbl_layer_add_child(other, child);
    unsafe {
        assert!((*root).first_child.is_null());
        assert_eq!((*other).first_child, child);
    }
    pbl_layer_add_child(child, other); // cycle rejected
    unsafe {
        assert!((*other).parent.is_null());
    }
    pbl_layer_destroy(child);
    unsafe {
        assert!((*other).first_child.is_null());
    }
    pbl_layer_destroy(root);
    unsafe {
        assert!((*window).root_layer.is_null());
    }
    pbl_window_destroy(window);
    reset_state();
}

#[test]
fn reset_clears_battery_callback_and_reclaims_remaining_objects() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let text = pbl_text_layer_create(frame());
    let bitmap = pbl_gbitmap_create_blank(8, 8);
    pbl_battery_state_service_subscribe(Some(battery));
    reset_state();
    poll_battery();
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert!(owned::generation(text).is_none());
    assert!(owned::generation(bitmap).is_none());
    assert_eq!(host_layer_count(), 0);
}

extern "C" fn destroy_animation(a: *mut PblAnimation, _: *mut u8) {
    pbl_animation_destroy(a);
}
extern "C" fn stopped(_: *mut PblAnimation, _: bool, _: *mut u8) {
    CALLS.fetch_add(1, Ordering::Relaxed);
}
#[test]
fn animation_callbacks_and_nested_teardown_do_not_use_freed_objects() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let anim = pbl_animation_create();
    pbl_animation_set_handlers(
        anim,
        Some(destroy_animation),
        Some(stopped),
        std::ptr::null_mut(),
    );
    let generation = owned::generation(anim).unwrap();
    dispatch_native_anim_events(&[
        AnimEvent::Started(anim, generation),
        AnimEvent::Stopped(anim, true, generation),
    ]);
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    let a = pbl_animation_create();
    let b = pbl_animation_create();
    let inner = pbl_animation_sequence_create(a, b, std::ptr::null_mut());
    let outer = pbl_animation_sequence_create(inner, std::ptr::null_mut(), std::ptr::null_mut());
    reset_animations();
    for ptr in [a, b, inner, outer] {
        assert!(owned::generation(ptr).is_none());
    }
    reset_state();
}

#[test]
fn guest_heap_reuses_frees_preserves_realloc_and_checks_overflow() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let a = guest_heap::malloc(8);
    assert!(!a.is_null());
    unsafe {
        a.write_bytes(0x5a, 8);
    }
    let b = guest_heap::realloc(a, 32);
    assert!(!b.is_null());
    unsafe {
        assert_eq!(std::slice::from_raw_parts(b, 8), &[0x5a; 8]);
    }
    assert!(guest_heap::malloc(usize::MAX).is_null());
    assert!(guest_heap::calloc(usize::MAX, 2).is_null());
    assert!(guest_heap::realloc(b, usize::MAX).is_null());
    unsafe {
        assert_eq!(*b, 0x5a);
    }
    guest_heap::free(b);
    for _ in 0..5 {
        let all = guest_heap::malloc(512 * 1024);
        assert!(!all.is_null());
        assert!(guest_heap::malloc(8).is_null());
        guest_heap::free(all);
    }
    let leaked = guest_heap::malloc(512 * 1024);
    assert!(!leaked.is_null());
    reset_state();
    assert!(!guest_heap::malloc(512 * 1024).is_null());
    reset_state();
}

fn binary() -> (Vec<u8>, crate::pbw::PebbleProcessInfo) {
    let mut data = vec![0u8; 132];
    data[..6].copy_from_slice(b"PBLAPP");
    data[14..16].copy_from_slice(&132u16.to_le_bytes());
    data[128..130].copy_from_slice(&256u16.to_le_bytes());
    data[92..96].copy_from_slice(&140u32.to_le_bytes());
    data[16..20].copy_from_slice(&130u32.to_le_bytes());
    data[130..132].copy_from_slice(&0xe7feu16.to_le_bytes());
    let info = crate::pbw::parse_header(&data).unwrap();
    (data, info)
}
#[test]
fn loader_rejects_malformed_ranges_before_mapping() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let (data, mut info) = binary();
    assert!(crate::executor::load_binary(&data, &info).is_ok());
    info.entry_point = u32::MAX;
    assert!(crate::executor::load_binary(&data, &info).is_err());
    let (_, mut info) = binary();
    info.load_size = 256;
    assert!(crate::executor::load_binary(&data, &info).is_err());
    let (_, mut info) = binary();
    info.num_reloc_entries = u32::MAX;
    assert!(crate::executor::load_binary(&data, &info).is_err());
    let (_, mut info) = binary();
    info.sym_table_addr = u32::MAX;
    assert!(crate::executor::load_binary(&data, &info).is_err());
}
#[cfg(not(target_arch = "arm"))]
#[test]
fn interpreter_honors_stop_before_guest_enters_event_loop() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let (data, info) = binary();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(crate::emu::load_and_execute(&data, &info, stop).is_ok());
}

#[test]
fn window_destruction_clears_descendant_window_links() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let window = pbl_window_create();
    let root = pbl_window_get_root_layer(window);
    let child = pbl_layer_create(frame());
    let grandchild = pbl_layer_create(frame());
    pbl_layer_add_child(child, grandchild);
    pbl_layer_add_child(root, child);
    unsafe {
        assert_eq!((*grandchild).window, window);
    }
    pbl_window_destroy(window);
    unsafe {
        assert!((*child).parent.is_null());
        assert!((*grandchild).window.is_null());
    }
    reset_state();
}

#[test]
fn clipped_sub_bitmaps_never_read_beyond_parent() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    assert!(pbl_gbitmap_create_blank(-1, 20).is_null());
    let bitmap = pbl_gbitmap_create_blank(4, 4);
    unsafe {
        pbl_gbitmap_get_data(bitmap).write_bytes(0x5a, 16);
    }
    let sub = pbl_gbitmap_create_as_sub_bitmap(
        bitmap,
        GRect {
            x: 3,
            y: 3,
            w: 20,
            h: 20,
        },
    );
    assert!(!sub.is_null());
    unsafe {
        assert_eq!((*sub).bounds.w, 1);
        assert_eq!((*sub).bounds.h, 1);
        assert_eq!(*(*sub).data, 0x5a);
    }
    assert!(pbl_gbitmap_create_as_sub_bitmap(
        bitmap,
        GRect {
            x: 20,
            y: 20,
            w: 2,
            h: 2
        }
    )
    .is_null());
    pbl_gbitmap_destroy(bitmap);
    pbl_gbitmap_destroy(sub);
    reset_state();
}

#[test]
fn owned_palette_can_come_from_guest_arena() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let bitmap = pbl_gbitmap_create_blank(4, 4);
    let palette = guest_heap::malloc(512 * 1024);
    assert!(!palette.is_null());
    pbl_gbitmap_set_palette(bitmap, palette, true);
    pbl_gbitmap_set_palette(bitmap, palette, true); // retain same pointer without freeing it
    assert!(guest_heap::malloc(8).is_null());
    pbl_gbitmap_destroy(bitmap);
    assert!(!guest_heap::malloc(512 * 1024).is_null());
    reset_state();
}

#[test]
fn many_live_layers_and_property_target_teardown() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    for _ in 0..40 {
        let layer = pbl_layer_create(frame());
        pbl_layer_set_update_proc(layer, Some(draw));
    }
    call_host_layer_update_procs(&mut context());
    assert_eq!(CALLS.load(Ordering::Relaxed), 40);
    let target = pbl_layer_create(frame());
    let from = frame();
    let to = GRect { x: 10, ..frame() };
    let anim = pbl_property_animation_create_layer_frame(target, &from, &to);
    pbl_animation_schedule(anim);
    pbl_layer_destroy(target);
    assert!(!pbl_animation_is_scheduled(anim));
    unsafe {
        assert!((*anim).target_layer.is_null());
    }
    tick_animations();
    reset_state();
}

#[cfg(not(target_arch = "arm"))]
#[test]
fn interpreter_honors_stop_during_guest_initialization() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let (data, info) = binary();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = stop.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        signal.store(true, Ordering::Relaxed);
    });
    assert!(crate::emu::load_and_execute(&data, &info, stop).is_ok());
    thread.join().unwrap();
}

#[test]
fn custom_fonts_and_outbox_buffers_are_reused_and_reclaimed() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    // One minimal PFO entry in a standard resource pack; no glyph parsing needed.
    let mut pack = vec![0u8; 12 + 256 * 16 + 10];
    pack[0..4].copy_from_slice(&1u32.to_le_bytes());
    pack[12..16].copy_from_slice(&1u32.to_le_bytes());
    pack[20..24].copy_from_slice(&10u32.to_le_bytes());
    pack[4108] = 2;
    pack[4109] = 14;
    set_resource_pack(pack);
    for _ in 0..40 {
        let font = pbl_fonts_load_custom_font(1);
        assert_eq!(crate::font::custom_font_bytes(), 10);
        pbl_fonts_unload_custom_font(font);
        assert_eq!(crate::font::custom_font_bytes(), 0);
    }
    pbl_fonts_load_custom_font(1);
    let mut first = std::ptr::null_mut();
    assert_eq!(pbl_app_message_outbox_begin(&mut first), 0);
    for _ in 0..40 {
        let mut next = std::ptr::null_mut();
        pbl_app_message_outbox_begin(&mut next);
        assert_eq!(first, next);
    }
    pbl_app_message_deregister_callbacks();
    assert!(owned::generation(first).is_some());
    reset_state();
    assert_eq!(crate::font::custom_font_bytes(), 0);
    assert!(owned::generation(first).is_none());
}

static CLICK_BUTTON: AtomicUsize = AtomicUsize::new(99);
static CLICK_CONTEXT: AtomicUsize = AtomicUsize::new(0);
extern "C" fn single_click(recognizer: usize, context: *mut u8) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    CLICK_BUTTON.store(pbl_click_recognizer_get_button_id(recognizer as *mut u8) as usize, Ordering::Relaxed);
    CLICK_CONTEXT.store(context as usize, Ordering::Relaxed);
}
extern "C" fn single_provider(_: *mut u8) {
    pbl_window_single_click_subscribe(2, Some(single_click));
}
#[test]
fn native_clicks_deliver_context_and_clear_on_window_or_session_changes() {
    let _lock = TEST.lock().unwrap();
    reset_state(); CALLS.store(0, Ordering::Relaxed);
    let queue = std::sync::Arc::new(Mutex::new(Vec::new()));
    set_button_queue(queue.clone());
    let first = pbl_window_create();
    pbl_window_set_click_config_provider(first, Some(single_provider));
    queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    pbl_window_stack_push(first, false);
    queue.lock().unwrap().extend([2, 99]); dispatch_native_clicks();
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    assert_eq!(CLICK_BUTTON.load(Ordering::Relaxed), 2);
    assert_eq!(CLICK_CONTEXT.load(Ordering::Relaxed), first as usize);
    pbl_window_set_click_config_provider_with_context(first, Some(single_provider), 123usize as *mut u8);
    queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CLICK_CONTEXT.load(Ordering::Relaxed), 123);
    pbl_window_set_click_context(2, 456usize as *mut u8);
    queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CLICK_CONTEXT.load(Ordering::Relaxed), 456);
    let second = pbl_window_create(); pbl_window_stack_push(second, false);
    queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CALLS.load(Ordering::Relaxed), 3, "subscriptions must not leak into another window");
    pbl_window_stack_push(first, false); pbl_window_destroy(first);
    queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CALLS.load(Ordering::Relaxed), 3);
    reset_state(); queue.lock().unwrap().push(2); dispatch_native_clicks();
    assert_eq!(CALLS.load(Ordering::Relaxed), 3, "old UI queue must be detached");
}

#[test]
fn persist_api_roundtrip_truncation_and_deprecated_argument_order() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let dir = std::env::temp_dir().join(format!("pebble-persist-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("settings.json");
    crate::persist::select_test(path.clone());
    assert_eq!(pbl_persist_read_int(10001), 0); // no app-specific fake defaults
    assert_eq!(pbl_persist_write_int(10, -42), 4);
    assert_eq!(pbl_persist_write_bool(11, true), 1);
    assert_eq!(pbl_persist_write_string(12, b"violet\0".as_ptr()), 7);
    reset_state();
    crate::persist::select_test(path);
    assert_eq!(pbl_persist_read_int(10), -42);
    assert!(pbl_persist_read_bool(11));
    let mut buffer = [0xaa; 4];
    assert_eq!(pbl_persist_read_string_deprecated(12, 4, buffer.as_mut_ptr()), 4);
    assert_eq!(&buffer, b"vio\0");
    assert_eq!(pbl_persist_read_data(99, buffer.as_mut_ptr(), 4), -9);
    assert_eq!(&buffer, b"vio\0");
    assert_eq!(pbl_persist_read_data(12, std::ptr::null_mut(), 0), 0);
    assert_eq!(pbl_persist_write_data_deprecated(13, 4, buffer.as_ptr()), 4);
    assert_eq!(pbl_persist_get_size(13), 4);
    assert_eq!(pbl_persist_delete(13), 0);
    assert!(!pbl_persist_exists(13));
    reset_state();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn idle_event_loop_draws_once_then_timer_redraws() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let layer = pbl_layer_create(frame());
    pbl_layer_set_update_proc(layer, Some(draw));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    set_stop_flag(stop.clone());
    extern "C" fn timer(_: *mut u8) {}
    pbl_app_timer_register(100, timer as usize, 0);
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        stop.store(true, Ordering::Relaxed);
    });
    pbl_app_event_loop();
    stopper.join().unwrap();
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    reset_state();
}

#[cfg(target_arch = "arm")]
#[test]
#[ignore = "requires the watch's sensorfw and BlueZ; run explicitly on device"]
fn live_watch_state_probe() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let mut zone = [0; 128];
    pbl_clock_get_timezone(zone.as_mut_ptr(), zone.len());
    println!("timezone: {}", String::from_utf8_lossy(&zone).trim_end_matches('\0'));
    assert!(zone.starts_with(b"Europe/Berlin\0"));
    println!("battery packed: {:06x}, bluetooth powered: {}", pbl_battery_state_service_peek(), pbl_bluetooth_connection_service_peek());
    extern "C" fn compass(h: crate::compass::Heading) {
        println!("compass: {h:?}");
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
    CALLS.store(0, Ordering::Relaxed);
    crate::compass::subscribe(Some(compass));
    crate::compass::set_filter(0);
    let mut early_timestamp = 0;
    for iteration in 0..52 {
        crate::compass::poll();
        if iteration == 8 { early_timestamp = crate::compass::sample_timestamp(); }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(CALLS.load(Ordering::Relaxed) > 1, "compass did not provide callbacks");
    assert!(crate::compass::sample_timestamp() > early_timestamp, "sensorfw samples stopped advancing");
    reset_state();
}

#[test]
fn property_animation_keeps_drawing_between_start_and_end() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    CALLS.store(0, Ordering::Relaxed);
    let layer = pbl_layer_create(frame());
    pbl_layer_set_update_proc(layer, Some(draw));
    let mut to = frame(); to.x = 80;
    let anim = pbl_property_animation_create_layer_frame(layer, &frame(), &to);
    pbl_animation_set_duration(anim, 300);
    pbl_animation_schedule(anim);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    set_stop_flag(stop.clone());
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        stop.store(true, Ordering::Relaxed);
    });
    pbl_app_event_loop();
    stopper.join().unwrap();
    assert!(CALLS.load(Ordering::Relaxed) >= 3);
    assert_eq!(unsafe { (*layer).frame.x }, 80);
    reset_state();
}

#[test]
fn battery_charging_only_changes_notify_and_full_is_plugged() {
    let _lock = TEST.lock().unwrap();
    reset_state();
    let dir = std::env::temp_dir().join(format!("pebble-battery-{}", std::process::id()));
    let battery_dir = dir.join("sys/class/power_supply/battery");
    std::fs::create_dir_all(&battery_dir).unwrap();
    std::fs::write(battery_dir.join("capacity"), "72\n").unwrap();
    std::fs::write(battery_dir.join("status"), "Discharging\n").unwrap();
    let old = std::env::var_os("HOKI_SIM_STATE");
    std::env::set_var("HOKI_SIM_STATE", &dir);
    CALLS.store(0, Ordering::Relaxed);
    pbl_battery_state_service_subscribe(Some(battery));
    poll_battery(); poll_battery();
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    assert_eq!(pbl_battery_state_service_peek(), 72);
    std::fs::write(battery_dir.join("status"), "Charging\n").unwrap();
    poll_battery();
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(pbl_battery_state_service_peek(), 72 | 1 << 8 | 1 << 16);
    std::fs::write(battery_dir.join("status"), "Full\n").unwrap();
    poll_battery();
    assert_eq!(pbl_battery_state_service_peek(), 72 | 1 << 16);
    pbl_battery_state_service_unsubscribe();
    std::fs::write(battery_dir.join("status"), "Discharging\n").unwrap();
    poll_battery();
    assert_eq!(CALLS.load(Ordering::Relaxed), 3);
    if let Some(old) = old { std::env::set_var("HOKI_SIM_STATE", old); } else { std::env::remove_var("HOKI_SIM_STATE"); }
    reset_state();
    std::fs::remove_dir_all(dir).unwrap();
}
