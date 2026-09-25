use crate::pbw::PebbleProcessInfo;
use crate::pebble_api;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Number of entries in the Pebble jump table (from exported_symbols.json revision 89)
const JUMP_TABLE_SIZE: usize = 632;

/// The jump table: array of function pointers that the Pebble binary calls into.
/// Must be static because the Pebble binary holds a raw pointer to it.
static mut JUMP_TABLE: [usize; JUMP_TABLE_SIZE] = [0; JUMP_TABLE_SIZE];

/// We need to know WHICH function index was called. Since the Pebble stub
/// mechanism uses `bx r12` to jump to us, r12 still holds our function address.
/// We generate a lookup table mapping function address -> index.
static mut STUB_ADDR_TO_INDEX: [usize; JUMP_TABLE_SIZE] = [0; JUMP_TABLE_SIZE];

/// A single shared unimplemented stub. Returns 0 and logs the call.
/// We can't easily determine which index called us, but we can detect that
/// an unimplemented function was called by scanning the jump table.
static mut UNIMPL_CALL_COUNT: u32 = 0;

#[cfg(target_arch = "arm")]
use crate::guest_heap::{malloc as pbl_malloc, calloc as pbl_calloc, realloc as pbl_realloc, free as pbl_free};

extern "C" fn unimplemented_stub() -> u32 {
    unsafe { UNIMPL_CALL_COUNT += 1; }
    eprintln!("[pebble:UNIMPL] unknown index (call #{})", unsafe { UNIMPL_CALL_COUNT });
    0
}

/// Per-index unimplemented stubs that log which jump table index was called
macro_rules! make_unimpl_stubs {
    ($($idx:expr),* $(,)?) => {
        $(
            paste::paste! {
                extern "C" fn [<unimpl_stub_ $idx>]() -> u32 {
                    unsafe { UNIMPL_CALL_COUNT += 1; }
                    eprintln!("[pebble:UNIMPL] index {} ({}) — call #{}",
                        $idx, jump_table_name($idx), unsafe { UNIMPL_CALL_COUNT });
                    0
                }
            }
        )*

        fn get_unimpl_stub(idx: usize) -> extern "C" fn() -> u32 {
            match idx {
                $( $idx => paste::paste! { [<unimpl_stub_ $idx>] }, )*
                _ => unimplemented_stub,
            }
        }
    };
}

make_unimpl_stubs!(
    0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,
    30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,
    60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,
    90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,
    120,121,122,123,124,125,126,127,128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,
    150,151,152,153,154,155,156,157,158,159,160,161,162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,
    180,181,182,183,184,185,186,187,188,189,190,191,192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,
    210,211,212,213,214,215,216,217,218,219,220,221,222,223,224,225,226,227,228,229,230,231,232,233,234,235,236,237,238,239,
    240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255,256,257,258,259,260,261,262,263,264,265,266,267,268,269,
    270,271,272,273,274,275,276,277,278,279,280,281,282,283,284,285,286,287,288,289,290,291,292,293,294,295,296,297,298,299,
    300,301,302,303,304,305,306,307,308,309,310,311,312,313,314,315,316,317,318,319,320,321,322,323,324,325,326,327,328,329,
    330,331,332,333,334,335,336,337,338,339,340,341,342,343,344,345,346,347,348,349,350,351,352,353,354,355,356,357,358,359,
    360,361,362,363,364,365,366,367,368,369,370,371,372,373,374,375,376,377,378,379,380,381,382,383,384,385,386,387,388,389,
    390,391,392,393,394,395,396,397,398,399,400,401,402,403,404,405,406,407,408,409,410,411,412,413,414,415,416,417,418,419,
    420,421,422,423,424,425,426,427,428,429,430,431,432,433,434,435,436,437,438,439,440,441,442,443,444,445,446,447,448,449,
    450,451,452,453,454,455,456,457,458,459,460,461,462,463,464,465,466,467,468,469,470,471,472,473,474,475,476,477,478,479,
    480,481,482,483,484,485,486,487,488,489,490,491,492,493,494,495,496,497,498,499,500,501,502,503,504,505,506,507,508,509,
    510,511,512,513,514,515,516,517,518,519,520,521,522,523,524,525,526,527,528,529,530,531,532,533,534,535,536,537,538,539,
    540,541,542,543,544,545,546,547,548,549,550,551,552,553,554,555,556,557,558,559,560,561,562,563,564,565,566,567,568,569,
    570,571,572,573,574,575,576,577,578,579,580,581,582,583,584,585,586,587,588,589,590,591,592,593,594,595,596,597,598,599,
    600,601,602,603,604,605,606,607,608,609,610,611,612,613,614,615,616,617,618,619,620,621,622,623,624,625,626,627,628,629,
    630,631
);

/// Log which jump table entries are still using the default stub
pub fn log_unimplemented_entries() {
    eprintln!("[pebble] Unimplemented jump table entries:");
    unsafe {
        for (i, &addr) in JUMP_TABLE.iter().enumerate() {
            if addr == get_unimpl_stub(i) as usize {
                // Look up the function name from our embedded table
                let name = jump_table_name(i);
                if !name.is_empty() {
                    eprintln!("  [{:3}] {}", i, name);
                }
            }
        }
    }
}

/// Return function name for a jump table index (from exported_symbols.json).
pub fn jump_table_name(idx: usize) -> &'static str {
    match idx {
        0 => "accel_data_service_subscribe__deprecated",
        1 => "accel_data_service_unsubscribe",
        2 => "accel_service_peek",
        3 => "accel_service_set_samples_per_update",
        4 => "accel_service_set_sampling_rate",
        5 => "accel_tap_service_subscribe",
        6 => "accel_tap_service_unsubscribe",
        7 => "action_bar_layer_legacy2_add_to_window",
        8 => "action_bar_layer_legacy2_clear_icon",
        9 => "action_bar_layer_legacy2_create",
        10 => "action_bar_layer_legacy2_destroy",
        11 => "action_bar_layer_legacy2_get_layer",
        12 => "action_bar_layer_legacy2_remove_from_window",
        13 => "action_bar_layer_legacy2_set_background_color_2bit",
        14 => "action_bar_layer_legacy2_set_click_config_provider",
        15 => "action_bar_layer_legacy2_set_context",
        16 => "action_bar_layer_legacy2_set_icon",
        17 => "animation_legacy2_create",
        18 => "animation_legacy2_destroy",
        19 => "animation_legacy2_get_context",
        20 => "animation_legacy2_is_scheduled",
        21 => "animation_legacy2_schedule",
        22 => "animation_legacy2_set_curve",
        23 => "animation_legacy2_set_delay",
        24 => "animation_legacy2_set_duration",
        25 => "animation_legacy2_set_handlers",
        26 => "animation_legacy2_set_implementation",
        27 => "animation_legacy2_unschedule",
        28 => "animation_legacy2_unschedule_all",
        29 => "app_comm_get_sniff_interval",
        30 => "app_comm_set_sniff_interval",
        31 => "app_event_loop",
        32 => "app_in_focus_service_subscribe (removed)",
        33 => "app_in_focus_service_unsubscribe (removed)",
        34 => "app_log",
        35 => "app_message_deregister_callbacks",
        36 => "app_message_open",
        37 => "app_message_out_get (removed)",
        38 => "app_message_out_release (removed)",
        39 => "app_message_out_send (removed)",
        40 => "app_message_register_callbacks (removed)",
        41 => "app_out_of_focus_service_subscribe (removed)",
        42 => "app_out_of_focus_service_unsubscribe (removed)",
        43 => "app_sync_deinit",
        44 => "app_sync_get",
        45 => "app_sync_init",
        46 => "app_sync_set",
        47 => "app_timer_cancel",
        48 => "app_timer_register",
        49 => "app_timer_reschedule",
        50 => "atan2_lookup",
        51 => "atoi",
        52 => "atol",
        53 => "battery_state_service_peek",
        54 => "battery_state_service_subscribe",
        55 => "battery_state_service_unsubscribe",
        56 => "bitmap_layer_create",
        57 => "bitmap_layer_destroy",
        58 => "bitmap_layer_get_layer",
        59 => "bitmap_layer_set_alignment",
        60 => "bitmap_layer_set_background_color_2bit",
        61 => "bitmap_layer_set_bitmap",
        62 => "bitmap_layer_set_compositing_mode",
        63 => "bluetooth_connection_service_peek",
        64 => "bluetooth_connection_service_subscribe",
        65 => "bluetooth_connection_service_unsubscribe",
        66 => "click_number_of_clicks_counted",
        67 => "click_recognizer_get_button_id",
        68 => "clock_copy_time_string",
        69 => "clock_is_24h_style",
        70 => "cos_lookup",
        71 => "data_logging_create",
        72 => "data_logging_finish",
        73 => "data_logging_log",
        74 => "dict_calc_buffer_size",
        75 => "dict_calc_buffer_size_from_tuplets",
        76 => "dict_find",
        77 => "dict_merge",
        78 => "dict_read_begin_from_buffer",
        79 => "dict_read_first",
        80 => "dict_read_next",
        81 => "dict_serialize_tuplets",
        82 => "dict_serialize_tuplets_to_buffer__deprecated",
        83 => "dict_serialize_tuplets_to_buffer_with_iter",
        84 => "dict_write_begin",
        85 => "dict_write_cstring",
        86 => "dict_write_data",
        87 => "dict_write_end",
        88 => "dict_write_int",
        89 => "dict_write_int16",
        90 => "dict_write_int32",
        91 => "dict_write_int8",
        92 => "dict_write_tuplet",
        93 => "dict_write_uint16",
        94 => "dict_write_uint32",
        95 => "dict_write_uint8",
        96 => "fonts_get_system_font",
        97 => "fonts_load_custom_font",
        98 => "fonts_unload_custom_font",
        99 => "free",
        100 => "gbitmap_create_as_sub_bitmap",
        101 => "gbitmap_create_with_data",
        102 => "gbitmap_create_with_resource",
        103 => "gbitmap_destroy",
        104 => "gmtime",
        105 => "gpath_create",
        106 => "gpath_destroy",
        107 => "gpath_draw_filled_legacy",
        108 => "gpath_draw_outline",
        109 => "gpath_move_to",
        110 => "gpath_rotate_to",
        111 => "gpoint_equal",
        112 => "graphics_context_set_compositing_mode",
        113 => "graphics_context_set_fill_color_2bit",
        114 => "graphics_context_set_stroke_color_2bit",
        115 => "graphics_context_set_text_color_2bit",
        116 => "graphics_draw_bitmap_in_rect",
        117 => "graphics_draw_circle",
        118 => "graphics_draw_line",
        119 => "graphics_draw_pixel",
        120 => "graphics_draw_rect",
        121 => "graphics_draw_round_rect",
        122 => "graphics_fill_circle",
        123 => "graphics_fill_rect",
        124 => "graphics_text_draw (removed)",
        125 => "graphics_text_layout_get_max_used_size",
        126 => "grect_align",
        127 => "grect_center_point",
        128 => "grect_clip",
        129 => "grect_contains_point",
        130 => "grect_crop",
        131 => "grect_equal",
        132 => "grect_is_empty",
        133 => "grect_standardize",
        134 => "gsize_equal",
        135 => "inverter_layer_create",
        136 => "inverter_layer_destroy",
        137 => "inverter_layer_get_layer",
        138 => "layer_add_child",
        139 => "layer_create",
        140 => "layer_create_with_data",
        141 => "layer_destroy",
        142 => "layer_get_bounds",
        143 => "layer_get_clips",
        144 => "layer_get_data",
        145 => "layer_get_frame",
        146 => "layer_get_hidden",
        147 => "layer_get_window",
        148 => "layer_insert_above_sibling",
        149 => "layer_insert_below_sibling",
        150 => "layer_mark_dirty",
        151 => "layer_remove_child_layers",
        152 => "layer_remove_from_parent",
        153 => "layer_set_bounds",
        154 => "layer_set_clips",
        155 => "layer_set_frame",
        156 => "layer_set_hidden",
        157 => "layer_set_update_proc",
        158 => "light_enable",
        159 => "light_enable_interaction",
        160 => "localtime__deprecated",
        161 => "malloc",
        162 => "memcpy",
        163 => "memmove",
        164 => "memset",
        165 => "menu_cell_basic_draw",
        166 => "menu_cell_basic_header_draw",
        167 => "menu_cell_title_draw",
        168 => "menu_index_compare",
        169 => "menu_layer_legacy2_create",
        170 => "menu_layer_destroy",
        171 => "menu_layer_get_layer",
        172 => "menu_layer_get_scroll_layer",
        173 => "menu_layer_get_selected_index",
        174 => "menu_layer_reload_data",
        175 => "menu_layer_legacy2_set_callbacks__deprecated",
        176 => "menu_layer_set_click_config_onto_window",
        177 => "menu_layer_set_selected_index",
        178 => "menu_layer_set_selected_next",
        179 => "number_window_create",
        180 => "number_window_destroy",
        181 => "number_window_get_value",
        182 => "number_window_set_label",
        183 => "number_window_set_max",
        184 => "number_window_set_min",
        185 => "number_window_set_step_size",
        186 => "number_window_set_value",
        187 => "persist_delete",
        188 => "persist_exists",
        189 => "persist_get_size",
        190 => "persist_read_bool",
        191 => "persist_read_data__deprecated",
        192 => "persist_read_int",
        193 => "persist_read_string__deprecated",
        194 => "persist_write_bool",
        195 => "persist_write_data__deprecated",
        196 => "persist_write_int",
        197 => "persist_write_string",
        198 => "property_animation_legacy2_create",
        199 => "property_animation_legacy2_create_layer_frame",
        200 => "property_animation_legacy2_destroy",
        201 => "property_animation_legacy2_update_gpoint",
        202 => "property_animation_legacy2_update_grect",
        203 => "property_animation_legacy2_update_int16",
        204 => "psleep",
        205 => "rand",
        206 => "resource_get_handle",
        207 => "resource_load",
        208 => "resource_load_byte_range",
        209 => "resource_size",
        210 => "rot_bitmap_layer_create",
        211 => "rot_bitmap_layer_destroy",
        212 => "rot_bitmap_layer_increment_angle",
        213 => "rot_bitmap_layer_set_angle",
        214 => "rot_bitmap_layer_set_corner_clip_color_2bit",
        215 => "rot_bitmap_set_compositing_mode",
        216 => "rot_bitmap_set_src_ic",
        217 => "scroll_layer_add_child",
        218 => "scroll_layer_create",
        219 => "scroll_layer_destroy",
        220 => "scroll_layer_get_content_offset",
        221 => "scroll_layer_get_content_size",
        222 => "scroll_layer_get_layer",
        223 => "scroll_layer_get_shadow_hidden",
        224 => "scroll_layer_scroll_down_click_handler",
        225 => "scroll_layer_scroll_up_click_handler",
        226 => "scroll_layer_set_callbacks",
        227 => "scroll_layer_set_click_config_onto_window",
        228 => "scroll_layer_set_content_offset",
        229 => "scroll_layer_set_content_size",
        230 => "scroll_layer_set_context",
        231 => "scroll_layer_set_frame",
        232 => "scroll_layer_set_shadow_hidden",
        233 => "simple_menu_layer_create",
        234 => "simple_menu_layer_destroy",
        235 => "simple_menu_layer_get_layer",
        236 => "simple_menu_layer_get_selected_index",
        237 => "simple_menu_layer_set_selected_index",
        238 => "sin_lookup",
        239 => "snprintf",
        240 => "srand",
        241 => "strcat",
        242 => "strcmp",
        243 => "strcpy",
        244 => "strftime",
        245 => "strlen",
        246 => "strncat",
        247 => "strncmp",
        248 => "strncpy",
        249 => "text_layer_legacy2_create",
        250 => "text_layer_legacy2_destroy",
        251 => "text_layer_legacy2_get_content_size",
        252 => "text_layer_legacy2_get_layer",
        253 => "text_layer_legacy2_get_text",
        254 => "text_layer_legacy2_set_background_color_2bit",
        255 => "text_layer_legacy2_set_font",
        256 => "text_layer_legacy2_set_overflow_mode",
        257 => "text_layer_legacy2_set_size",
        258 => "text_layer_legacy2_set_text",
        259 => "text_layer_legacy2_set_text_alignment",
        260 => "text_layer_legacy2_set_text_color_2bit",
        261 => "tick_timer_service_init (removed)",
        262 => "tick_timer_service_subscribe",
        263 => "tick_timer_service_unsubscribe",
        264 => "time__deprecated",
        265 => "time_ms_deprecated",
        266 => "vibes_cancel",
        267 => "vibes_double_pulse",
        268 => "vibes_enqueue_custom_pattern",
        269 => "vibes_long_pulse",
        270 => "vibes_short_pulse",
        271 => "window_create",
        272 => "window_destroy",
        273 => "window_get_click_config_provider",
        274 => "window_get_fullscreen",
        275 => "window_get_root_layer",
        276 => "window_is_loaded",
        277 => "window_set_background_color_2bit",
        278 => "window_set_click_config_provider",
        279 => "window_set_click_config_provider_with_context",
        280 => "window_set_fullscreen",
        281 => "window_set_status_bar_icon",
        282 => "window_set_window_handlers",
        283 => "window_stack_contains_window",
        284 => "window_stack_get_top_window",
        285 => "window_stack_pop",
        286 => "window_stack_pop_all",
        287 => "window_stack_push",
        288 => "window_stack_remove",
        289 => "app_focus_service_subscribe",
        290 => "app_focus_service_unsubscribe",
        291 => "window_get_user_data",
        292 => "window_set_user_data",
        293 => "app_message_get_context",
        294 => "app_message_inbox_size_maximum",
        295 => "app_message_outbox_begin",
        296 => "app_message_outbox_send",
        297 => "app_message_outbox_size_maximum",
        298 => "app_message_register_inbox_dropped",
        299 => "app_message_register_inbox_received",
        300 => "app_message_register_outbox_failed",
        301 => "app_message_register_outbox_sent",
        302 => "app_message_set_context",
        303 => "window_long_click_subscribe",
        304 => "window_multi_click_subscribe",
        305 => "window_raw_click_subscribe",
        306 => "window_set_click_context",
        307 => "window_single_click_subscribe",
        308 => "window_single_repeating_click_subscribe",
        309 => "graphics_draw_text",
        310 => "dict_serialize_tuplets_to_buffer",
        311 => "persist_read_data",
        312 => "persist_read_string",
        313 => "persist_write_data",
        314 => "dict_size",
        315 => "graphics_text_layout_get_content_size",
        316 => "simple_menu_layer_get_menu_layer",
        317 => "accel_data_service_subscribe",
        318 => "calloc",
        319 => "bitmap_layer_get_bitmap",
        320 => "menu_layer_legacy2_set_callbacks",
        321 => "window_get_click_config_context",
        322 => "number_window_get_window",
        323 => "realloc",
        324 => "gbitmap_create_blank_2bit",
        325 => "click_recognizer_is_repeating",
        326 => "accel_raw_data_service_subscribe",
        327 => "app_worker_is_running",
        328 => "app_worker_kill",
        329 => "app_worker_launch",
        330 => "app_worker_message_subscribe",
        331 => "app_worker_message_unsubscribe",
        332 => "app_worker_send_message",
        333 => "worker_event_loop",
        334 => "worker_launch_app",
        335 => "heap_bytes_free",
        336 => "heap_bytes_used",
        337 => "compass_service_peek",
        338 => "compass_service_set_heading_filter",
        339 => "compass_service_subscribe",
        340 => "compass_service_unsubscribe",
        341 => "uuid_equal",
        342 => "uuid_to_string",
        343 => "gpath_draw_filled",
        344 => "animation_legacy2_set_custom_curve",
        345 => "watch_info_get_color",
        346 => "watch_info_get_firmware_version",
        347 => "watch_info_get_model",
        348 => "graphics_capture_frame_buffer_2bit",
        349 => "graphics_frame_buffer_is_captured",
        350 => "graphics_release_frame_buffer",
        351 => "clock_to_timestamp",
        352 => "launch_reason",
        353 => "wakeup_cancel",
        354 => "wakeup_cancel_all",
        355 => "wakeup_get_launch_event",
        356 => "wakeup_query",
        357 => "wakeup_schedule",
        358 => "wakeup_service_subscribe",
        359 => "clock_is_timezone_set",
        360 => "i18n_get_system_locale",
        361 => "_localeconv_r",
        362 => "setlocale",
        363 => "mktime",
        364 => "gcolor_equal__deprecated",
        365 => "__profiler_init",
        366 => "__profiler_print_stats",
        367 => "__profiler_start",
        368 => "__profiler_stop",
        369 => "action_bar_layer_legacy2_set_background_color (removed)",
        370 => "bitmap_layer_set_background_color",
        371 => "graphics_context_set_fill_color",
        372 => "graphics_context_set_stroke_color",
        373 => "graphics_context_set_text_color",
        374 => "rot_bitmap_layer_set_corner_clip_color",
        375 => "text_layer_legacy2_set_background_color (removed)",
        376 => "text_layer_legacy2_set_text_color (removed)",
        377 => "window_set_background_color",
        378 => "clock_get_timezone",
        379 => "localtime",
        380 => "animation_create",
        381 => "animation_destroy",
        382 => "animation_get_context",
        383 => "animation_is_scheduled",
        384 => "animation_schedule",
        385 => "animation_set_curve",
        386 => "animation_set_custom_curve",
        387 => "animation_set_delay",
        388 => "animation_set_duration",
        389 => "animation_set_handlers",
        390 => "animation_set_implementation",
        391 => "animation_unschedule",
        392 => "animation_unschedule_all",
        393 => "gbitmap_create_blank",
        394 => "graphics_capture_frame_buffer",
        395 => "graphics_capture_frame_buffer_format",
        396 => "property_animation_create",
        397 => "property_animation_create_layer_frame",
        398 => "property_animation_destroy",
        399 => "property_animation_from",
        400 => "property_animation_get_animation",
        401 => "property_animation_subject",
        402 => "property_animation_to",
        403 => "property_animation_update_gpoint",
        404 => "property_animation_update_grect",
        405 => "property_animation_update_int16",
        406 => "gbitmap_create_blank_with_palette",
        407 => "gbitmap_get_bounds",
        408 => "gbitmap_get_bytes_per_row",
        409 => "gbitmap_get_data",
        410 => "gbitmap_get_format",
        411 => "gbitmap_get_palette",
        412 => "gbitmap_set_bounds",
        413 => "gbitmap_set_data",
        414 => "gbitmap_set_palette",
        415 => "gbitmap_sequence_create_with_resource",
        416 => "gbitmap_sequence_destroy",
        417 => "gbitmap_sequence_get_bitmap_size",
        418 => "gbitmap_sequence_get_current_frame_idx",
        419 => "gbitmap_sequence_get_total_num_frames",
        420 => "gbitmap_sequence_update_bitmap_next_frame",
        421 => "gbitmap_create_from_png_data",
        422 => "animation_clone",
        423 => "animation_get_delay",
        424 => "animation_get_duration",
        425 => "animation_get_play_count",
        426 => "animation_get_elapsed",
        427 => "animation_get_reverse",
        428 => "animation_sequence_create",
        429 => "animation_sequence_create_from_array",
        430 => "animation_set_play_count",
        431 => "animation_set_elapsed",
        432 => "animation_set_reverse",
        433 => "animation_spawn_create",
        434 => "animation_spawn_create_from_array",
        435 => "animation_get_curve",
        436 => "animation_get_custom_curve",
        437 => "animation_get_implementation",
        438 => "launch_get_args",
        439 => "menu_layer_create",
        440 => "menu_layer_shadow_enable (removed)",
        441 => "gbitmap_sequence_get_play_count",
        442 => "gbitmap_sequence_restart",
        443 => "gbitmap_sequence_set_play_count",
        444 => "graphics_context_set_antialiased",
        445 => "graphics_context_set_stroke_width",
        446 => "action_bar_layer_add_to_window",
        447 => "action_bar_layer_clear_icon",
        448 => "action_bar_layer_create",
        449 => "action_bar_layer_destroy",
        450 => "action_bar_layer_get_layer",
        451 => "action_bar_layer_remove_from_window",
        452 => "action_bar_layer_set_background_color",
        453 => "action_bar_layer_set_click_config_provider",
        454 => "action_bar_layer_set_context",
        455 => "action_bar_layer_set_icon",
        456 => "action_bar_layer_set_icon_animated",
        457 => "gbitmap_sequence_update_bitmap_by_elapsed",
        458 => "gbitmap_create_palettized_from_1bit",
        459 => "menu_cell_layer_is_highlighted",
        460 => "graphics_draw_rotated_bitmap",
        461 => "action_bar_layer_set_icon_press_animation",
        462 => "text_layer_create",
        463 => "text_layer_destroy",
        464 => "text_layer_get_content_size",
        465 => "text_layer_get_layer",
        466 => "text_layer_get_text",
        467 => "text_layer_set_background_color",
        468 => "text_layer_set_font",
        469 => "text_layer_set_overflow_mode",
        470 => "text_layer_set_size",
        471 => "text_layer_set_text",
        472 => "text_layer_set_text_alignment",
        473 => "text_layer_set_text_color",
        474 => "gdraw_command_draw",
        475 => "gdraw_command_frame_draw",
        476 => "gdraw_command_frame_get_duration",
        477 => "gdraw_command_frame_set_duration",
        478 => "gdraw_command_get_fill_color",
        479 => "gdraw_command_get_hidden",
        480 => "gdraw_command_get_num_points",
        481 => "gdraw_command_get_path_open",
        482 => "gdraw_command_get_point",
        483 => "gdraw_command_get_radius",
        484 => "gdraw_command_get_stroke_color",
        485 => "gdraw_command_get_stroke_width",
        486 => "gdraw_command_get_type",
        487 => "gdraw_command_image_clone",
        488 => "gdraw_command_image_create_with_resource",
        489 => "gdraw_command_image_destroy",
        490 => "gdraw_command_image_draw",
        491 => "gdraw_command_image_get_bounds_size",
        492 => "gdraw_command_image_get_command_list",
        493 => "gdraw_command_image_set_bounds_size",
        494 => "gdraw_command_list_draw",
        495 => "gdraw_command_list_get_command",
        496 => "gdraw_command_list_get_num_commands",
        497 => "gdraw_command_list_iterate",
        498 => "gdraw_command_sequence_clone",
        499 => "gdraw_command_sequence_create_with_resource",
        500 => "gdraw_command_sequence_destroy",
        501 => "gdraw_command_sequence_get_bounds_size",
        502 => "gdraw_command_sequence_get_frame_by_elapsed",
        503 => "gdraw_command_sequence_get_frame_by_index",
        504 => "gdraw_command_sequence_get_num_frames",
        505 => "gdraw_command_sequence_get_play_count",
        506 => "gdraw_command_sequence_get_total_duration",
        507 => "gdraw_command_sequence_set_bounds_size",
        508 => "gdraw_command_sequence_set_play_count",
        509 => "gdraw_command_set_fill_color",
        510 => "gdraw_command_set_hidden",
        511 => "gdraw_command_set_path_open",
        512 => "gdraw_command_set_point",
        513 => "gdraw_command_set_radius",
        514 => "gdraw_command_set_stroke_color",
        515 => "gdraw_command_set_stroke_width",
        516 => "property_animation_create_bounds_origin",
        517 => "property_animation_update_uint32",
        518 => "gpath_draw_outline_open",
        519 => "time",
        520 => "menu_layer_set_highlight_colors",
        521 => "menu_layer_set_normal_colors",
        522 => "menu_layer_set_callbacks",
        523 => "menu_layer_pad_bottom_enable",
        524 => "status_bar_layer_create",
        525 => "status_bar_layer_destroy",
        526 => "status_bar_layer_get_background_color",
        527 => "status_bar_layer_get_foreground_color",
        528 => "status_bar_layer_get_layer",
        529 => "status_bar_layer_set_colors",
        530 => "status_bar_layer_set_separator_mode",
        531 => "difftime",
        532 => "time_ms",
        533 => "gcolor_legible_over",
        534 => "property_animation_update_gcolor8",
        535 => "app_focus_service_subscribe_handlers",
        536 => "action_menu_close",
        537 => "action_menu_freeze",
        538 => "action_menu_get_context",
        539 => "action_menu_get_root_level",
        540 => "action_menu_hierarchy_destroy",
        541 => "action_menu_item_get_action_data",
        542 => "action_menu_item_get_label",
        543 => "action_menu_level_add_action",
        544 => "action_menu_level_add_child",
        545 => "action_menu_level_create",
        546 => "action_menu_level_set_display_mode",
        547 => "action_menu_open",
        548 => "action_menu_set_result_window",
        549 => "action_menu_unfreeze",
        550 => "dictation_session_create",
        551 => "dictation_session_destroy",
        552 => "dictation_session_enable_confirmation",
        553 => "dictation_session_start",
        554 => "dictation_session_stop",
        555 => "smartstrap_attribute_begin_write",
        556 => "smartstrap_attribute_create",
        557 => "smartstrap_attribute_destroy",
        558 => "smartstrap_attribute_end_write",
        559 => "smartstrap_attribute_get_attribute_id",
        560 => "smartstrap_attribute_get_service_id",
        561 => "smartstrap_attribute_read",
        562 => "smartstrap_service_is_available",
        563 => "smartstrap_set_timeout",
        564 => "smartstrap_subscribe",
        565 => "smartstrap_unsubscribe",
        566 => "connection_service_peek_pebble_app_connection",
        567 => "connection_service_peek_pebblekit_connection",
        568 => "connection_service_subscribe",
        569 => "connection_service_unsubscribe",
        570 => "dictation_session_enable_error_dialogs",
        571 => "gbitmap_get_data_row_info",
        572 => "content_indicator_configure_direction",
        573 => "content_indicator_create",
        574 => "content_indicator_destroy",
        575 => "content_indicator_get_content_available",
        576 => "content_indicator_set_content_available",
        577 => "scroll_layer_get_content_indicator",
        578 => "menu_layer_get_center_focused",
        579 => "menu_layer_set_center_focused",
        580 => "grect_inset",
        581 => "gpoint_from_polar",
        582 => "graphics_draw_arc",
        583 => "graphics_fill_radial",
        584 => "grect_centered_from_polar",
        585 => "graphics_text_attributes_create",
        586 => "graphics_text_attributes_destroy",
        587 => "graphics_text_attributes_enable_paging",
        588 => "graphics_text_attributes_enable_screen_text_flow",
        589 => "graphics_text_attributes_restore_default_paging",
        590 => "graphics_text_attributes_restore_default_text_flow",
        591 => "graphics_text_layout_get_content_size_with_attributes",
        592 => "layer_convert_point_to_screen",
        593 => "layer_convert_rect_to_screen",
        594 => "scroll_layer_get_paging",
        595 => "scroll_layer_set_paging",
        596 => "text_layer_enable_screen_text_flow_and_paging",
        597 => "text_layer_restore_default_text_flow_and_paging",
        598 => "menu_layer_is_index_selected",
        599 => "health_service_activities_iterate",
        600 => "health_service_any_activity_accessible",
        601 => "health_service_events_subscribe",
        602 => "health_service_events_unsubscribe",
        603 => "health_service_get_minute_history",
        604 => "health_service_metric_accessible",
        605 => "health_service_peek_current_activities",
        606 => "health_service_sum",
        607 => "health_service_sum_today",
        608 => "time_start_of_today",
        609 => "health_service_metric_averaged_accessible",
        610 => "health_service_sum_averaged",
        611 => "health_service_get_measurement_system_for_display",
        612 => "gdraw_command_frame_get_command_list",
        613 => "gcolor_equal",
        614 => "app_glance_add_slice",
        615 => "app_glance_reload",
        616 => "exit_reason_set",
        617 => "health_service_aggregate_averaged",
        618 => "health_service_cancel_metric_alert",
        619 => "health_service_metric_aggregate_averaged_accessible",
        620 => "health_service_peek_current_value",
        621 => "health_service_register_metric_alert",
        622 => "layer_get_unobstructed_bounds",
        623 => "preferred_result_display_duration",
        624 => "unobstructed_area_service_subscribe",
        625 => "unobstructed_area_service_unsubscribe",
        626 => "memory_cache_flush",
        627 => "rocky_event_loop_with_resource",
        628 => "health_service_get_heart_rate_sample_period_expiration_sec",
        629 => "health_service_set_heart_rate_sample_period",
        630 => "preferred_content_size",
        631 => "quiet_time_is_active",
        _ => "",
    }
}


/// Load a Pebble binary into executable memory and prepare it for execution.
/// Returns a function pointer to the entry point.
pub struct LoadedBinary { base: *mut u8, size: usize, entry: usize }
impl Drop for LoadedBinary {
    fn drop(&mut self) { unsafe { libc::munmap(self.base.cast(), self.size); } }
}

pub fn load_binary(bin_data: &[u8], info: &PebbleProcessInfo) -> Result<LoadedBinary, String> {
    crate::pbw::validate_binary(bin_data, info)?;
    crate::persist::select(&info.uuid_str());
    let size = info.virtual_size as usize;
    let base = unsafe { libc::mmap(std::ptr::null_mut(), size,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
    if base == libc::MAP_FAILED { return Err(format!("mmap: {}", std::io::Error::last_os_error())); }
    let loaded = LoadedBinary { base: base.cast(), size, entry: (info.entry_point & !1) as usize };
    // Anonymous mapping is already zeroed. Never copy the relocation table into BSS.
    unsafe { std::ptr::copy_nonoverlapping(bin_data.as_ptr(), loaded.base, info.load_size as usize); }
    for i in 0..info.num_reloc_entries as usize {
        let offset = info.load_size as usize + i * 4;
        let target = u32::from_le_bytes(bin_data[offset..offset + 4].try_into().unwrap()) as usize;
        unsafe {
            let ptr = loaded.base.add(target).cast::<u32>();
            ptr.write_unaligned(ptr.read_unaligned().wrapping_add(loaded.base as u32));
        }
    }
    build_jump_table();
    unsafe {
        loaded.base.add(info.sym_table_addr as usize).cast::<u32>().write_unaligned(JUMP_TABLE.as_ptr() as u32);
    }
    #[cfg(target_arch = "arm")]
    unsafe {
        // Linux ARM private cacheflush syscall; see arch/arm/include/uapi/asm/unistd.h.
        if libc::syscall(0x0f0002, loaded.base, loaded.base.add(size), 0) != 0 {
            return Err(format!("instruction cache flush: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(loaded)
}

/// Build the jump table with our Pebble API implementations.
pub fn build_jump_table() {
    unsafe {
        // Fill with per-index stubs that log which index was called
        for i in 0..JUMP_TABLE.len() {
            JUMP_TABLE[i] = get_unimpl_stub(i) as usize;
        }

        // Now wire up the functions we've implemented.
        // Index numbers from exported_symbols.json (sorted by addedRevision, then name).
        JUMP_TABLE[31] = pebble_api::pbl_app_event_loop as usize;        // app_event_loop
        JUMP_TABLE[34] = pebble_api::pbl_app_log as usize;               // app_log
        JUMP_TABLE[47] = pebble_api::pbl_app_timer_cancel as usize;      // app_timer_cancel
        JUMP_TABLE[48] = pebble_api::pbl_app_timer_register as usize;    // app_timer_register
        JUMP_TABLE[49] = pebble_api::pbl_app_timer_reschedule as usize;  // app_timer_reschedule
        JUMP_TABLE[96] = pebble_api::pbl_fonts_get_system_font as usize; // fonts_get_system_font
        #[cfg(target_arch = "arm")]
        { JUMP_TABLE[99] = pbl_free as usize; }                        // free (debug)
        #[cfg(not(target_arch = "arm"))]
        { JUMP_TABLE[99] = libc::free as usize; }                        // free
        JUMP_TABLE[116] = pebble_api::pbl_graphics_draw_bitmap_in_rect as usize;
        JUMP_TABLE[117] = pebble_api::pbl_graphics_draw_circle as usize;
        JUMP_TABLE[118] = pebble_api::pbl_graphics_draw_line as usize;
        JUMP_TABLE[122] = pebble_api::pbl_graphics_fill_circle as usize;
        JUMP_TABLE[123] = pebble_api::pbl_graphics_fill_rect as usize;
        JUMP_TABLE[138] = pebble_api::pbl_layer_add_child as usize;
        JUMP_TABLE[139] = pebble_api::pbl_layer_create as usize;
        JUMP_TABLE[141] = pebble_api::pbl_layer_destroy as usize;
        JUMP_TABLE[142] = pebble_api::pbl_layer_get_bounds as usize;
        JUMP_TABLE[145] = pebble_api::pbl_layer_get_frame as usize;
        JUMP_TABLE[150] = pebble_api::pbl_layer_mark_dirty as usize;
        JUMP_TABLE[153] = pebble_api::pbl_layer_set_bounds as usize;
        JUMP_TABLE[155] = pebble_api::pbl_layer_set_frame as usize;
        JUMP_TABLE[157] = pebble_api::pbl_layer_set_update_proc as usize;
        #[cfg(target_arch = "arm")]
        { JUMP_TABLE[161] = pbl_malloc as usize; }                     // malloc (debug)
        #[cfg(not(target_arch = "arm"))]
        { JUMP_TABLE[161] = libc::malloc as usize; }                     // malloc
        JUMP_TABLE[162] = libc::memcpy as usize;                         // memcpy
        JUMP_TABLE[163] = libc::memmove as usize;                        // memmove
        JUMP_TABLE[164] = libc::memset as usize;                         // memset
        JUMP_TABLE[238] = pebble_api::pbl_sin_lookup as usize;            // sin_lookup
        JUMP_TABLE[239] = libc::snprintf as usize;                       // snprintf
        JUMP_TABLE[242] = libc::strcmp as usize;                         // strcmp
        JUMP_TABLE[244] = pebble_api::pbl_strftime as usize;             // strftime (Pebble-compat wrapper)
        JUMP_TABLE[245] = libc::strlen as usize;                        // strlen
        JUMP_TABLE[247] = libc::strncmp as usize;                       // strncmp
        JUMP_TABLE[262] = pebble_api::pbl_tick_timer_service_subscribe as usize;
        JUMP_TABLE[263] = pebble_api::pbl_tick_timer_service_unsubscribe as usize;
        JUMP_TABLE[271] = pebble_api::pbl_window_create as usize;
        JUMP_TABLE[272] = pebble_api::pbl_window_destroy as usize;
        JUMP_TABLE[275] = pebble_api::pbl_window_get_root_layer as usize;
        JUMP_TABLE[282] = pebble_api::pbl_window_set_window_handlers as usize;
        JUMP_TABLE[287] = pebble_api::pbl_window_stack_push as usize;
        JUMP_TABLE[309] = pebble_api::pbl_graphics_draw_text as usize;
        #[cfg(target_arch = "arm")]
        { JUMP_TABLE[318] = pbl_calloc as usize; }                    // calloc (debug)
        #[cfg(not(target_arch = "arm"))]
        { JUMP_TABLE[318] = libc::calloc as usize; }                    // calloc
        JUMP_TABLE[343] = pebble_api::pbl_gpath_draw_filled as usize;
        JUMP_TABLE[351] = pebble_api::pbl_clock_to_timestamp as usize;
        JUMP_TABLE[360] = pebble_api::pbl_i18n_get_system_locale as usize;
        JUMP_TABLE[361] = pebble_api::pbl_localeconv_r as usize;              // _localeconv_r
        JUMP_TABLE[362] = pebble_api::pbl_setlocale as usize;                 // setlocale
        JUMP_TABLE[371] = pebble_api::pbl_graphics_context_set_fill_color as usize;
        JUMP_TABLE[372] = pebble_api::pbl_graphics_context_set_stroke_color as usize;
        JUMP_TABLE[378] = pebble_api::pbl_clock_get_timezone as usize;
        JUMP_TABLE[379] = libc::localtime as usize;                     // localtime
        JUMP_TABLE[462] = pebble_api::pbl_text_layer_create as usize;
        JUMP_TABLE[463] = pebble_api::pbl_text_layer_destroy as usize;
        JUMP_TABLE[464] = pebble_api::pbl_text_layer_get_content_size as usize;
        JUMP_TABLE[465] = pebble_api::pbl_text_layer_get_layer as usize;
        JUMP_TABLE[466] = pebble_api::pbl_text_layer_get_text as usize;
        JUMP_TABLE[467] = pebble_api::pbl_text_layer_set_background_color as usize;
        JUMP_TABLE[468] = pebble_api::pbl_text_layer_set_font as usize;
        JUMP_TABLE[469] = pebble_api::pbl_text_layer_set_overflow_mode as usize;
        JUMP_TABLE[471] = pebble_api::pbl_text_layer_set_text as usize;
        JUMP_TABLE[472] = pebble_api::pbl_text_layer_set_text_alignment as usize;
        JUMP_TABLE[473] = pebble_api::pbl_text_layer_set_text_color as usize;
        JUMP_TABLE[519] = libc::time as usize;                          // time

        // Trig
        JUMP_TABLE[50] = pebble_api::pbl_atan2_lookup as usize;         // atan2_lookup
        JUMP_TABLE[70] = pebble_api::pbl_cos_lookup as usize;           // cos_lookup

        // Battery / connection
        JUMP_TABLE[53] = pebble_api::pbl_battery_state_service_peek as usize;
        JUMP_TABLE[54] = pebble_api::pbl_battery_state_service_subscribe as usize;
        JUMP_TABLE[55] = pebble_api::pbl_battery_state_service_unsubscribe as usize;
        JUMP_TABLE[63] = pebble_api::pbl_bluetooth_connection_service_peek as usize;
        JUMP_TABLE[64] = pebble_api::pbl_bluetooth_connection_service_subscribe as usize;
        JUMP_TABLE[65] = pebble_api::pbl_bluetooth_connection_service_unsubscribe as usize;
        JUMP_TABLE[566] = pebble_api::pbl_connection_service_peek_pebble_app_connection as usize;
        JUMP_TABLE[567] = pebble_api::pbl_connection_service_peek_pebblekit_connection as usize;
        JUMP_TABLE[568] = pebble_api::pbl_connection_service_subscribe as usize;
        JUMP_TABLE[569] = pebble_api::pbl_connection_service_unsubscribe as usize;

        // Window extras
        JUMP_TABLE[69] = pebble_api::pbl_clock_is_24h_style as usize;
        JUMP_TABLE[276] = pebble_api::pbl_window_is_loaded as usize;
        JUMP_TABLE[277] = pebble_api::pbl_window_set_background_color_2bit as usize;
        JUMP_TABLE[291] = pebble_api::pbl_window_get_user_data as usize;
        JUMP_TABLE[292] = pebble_api::pbl_window_set_user_data as usize;
        JUMP_TABLE[345] = pebble_api::pbl_watch_info_get_color as usize;
        JUMP_TABLE[347] = pebble_api::pbl_watch_info_get_model as usize;
        JUMP_TABLE[377] = pebble_api::pbl_window_set_background_color as usize;

        // Window navigation
        JUMP_TABLE[273] = pebble_api::pbl_window_get_click_config_provider as usize;
        JUMP_TABLE[274] = pebble_api::pbl_window_get_fullscreen as usize;
        JUMP_TABLE[278] = pebble_api::pbl_window_set_click_config_provider as usize;
        JUMP_TABLE[279] = pebble_api::pbl_window_set_click_config_provider_with_context as usize;
        JUMP_TABLE[280] = pebble_api::pbl_window_set_fullscreen as usize;
        JUMP_TABLE[281] = pebble_api::pbl_window_set_status_bar_icon as usize;
        JUMP_TABLE[285] = pebble_api::pbl_window_stack_pop as usize;
        JUMP_TABLE[286] = pebble_api::pbl_window_stack_pop_all as usize;
        JUMP_TABLE[288] = pebble_api::pbl_window_stack_remove as usize;

        // Click handling
        JUMP_TABLE[303] = pebble_api::pbl_window_long_click_subscribe as usize;
        JUMP_TABLE[304] = pebble_api::pbl_window_multi_click_subscribe as usize;
        JUMP_TABLE[305] = pebble_api::pbl_window_raw_click_subscribe as usize;
        JUMP_TABLE[306] = pebble_api::pbl_window_set_click_context as usize;
        JUMP_TABLE[307] = pebble_api::pbl_window_single_click_subscribe as usize;

        // Layer extras
        JUMP_TABLE[140] = pebble_api::pbl_layer_create_with_data as usize;
        JUMP_TABLE[144] = pebble_api::pbl_layer_get_data as usize;      // [144] not 143!
        JUMP_TABLE[146] = pebble_api::pbl_layer_get_hidden as usize;
        JUMP_TABLE[147] = pebble_api::pbl_layer_get_window as usize;
        JUMP_TABLE[148] = pebble_api::pbl_layer_insert_above_sibling as usize;
        JUMP_TABLE[149] = pebble_api::pbl_layer_insert_below_sibling as usize;
        JUMP_TABLE[151] = pebble_api::pbl_layer_remove_child_layers as usize;
        JUMP_TABLE[152] = pebble_api::pbl_layer_remove_from_parent as usize;
        JUMP_TABLE[154] = pebble_api::pbl_layer_set_clips as usize;
        JUMP_TABLE[156] = pebble_api::pbl_layer_set_hidden as usize;

        // Graphics — FIXED: [119]=draw_pixel, [120]=draw_rect, [112]=compositing_mode
        JUMP_TABLE[112] = pebble_api::pbl_graphics_context_set_compositing_mode as usize;
        JUMP_TABLE[113] = pebble_api::pbl_graphics_context_set_fill_color_2bit as usize;
        JUMP_TABLE[114] = pebble_api::pbl_graphics_context_set_stroke_color_2bit as usize;
        JUMP_TABLE[115] = pebble_api::pbl_graphics_context_set_text_color_2bit as usize;
        JUMP_TABLE[119] = pebble_api::pbl_graphics_draw_pixel as usize;
        JUMP_TABLE[120] = pebble_api::pbl_graphics_draw_rect as usize;
        JUMP_TABLE[121] = pebble_api::pbl_graphics_draw_round_rect as usize;
        JUMP_TABLE[373] = pebble_api::pbl_graphics_context_set_text_color as usize;
        JUMP_TABLE[444] = pebble_api::pbl_graphics_context_set_antialiased as usize;
        JUMP_TABLE[445] = pebble_api::pbl_graphics_context_set_stroke_width as usize;

        // Arc / radial / polar
        JUMP_TABLE[581] = pebble_api::pbl_gpoint_from_polar as usize;
        JUMP_TABLE[582] = pebble_api::pbl_graphics_draw_arc as usize;
        JUMP_TABLE[583] = pebble_api::pbl_graphics_fill_radial as usize;
        JUMP_TABLE[584] = pebble_api::pbl_grect_centered_from_polar as usize;

        // Framebuffer capture
        JUMP_TABLE[348] = pebble_api::pbl_graphics_capture_frame_buffer as usize;  // 2bit compat
        JUMP_TABLE[349] = pebble_api::pbl_graphics_frame_buffer_is_captured as usize;
        JUMP_TABLE[350] = pebble_api::pbl_graphics_release_frame_buffer as usize;
        JUMP_TABLE[394] = pebble_api::pbl_graphics_capture_frame_buffer as usize;
        JUMP_TABLE[395] = pebble_api::pbl_graphics_capture_frame_buffer_format as usize;

        // GPath
        JUMP_TABLE[105] = pebble_api::pbl_gpath_create as usize;
        JUMP_TABLE[106] = pebble_api::pbl_gpath_destroy as usize;
        JUMP_TABLE[107] = pebble_api::pbl_gpath_draw_filled as usize;   // legacy alias
        JUMP_TABLE[108] = pebble_api::pbl_gpath_draw_outline as usize;
        JUMP_TABLE[109] = pebble_api::pbl_gpath_move_to as usize;
        JUMP_TABLE[110] = pebble_api::pbl_gpath_rotate_to as usize;
        JUMP_TABLE[111] = pebble_api::pbl_gpoint_equal as usize;
        JUMP_TABLE[518] = pebble_api::pbl_gpath_draw_outline_open as usize;

        // Fonts — custom font loading
        JUMP_TABLE[97] = pebble_api::pbl_fonts_load_custom_font as usize;
        JUMP_TABLE[98] = pebble_api::pbl_fonts_unload_custom_font as usize;

        // Resource loading / GBitmap
        JUMP_TABLE[100] = pebble_api::pbl_gbitmap_create_as_sub_bitmap as usize;
        JUMP_TABLE[101] = pebble_api::pbl_gbitmap_create_with_data as usize;
        JUMP_TABLE[102] = pebble_api::pbl_gbitmap_create_with_resource as usize;
        JUMP_TABLE[103] = pebble_api::pbl_gbitmap_destroy as usize;
        JUMP_TABLE[393] = pebble_api::pbl_gbitmap_create_blank as usize;
        JUMP_TABLE[407] = pebble_api::pbl_gbitmap_get_bounds as usize;
        JUMP_TABLE[408] = pebble_api::pbl_gbitmap_get_bytes_per_row as usize;
        JUMP_TABLE[409] = pebble_api::pbl_gbitmap_get_data as usize;
        JUMP_TABLE[410] = pebble_api::pbl_gbitmap_get_format as usize;
        JUMP_TABLE[411] = pebble_api::pbl_gbitmap_get_palette as usize;
        JUMP_TABLE[414] = pebble_api::pbl_gbitmap_set_palette as usize;
        JUMP_TABLE[571] = pebble_api::pbl_gbitmap_get_data_row_info as usize;

        // Bitmap layer
        JUMP_TABLE[56] = pebble_api::pbl_bitmap_layer_create as usize;
        JUMP_TABLE[57] = pebble_api::pbl_bitmap_layer_destroy as usize;
        JUMP_TABLE[58] = pebble_api::pbl_bitmap_layer_get_layer as usize;
        JUMP_TABLE[61] = pebble_api::pbl_bitmap_layer_set_bitmap as usize;
        JUMP_TABLE[62] = pebble_api::pbl_bitmap_layer_set_compositing_mode as usize;
        JUMP_TABLE[370] = pebble_api::pbl_bitmap_layer_set_background_color as usize;

        // Scroll layer
        JUMP_TABLE[217] = pebble_api::pbl_scroll_layer_add_child as usize;
        JUMP_TABLE[218] = pebble_api::pbl_scroll_layer_create as usize;
        JUMP_TABLE[219] = pebble_api::pbl_scroll_layer_destroy as usize;
        JUMP_TABLE[220] = pebble_api::pbl_scroll_layer_get_content_offset as usize;
        JUMP_TABLE[221] = pebble_api::pbl_scroll_layer_get_content_size as usize;
        JUMP_TABLE[222] = pebble_api::pbl_scroll_layer_get_layer as usize;
        JUMP_TABLE[223] = pebble_api::pbl_scroll_layer_get_shadow_hidden as usize;
        JUMP_TABLE[224] = pebble_api::pbl_scroll_layer_scroll_down_click_handler as usize;
        JUMP_TABLE[225] = pebble_api::pbl_scroll_layer_scroll_up_click_handler as usize;
        JUMP_TABLE[226] = pebble_api::pbl_scroll_layer_set_callbacks as usize;
        JUMP_TABLE[227] = pebble_api::pbl_scroll_layer_set_click_config_onto_window as usize;
        JUMP_TABLE[228] = pebble_api::pbl_scroll_layer_set_content_offset as usize;
        JUMP_TABLE[229] = pebble_api::pbl_scroll_layer_set_content_size as usize;
        JUMP_TABLE[230] = pebble_api::pbl_scroll_layer_set_context as usize;
        JUMP_TABLE[231] = pebble_api::pbl_scroll_layer_set_frame as usize;
        JUMP_TABLE[232] = pebble_api::pbl_scroll_layer_set_shadow_hidden as usize;
        JUMP_TABLE[577] = pebble_api::pbl_scroll_layer_get_content_indicator as usize;
        JUMP_TABLE[594] = pebble_api::pbl_scroll_layer_get_paging as usize;
        JUMP_TABLE[595] = pebble_api::pbl_scroll_layer_set_paging as usize;

        // Action bar layer (legacy2)
        JUMP_TABLE[7] = pebble_api::pbl_action_bar_layer_add_to_window as usize;
        JUMP_TABLE[8] = pebble_api::pbl_action_bar_layer_clear_icon as usize;
        JUMP_TABLE[9] = pebble_api::pbl_action_bar_layer_create as usize;
        JUMP_TABLE[10] = pebble_api::pbl_action_bar_layer_destroy as usize;
        JUMP_TABLE[11] = pebble_api::pbl_action_bar_layer_get_layer as usize;
        JUMP_TABLE[12] = pebble_api::pbl_action_bar_layer_remove_from_window as usize;
        JUMP_TABLE[13] = pebble_api::pbl_action_bar_layer_set_background_color as usize;
        JUMP_TABLE[14] = pebble_api::pbl_action_bar_layer_set_click_config_provider as usize;
        JUMP_TABLE[15] = pebble_api::pbl_action_bar_layer_set_context as usize;
        JUMP_TABLE[16] = pebble_api::pbl_action_bar_layer_set_icon as usize;
        // Action bar layer (modern)
        JUMP_TABLE[446] = pebble_api::pbl_action_bar_layer_add_to_window as usize;
        JUMP_TABLE[447] = pebble_api::pbl_action_bar_layer_clear_icon as usize;
        JUMP_TABLE[448] = pebble_api::pbl_action_bar_layer_create as usize;
        JUMP_TABLE[449] = pebble_api::pbl_action_bar_layer_destroy as usize;
        JUMP_TABLE[450] = pebble_api::pbl_action_bar_layer_get_layer as usize;
        JUMP_TABLE[451] = pebble_api::pbl_action_bar_layer_remove_from_window as usize;
        JUMP_TABLE[452] = pebble_api::pbl_action_bar_layer_set_background_color as usize;
        JUMP_TABLE[453] = pebble_api::pbl_action_bar_layer_set_click_config_provider as usize;
        JUMP_TABLE[454] = pebble_api::pbl_action_bar_layer_set_context as usize;
        JUMP_TABLE[455] = pebble_api::pbl_action_bar_layer_set_icon as usize;
        JUMP_TABLE[456] = pebble_api::pbl_action_bar_layer_set_icon_animated as usize;
        JUMP_TABLE[461] = pebble_api::pbl_action_bar_layer_set_icon_press_animation as usize;

        // Menu layer (legacy2 + modern)
        JUMP_TABLE[169] = pebble_api::pbl_menu_layer_create as usize;
        JUMP_TABLE[439] = pebble_api::pbl_menu_layer_create as usize;
        JUMP_TABLE[170] = pebble_api::pbl_menu_layer_destroy as usize;
        JUMP_TABLE[171] = pebble_api::pbl_menu_layer_get_layer as usize;
        JUMP_TABLE[172] = pebble_api::pbl_menu_layer_get_scroll_layer as usize;
        JUMP_TABLE[173] = pebble_api::pbl_menu_layer_get_selected_index as usize;
        JUMP_TABLE[174] = pebble_api::pbl_menu_layer_reload_data as usize;
        JUMP_TABLE[175] = pebble_api::pbl_menu_layer_set_callbacks as usize;
        JUMP_TABLE[320] = pebble_api::pbl_menu_layer_set_callbacks as usize;
        JUMP_TABLE[522] = pebble_api::pbl_menu_layer_set_callbacks as usize;
        JUMP_TABLE[176] = pebble_api::pbl_menu_layer_set_click_config_onto_window as usize;
        JUMP_TABLE[177] = pebble_api::pbl_menu_layer_set_selected_index as usize;
        JUMP_TABLE[178] = pebble_api::pbl_menu_layer_set_selected_next as usize;
        JUMP_TABLE[520] = pebble_api::pbl_menu_layer_set_highlight_colors as usize;
        JUMP_TABLE[521] = pebble_api::pbl_menu_layer_set_normal_colors as usize;
        JUMP_TABLE[523] = pebble_api::pbl_menu_layer_pad_bottom_enable as usize;
        JUMP_TABLE[578] = pebble_api::pbl_menu_layer_get_center_focused as usize;
        JUMP_TABLE[579] = pebble_api::pbl_menu_layer_set_center_focused as usize;
        JUMP_TABLE[598] = pebble_api::pbl_menu_layer_is_index_selected as usize;

        // Simple menu layer
        JUMP_TABLE[233] = pebble_api::pbl_simple_menu_layer_create as usize;
        JUMP_TABLE[234] = pebble_api::pbl_simple_menu_layer_destroy as usize;
        JUMP_TABLE[235] = pebble_api::pbl_simple_menu_layer_get_layer as usize;
        JUMP_TABLE[236] = pebble_api::pbl_simple_menu_layer_get_selected_index as usize;
        JUMP_TABLE[237] = pebble_api::pbl_simple_menu_layer_set_selected_index as usize;
        JUMP_TABLE[316] = pebble_api::pbl_simple_menu_layer_get_menu_layer as usize;

        // Status bar layer
        JUMP_TABLE[524] = pebble_api::pbl_status_bar_layer_create as usize;
        JUMP_TABLE[525] = pebble_api::pbl_status_bar_layer_destroy as usize;
        JUMP_TABLE[526] = pebble_api::pbl_status_bar_layer_get_background_color as usize;
        JUMP_TABLE[527] = pebble_api::pbl_status_bar_layer_get_foreground_color as usize;
        JUMP_TABLE[528] = pebble_api::pbl_status_bar_layer_get_layer as usize;
        JUMP_TABLE[529] = pebble_api::pbl_status_bar_layer_set_colors as usize;
        JUMP_TABLE[530] = pebble_api::pbl_status_bar_layer_set_separator_mode as usize;

        // Number window
        JUMP_TABLE[179] = pebble_api::pbl_number_window_create as usize;
        JUMP_TABLE[180] = pebble_api::pbl_number_window_destroy as usize;
        JUMP_TABLE[181] = pebble_api::pbl_number_window_get_value as usize;
        JUMP_TABLE[182] = pebble_api::pbl_number_window_set_label as usize;
        JUMP_TABLE[183] = pebble_api::pbl_number_window_set_max as usize;
        JUMP_TABLE[184] = pebble_api::pbl_number_window_set_min as usize;
        JUMP_TABLE[185] = pebble_api::pbl_number_window_set_step_size as usize;
        JUMP_TABLE[186] = pebble_api::pbl_number_window_set_value as usize;
        JUMP_TABLE[322] = pebble_api::pbl_number_window_get_window as usize;

        // GRect helpers
        JUMP_TABLE[127] = pebble_api::pbl_grect_center_point as usize;
        JUMP_TABLE[128] = pebble_api::pbl_grect_clip as usize;
        JUMP_TABLE[129] = pebble_api::pbl_grect_contains_point as usize;
        JUMP_TABLE[130] = pebble_api::pbl_grect_crop as usize;
        JUMP_TABLE[131] = pebble_api::pbl_grect_equal as usize;
        JUMP_TABLE[132] = pebble_api::pbl_grect_is_empty as usize;
        JUMP_TABLE[126] = pebble_api::pbl_grect_align as usize;
        JUMP_TABLE[133] = pebble_api::pbl_grect_standardize as usize;
        JUMP_TABLE[134] = pebble_api::pbl_gsize_equal as usize;

        // Color helpers
        JUMP_TABLE[533] = pebble_api::pbl_gcolor_legible_over as usize;
        JUMP_TABLE[613] = pebble_api::pbl_gcolor_equal as usize;
        JUMP_TABLE[580] = pebble_api::pbl_grect_inset as usize;

        // Light / vibes (no-ops)
        JUMP_TABLE[158] = pebble_api::pbl_light_enable as usize;
        JUMP_TABLE[159] = pebble_api::pbl_light_enable_interaction as usize;
        JUMP_TABLE[266] = pebble_api::pbl_vibes_cancel as usize;
        JUMP_TABLE[267] = pebble_api::pbl_vibes_double_pulse as usize;
        JUMP_TABLE[268] = pebble_api::pbl_vibes_enqueue_custom_pattern as usize;
        JUMP_TABLE[269] = pebble_api::pbl_vibes_long_pulse as usize;
        JUMP_TABLE[270] = pebble_api::pbl_vibes_short_pulse as usize;

        // Persist
        JUMP_TABLE[187] = pebble_api::pbl_persist_delete as usize;
        JUMP_TABLE[188] = pebble_api::pbl_persist_exists as usize;
        JUMP_TABLE[189] = pebble_api::pbl_persist_get_size as usize;
        JUMP_TABLE[190] = pebble_api::pbl_persist_read_bool as usize;
        JUMP_TABLE[191] = pebble_api::pbl_persist_read_data_deprecated as usize;   // deprecated
        JUMP_TABLE[192] = pebble_api::pbl_persist_read_int as usize;
        JUMP_TABLE[194] = pebble_api::pbl_persist_write_bool as usize;
        JUMP_TABLE[195] = pebble_api::pbl_persist_write_data_deprecated as usize;  // deprecated
        JUMP_TABLE[196] = pebble_api::pbl_persist_write_int as usize;
        JUMP_TABLE[197] = pebble_api::pbl_persist_write_string as usize;
        JUMP_TABLE[311] = pebble_api::pbl_persist_read_data as usize;   // modern
        JUMP_TABLE[312] = pebble_api::pbl_persist_read_string as usize;
        JUMP_TABLE[313] = pebble_api::pbl_persist_write_data as usize;  // modern

        // Health (stubs)
        JUMP_TABLE[601] = pebble_api::pbl_health_service_events_subscribe as usize;
        JUMP_TABLE[604] = pebble_api::pbl_health_service_metric_accessible as usize;
        JUMP_TABLE[607] = pebble_api::pbl_health_service_sum_today as usize;
        JUMP_TABLE[608] = pebble_api::pbl_time_start_of_today as usize;
        JUMP_TABLE[620] = pebble_api::pbl_health_service_peek_current_value as usize;
        JUMP_TABLE[622] = pebble_api::pbl_layer_get_unobstructed_bounds as usize;
        JUMP_TABLE[624] = pebble_api::pbl_unobstructed_area_service_subscribe as usize;
        JUMP_TABLE[631] = pebble_api::pbl_quiet_time_is_active as usize;

        // Resource API
        JUMP_TABLE[206] = pebble_api::pbl_resource_get_handle as usize;
        JUMP_TABLE[207] = pebble_api::pbl_resource_load as usize;
        JUMP_TABLE[208] = pebble_api::pbl_resource_load_byte_range as usize;
        JUMP_TABLE[209] = pebble_api::pbl_resource_size as usize;

        // Accelerometer
        JUMP_TABLE[0] = crate::accel::pbl_accel_data_service_subscribe as usize; // deprecated
        JUMP_TABLE[1] = crate::accel::pbl_accel_data_service_unsubscribe as usize;
        JUMP_TABLE[2] = crate::accel::pbl_accel_service_peek as usize;
        JUMP_TABLE[3] = crate::accel::pbl_accel_service_set_samples_per_update as usize;
        JUMP_TABLE[4] = crate::accel::pbl_accel_service_set_sampling_rate as usize;
        JUMP_TABLE[317] = crate::accel::pbl_accel_data_service_subscribe as usize; // modern

        // Animation (modern, indices 380-437)
        JUMP_TABLE[380] = pebble_api::pbl_animation_create as usize;
        JUMP_TABLE[381] = pebble_api::pbl_animation_destroy as usize;
        JUMP_TABLE[382] = pebble_api::pbl_animation_get_context as usize;
        JUMP_TABLE[383] = pebble_api::pbl_animation_is_scheduled as usize;
        JUMP_TABLE[384] = pebble_api::pbl_animation_schedule as usize;
        JUMP_TABLE[385] = pebble_api::pbl_animation_set_curve as usize;
        JUMP_TABLE[386] = pebble_api::pbl_animation_set_custom_curve as usize;
        JUMP_TABLE[387] = pebble_api::pbl_animation_set_delay as usize;
        JUMP_TABLE[388] = pebble_api::pbl_animation_set_duration as usize;
        JUMP_TABLE[389] = pebble_api::pbl_animation_set_handlers as usize;
        JUMP_TABLE[390] = pebble_api::pbl_animation_set_implementation as usize;
        JUMP_TABLE[391] = pebble_api::pbl_animation_unschedule as usize;
        JUMP_TABLE[392] = pebble_api::pbl_animation_unschedule_all as usize;
        JUMP_TABLE[396] = pebble_api::pbl_property_animation_create as usize;
        JUMP_TABLE[397] = pebble_api::pbl_property_animation_create_layer_frame as usize;
        JUMP_TABLE[398] = pebble_api::pbl_property_animation_destroy as usize;
        JUMP_TABLE[399] = pebble_api::pbl_property_animation_from as usize;
        JUMP_TABLE[400] = pebble_api::pbl_property_animation_get_animation as usize;
        JUMP_TABLE[401] = pebble_api::pbl_property_animation_subject as usize;
        JUMP_TABLE[402] = pebble_api::pbl_property_animation_to as usize;
        JUMP_TABLE[403] = pebble_api::pbl_property_animation_update_gpoint as usize;
        JUMP_TABLE[404] = pebble_api::pbl_property_animation_update_grect as usize;
        JUMP_TABLE[405] = pebble_api::pbl_property_animation_update_int16 as usize;
        JUMP_TABLE[422] = pebble_api::pbl_animation_clone as usize;
        JUMP_TABLE[423] = pebble_api::pbl_animation_get_delay as usize;
        JUMP_TABLE[424] = pebble_api::pbl_animation_get_duration as usize;
        JUMP_TABLE[425] = pebble_api::pbl_animation_get_play_count as usize;
        JUMP_TABLE[426] = pebble_api::pbl_animation_get_elapsed as usize;
        JUMP_TABLE[427] = pebble_api::pbl_animation_get_reverse as usize;
        JUMP_TABLE[428] = pebble_api::pbl_animation_sequence_create as usize;
        JUMP_TABLE[429] = pebble_api::pbl_animation_sequence_create_from_array as usize;
        JUMP_TABLE[430] = pebble_api::pbl_animation_set_play_count as usize;
        JUMP_TABLE[431] = pebble_api::pbl_animation_set_elapsed as usize;
        JUMP_TABLE[432] = pebble_api::pbl_animation_set_reverse as usize;
        JUMP_TABLE[433] = pebble_api::pbl_animation_spawn_create as usize;
        JUMP_TABLE[434] = pebble_api::pbl_animation_spawn_create_from_array as usize;
        JUMP_TABLE[435] = pebble_api::pbl_animation_get_curve as usize;
        JUMP_TABLE[436] = pebble_api::pbl_animation_get_custom_curve as usize;
        JUMP_TABLE[437] = pebble_api::pbl_animation_get_implementation as usize;
        JUMP_TABLE[516] = pebble_api::pbl_property_animation_create_bounds_origin as usize;
        JUMP_TABLE[517] = pebble_api::pbl_property_animation_update_uint32 as usize;
        JUMP_TABLE[534] = pebble_api::pbl_property_animation_update_gcolor8 as usize;

        // Animation legacy (indices 17-28)
        JUMP_TABLE[17] = pebble_api::pbl_animation_legacy2_create as usize;
        JUMP_TABLE[18] = pebble_api::pbl_animation_legacy2_destroy as usize;
        JUMP_TABLE[19] = pebble_api::pbl_animation_legacy2_get_context as usize;
        JUMP_TABLE[20] = pebble_api::pbl_animation_legacy2_is_scheduled as usize;
        JUMP_TABLE[21] = pebble_api::pbl_animation_legacy2_schedule as usize;
        JUMP_TABLE[22] = pebble_api::pbl_animation_legacy2_set_curve as usize;
        JUMP_TABLE[23] = pebble_api::pbl_animation_legacy2_set_delay as usize;
        JUMP_TABLE[24] = pebble_api::pbl_animation_legacy2_set_duration as usize;
        JUMP_TABLE[25] = pebble_api::pbl_animation_legacy2_set_handlers as usize;
        JUMP_TABLE[26] = pebble_api::pbl_animation_legacy2_set_implementation as usize;
        JUMP_TABLE[27] = pebble_api::pbl_animation_legacy2_unschedule as usize;
        JUMP_TABLE[28] = pebble_api::pbl_animation_legacy2_unschedule_all as usize;
        JUMP_TABLE[198] = pebble_api::pbl_property_animation_legacy2_create as usize;
        JUMP_TABLE[199] = pebble_api::pbl_property_animation_legacy2_create_layer_frame as usize;
        JUMP_TABLE[200] = pebble_api::pbl_property_animation_legacy2_destroy as usize;
        JUMP_TABLE[201] = pebble_api::pbl_property_animation_legacy2_update_gpoint as usize;
        JUMP_TABLE[202] = pebble_api::pbl_property_animation_legacy2_update_grect as usize;
        JUMP_TABLE[203] = pebble_api::pbl_property_animation_legacy2_update_int16 as usize;
        JUMP_TABLE[344] = pebble_api::pbl_animation_legacy2_set_custom_curve as usize;

        // App Message
        JUMP_TABLE[35] = pebble_api::pbl_app_message_deregister_callbacks as usize;
        JUMP_TABLE[36] = pebble_api::pbl_app_message_open as usize;
        JUMP_TABLE[293] = pebble_api::pbl_app_message_get_context as usize;
        JUMP_TABLE[294] = pebble_api::pbl_app_message_inbox_size_maximum as usize;
        JUMP_TABLE[295] = pebble_api::pbl_app_message_outbox_begin as usize;
        JUMP_TABLE[296] = pebble_api::pbl_app_message_outbox_send as usize;
        JUMP_TABLE[297] = pebble_api::pbl_app_message_outbox_size_maximum as usize;
        JUMP_TABLE[298] = pebble_api::pbl_app_message_register_inbox_dropped as usize;
        JUMP_TABLE[299] = pebble_api::pbl_app_message_register_inbox_received as usize;
        JUMP_TABLE[300] = pebble_api::pbl_app_message_register_outbox_failed as usize;
        JUMP_TABLE[301] = pebble_api::pbl_app_message_register_outbox_sent as usize;
        JUMP_TABLE[302] = pebble_api::pbl_app_message_set_context as usize;

        // App Sync
        JUMP_TABLE[43] = pebble_api::pbl_app_sync_deinit as usize;
        JUMP_TABLE[44] = pebble_api::pbl_app_sync_get as usize;
        JUMP_TABLE[45] = pebble_api::pbl_app_sync_init as usize;
        JUMP_TABLE[46] = pebble_api::pbl_app_sync_set as usize;

        // Dictionary
        JUMP_TABLE[74] = pebble_api::pbl_dict_calc_buffer_size as usize;
        JUMP_TABLE[84] = pebble_api::pbl_dict_write_begin as usize;
        JUMP_TABLE[85] = pebble_api::pbl_dict_write_cstring as usize;
        JUMP_TABLE[86] = pebble_api::pbl_dict_write_data as usize;
        JUMP_TABLE[87] = pebble_api::pbl_dict_write_end as usize;
        JUMP_TABLE[88] = pebble_api::pbl_dict_write_int as usize;
        JUMP_TABLE[89] = pebble_api::pbl_dict_write_int16 as usize;
        JUMP_TABLE[90] = pebble_api::pbl_dict_write_int32 as usize;
        JUMP_TABLE[91] = pebble_api::pbl_dict_write_int8 as usize;
        JUMP_TABLE[92] = pebble_api::pbl_dict_write_tuplet as usize;
        JUMP_TABLE[93] = pebble_api::pbl_dict_write_uint16 as usize;
        JUMP_TABLE[94] = pebble_api::pbl_dict_write_uint32 as usize;
        JUMP_TABLE[95] = pebble_api::pbl_dict_write_uint8 as usize;

        // Libc aliases
        JUMP_TABLE[51] = libc::atoi as usize;                              // atoi
        JUMP_TABLE[52] = libc::atol as usize;                              // atol
        JUMP_TABLE[104] = libc::gmtime as usize;                           // gmtime
        JUMP_TABLE[160] = libc::localtime as usize;                        // localtime__deprecated
        JUMP_TABLE[205] = libc::rand as usize;                             // rand
        JUMP_TABLE[240] = libc::srand as usize;                            // srand
        JUMP_TABLE[241] = libc::strcat as usize;                           // strcat
        JUMP_TABLE[243] = libc::strcpy as usize;                           // strcpy
        JUMP_TABLE[246] = libc::strncat as usize;                          // strncat
        JUMP_TABLE[248] = libc::strncpy as usize;                          // strncpy
        JUMP_TABLE[264] = libc::time as usize;                             // time__deprecated
        #[cfg(target_arch = "arm")]
        { JUMP_TABLE[323] = pbl_realloc as usize; }                    // realloc (debug)
        #[cfg(not(target_arch = "arm"))]
        { JUMP_TABLE[323] = libc::realloc as usize; }                    // realloc
        JUMP_TABLE[363] = libc::mktime as usize;                           // mktime
        JUMP_TABLE[531] = libc::difftime as usize;                         // difftime

        // New Rust stubs
        JUMP_TABLE[125] = pebble_api::pbl_graphics_text_layout_get_max_used_size as usize;
        JUMP_TABLE[143] = pebble_api::pbl_layer_get_clips as usize;
        JUMP_TABLE[265] = pebble_api::pbl_time_ms as usize;                // time_ms_deprecated
        JUMP_TABLE[532] = pebble_api::pbl_time_ms as usize;                // time_ms
        JUMP_TABLE[283] = pebble_api::pbl_window_stack_contains_window as usize;
        JUMP_TABLE[284] = pebble_api::pbl_window_stack_get_top_window as usize;
        JUMP_TABLE[289] = pebble_api::pbl_app_focus_service_subscribe as usize;
        JUMP_TABLE[290] = pebble_api::pbl_app_focus_service_unsubscribe as usize;
        JUMP_TABLE[535] = pebble_api::pbl_app_focus_service_subscribe_handlers as usize;
        JUMP_TABLE[315] = pebble_api::pbl_graphics_text_layout_get_content_size as usize;
        JUMP_TABLE[591] = pebble_api::pbl_graphics_text_layout_get_content_size as usize;
        JUMP_TABLE[59] = pebble_api::pbl_bitmap_layer_set_alignment as usize;
        JUMP_TABLE[60] = pebble_api::pbl_bitmap_layer_set_background_color_2bit as usize;
        JUMP_TABLE[319] = pebble_api::pbl_bitmap_layer_get_bitmap as usize;
        JUMP_TABLE[68] = pebble_api::pbl_clock_copy_time_string as usize;
        JUMP_TABLE[66] = pebble_api::pbl_click_number_of_clicks_counted as usize;
        JUMP_TABLE[67] = pebble_api::pbl_click_recognizer_get_button_id as usize;

        // Text layer legacy2 — delegate to modern text_layer functions
        JUMP_TABLE[249] = pebble_api::pbl_text_layer_create as usize;
        JUMP_TABLE[250] = pebble_api::pbl_text_layer_destroy as usize;
        JUMP_TABLE[251] = pebble_api::pbl_text_layer_get_content_size as usize;
        JUMP_TABLE[252] = pebble_api::pbl_text_layer_get_layer as usize;
        JUMP_TABLE[253] = pebble_api::pbl_text_layer_get_text as usize;
        JUMP_TABLE[254] = pebble_api::pbl_text_layer_set_background_color as usize;  // 2bit variant
        JUMP_TABLE[255] = pebble_api::pbl_text_layer_set_font as usize;
        JUMP_TABLE[256] = pebble_api::pbl_text_layer_set_overflow_mode as usize;
        JUMP_TABLE[257] = pebble_api::pbl_text_layer_set_size as usize;
        JUMP_TABLE[258] = pebble_api::pbl_text_layer_set_text as usize;
        JUMP_TABLE[259] = pebble_api::pbl_text_layer_set_text_alignment as usize;
        JUMP_TABLE[260] = pebble_api::pbl_text_layer_set_text_color as usize;  // 2bit variant

        // Menu cell drawing
        JUMP_TABLE[165] = pebble_api::pbl_menu_cell_basic_draw as usize;
        JUMP_TABLE[166] = pebble_api::pbl_menu_cell_basic_header_draw as usize;
        JUMP_TABLE[167] = pebble_api::pbl_menu_cell_title_draw as usize;
        JUMP_TABLE[168] = pebble_api::pbl_menu_index_compare as usize;
        JUMP_TABLE[459] = pebble_api::pbl_menu_cell_layer_is_highlighted as usize;

        // Dictionary read/find
        JUMP_TABLE[76] = pebble_api::pbl_dict_find as usize;
        JUMP_TABLE[77] = pebble_api::pbl_dict_merge as usize;
        JUMP_TABLE[78] = pebble_api::pbl_dict_read_begin_from_buffer as usize;
        JUMP_TABLE[79] = pebble_api::pbl_dict_read_first as usize;
        JUMP_TABLE[80] = pebble_api::pbl_dict_read_next as usize;
        JUMP_TABLE[74] = pebble_api::pbl_dict_calc_buffer_size as usize;
        JUMP_TABLE[75] = pebble_api::pbl_dict_calc_buffer_size_from_tuplets as usize;
        JUMP_TABLE[81] = pebble_api::pbl_dict_serialize_tuplets as usize;
        JUMP_TABLE[82] = pebble_api::pbl_dict_serialize_tuplets_to_buffer as usize;
        JUMP_TABLE[83] = pebble_api::pbl_dict_serialize_tuplets_to_buffer_with_iter as usize;

        // Inverter layer
        JUMP_TABLE[135] = pebble_api::pbl_inverter_layer_create as usize;
        JUMP_TABLE[136] = pebble_api::pbl_inverter_layer_destroy as usize;
        JUMP_TABLE[137] = pebble_api::pbl_inverter_layer_get_layer as usize;

        // Rot bitmap layer
        JUMP_TABLE[210] = pebble_api::pbl_rot_bitmap_layer_create as usize;
        JUMP_TABLE[211] = pebble_api::pbl_rot_bitmap_layer_destroy as usize;
        JUMP_TABLE[212] = pebble_api::pbl_rot_bitmap_layer_increment_angle as usize;
        JUMP_TABLE[213] = pebble_api::pbl_rot_bitmap_layer_set_angle as usize;
        JUMP_TABLE[214] = pebble_api::pbl_rot_bitmap_layer_set_corner_clip_color as usize;
        JUMP_TABLE[215] = pebble_api::pbl_rot_bitmap_set_compositing_mode as usize;
        JUMP_TABLE[216] = pebble_api::pbl_rot_bitmap_set_src_ic as usize;

        // Compass service
        JUMP_TABLE[337] = pebble_api::pbl_compass_service_peek as usize;
        JUMP_TABLE[338] = pebble_api::pbl_compass_service_set_heading_filter as usize;
        JUMP_TABLE[339] = pebble_api::pbl_compass_service_subscribe as usize;
        JUMP_TABLE[340] = pebble_api::pbl_compass_service_unsubscribe as usize;

        // Content indicator
        JUMP_TABLE[572] = pebble_api::pbl_content_indicator_configure_direction as usize;
        JUMP_TABLE[573] = pebble_api::pbl_content_indicator_create as usize;
        JUMP_TABLE[574] = pebble_api::pbl_content_indicator_destroy as usize;
        JUMP_TABLE[575] = pebble_api::pbl_content_indicator_get_content_available as usize;
        JUMP_TABLE[576] = pebble_api::pbl_content_indicator_set_content_available as usize;

        // Launch
        JUMP_TABLE[352] = pebble_api::pbl_launch_reason as usize;
        JUMP_TABLE[438] = pebble_api::pbl_launch_get_args as usize;

        // UUID
        JUMP_TABLE[341] = pebble_api::pbl_uuid_equal as usize;
        JUMP_TABLE[342] = pebble_api::pbl_uuid_to_string as usize;

        // Heap
        JUMP_TABLE[335] = pebble_api::pbl_heap_bytes_free as usize;
        JUMP_TABLE[336] = pebble_api::pbl_heap_bytes_used as usize;

        // psleep
        JUMP_TABLE[204] = pebble_api::pbl_psleep as usize;

        // Watch info
        JUMP_TABLE[346] = pebble_api::pbl_watch_info_get_firmware_version as usize;

        // Clock timezone
        JUMP_TABLE[359] = pebble_api::pbl_clock_is_timezone_set as usize;

        // Exit reason
        JUMP_TABLE[616] = pebble_api::pbl_exit_reason_set as usize;

        // Preferred content size / display duration
        JUMP_TABLE[623] = pebble_api::pbl_preferred_result_display_duration as usize;
        JUMP_TABLE[630] = pebble_api::pbl_preferred_content_size as usize;

        // Click helpers
        JUMP_TABLE[308] = pebble_api::pbl_window_single_repeating_click_subscribe as usize;
        JUMP_TABLE[321] = pebble_api::pbl_window_get_click_config_context as usize;
        JUMP_TABLE[325] = pebble_api::pbl_click_recognizer_is_repeating as usize;

        // Data logging
        JUMP_TABLE[71] = pebble_api::pbl_data_logging_create as usize;
        JUMP_TABLE[72] = pebble_api::pbl_data_logging_finish as usize;
        JUMP_TABLE[73] = pebble_api::pbl_data_logging_log as usize;

        // Wakeup
        JUMP_TABLE[353] = pebble_api::pbl_wakeup_cancel as usize;
        JUMP_TABLE[354] = pebble_api::pbl_wakeup_cancel_all as usize;
        JUMP_TABLE[355] = pebble_api::pbl_wakeup_get_launch_event as usize;
        JUMP_TABLE[356] = pebble_api::pbl_wakeup_query as usize;
        JUMP_TABLE[357] = pebble_api::pbl_wakeup_schedule as usize;
        JUMP_TABLE[358] = pebble_api::pbl_wakeup_service_subscribe as usize;

        // Dictation
        JUMP_TABLE[550] = pebble_api::pbl_dictation_session_create as usize;
        JUMP_TABLE[551] = pebble_api::pbl_dictation_session_destroy as usize;
        JUMP_TABLE[552] = pebble_api::pbl_dictation_session_enable_confirmation as usize;
        JUMP_TABLE[553] = pebble_api::pbl_dictation_session_start as usize;
        JUMP_TABLE[554] = pebble_api::pbl_dictation_session_stop as usize;
        JUMP_TABLE[570] = pebble_api::pbl_dictation_session_enable_error_dialogs as usize;

        // Smartstrap
        JUMP_TABLE[555] = pebble_api::pbl_smartstrap_attribute_begin_write as usize;
        JUMP_TABLE[556] = pebble_api::pbl_smartstrap_attribute_create as usize;
        JUMP_TABLE[557] = pebble_api::pbl_smartstrap_attribute_destroy as usize;
        JUMP_TABLE[558] = pebble_api::pbl_smartstrap_attribute_end_write as usize;
        JUMP_TABLE[559] = pebble_api::pbl_smartstrap_attribute_get_attribute_id as usize;
        JUMP_TABLE[560] = pebble_api::pbl_smartstrap_attribute_get_service_id as usize;
        JUMP_TABLE[561] = pebble_api::pbl_smartstrap_attribute_read as usize;
        JUMP_TABLE[562] = pebble_api::pbl_smartstrap_service_is_available as usize;
        JUMP_TABLE[563] = pebble_api::pbl_smartstrap_set_timeout as usize;
        JUMP_TABLE[564] = pebble_api::pbl_smartstrap_subscribe as usize;
        JUMP_TABLE[565] = pebble_api::pbl_smartstrap_unsubscribe as usize;

        // App comm / worker
        JUMP_TABLE[29] = pebble_api::pbl_app_comm_get_sniff_interval as usize;
        JUMP_TABLE[30] = pebble_api::pbl_app_comm_set_sniff_interval as usize;
        JUMP_TABLE[327] = pebble_api::pbl_app_worker_is_running as usize;
        JUMP_TABLE[328] = pebble_api::pbl_app_worker_kill as usize;
        JUMP_TABLE[329] = pebble_api::pbl_app_worker_launch as usize;
        JUMP_TABLE[330] = pebble_api::pbl_app_worker_message_subscribe as usize;
        JUMP_TABLE[331] = pebble_api::pbl_app_worker_message_unsubscribe as usize;
        JUMP_TABLE[332] = pebble_api::pbl_app_worker_send_message as usize;
        JUMP_TABLE[333] = pebble_api::pbl_worker_event_loop as usize;
        JUMP_TABLE[334] = pebble_api::pbl_worker_launch_app as usize;

        // Health (remaining)
        JUMP_TABLE[599] = pebble_api::pbl_health_service_activities_iterate as usize;
        JUMP_TABLE[600] = pebble_api::pbl_health_service_any_activity_accessible as usize;
        JUMP_TABLE[602] = pebble_api::pbl_health_service_events_unsubscribe as usize;
        JUMP_TABLE[603] = pebble_api::pbl_health_service_get_minute_history as usize;
        JUMP_TABLE[605] = pebble_api::pbl_health_service_peek_current_activities as usize;
        JUMP_TABLE[606] = pebble_api::pbl_health_service_sum as usize;
        JUMP_TABLE[609] = pebble_api::pbl_health_service_metric_averaged_accessible as usize;
        JUMP_TABLE[610] = pebble_api::pbl_health_service_sum_averaged as usize;
        JUMP_TABLE[611] = pebble_api::pbl_health_service_metric_accessible as usize;  // measurement_system
        JUMP_TABLE[617] = pebble_api::pbl_health_service_sum_averaged as usize;  // aggregate_averaged
        JUMP_TABLE[618] = pebble_api::pbl_health_service_events_unsubscribe as usize;  // cancel_metric_alert
        JUMP_TABLE[619] = pebble_api::pbl_health_service_metric_averaged_accessible as usize;
        JUMP_TABLE[621] = pebble_api::pbl_health_service_register_metric_alert as usize;
        JUMP_TABLE[628] = pebble_api::pbl_health_service_peek_current_value as usize;  // hr_period_expiration
        JUMP_TABLE[629] = pebble_api::pbl_health_service_set_heart_rate_sample_period as usize;

        // Accel tap
        JUMP_TABLE[6] = pebble_api::pbl_accel_tap_service_unsubscribe as usize;
        JUMP_TABLE[326] = pebble_api::pbl_accel_raw_data_service_subscribe as usize;

        // Action menu
        JUMP_TABLE[536] = pebble_api::pbl_action_menu_close as usize;
        JUMP_TABLE[537] = pebble_api::pbl_action_menu_freeze as usize;
        JUMP_TABLE[538] = pebble_api::pbl_action_menu_get_context as usize;
        JUMP_TABLE[539] = pebble_api::pbl_action_menu_get_root_level as usize;
        JUMP_TABLE[540] = pebble_api::pbl_action_menu_hierarchy_destroy as usize;
        JUMP_TABLE[541] = pebble_api::pbl_action_menu_get_context as usize;  // item_get_action_data — stub
        JUMP_TABLE[542] = pebble_api::pbl_action_menu_get_context as usize;  // item_get_label — stub
        JUMP_TABLE[543] = pebble_api::pbl_action_menu_level_add_action as usize;
        JUMP_TABLE[544] = pebble_api::pbl_action_menu_level_add_child as usize;
        JUMP_TABLE[545] = pebble_api::pbl_action_menu_level_create as usize;
        JUMP_TABLE[546] = pebble_api::pbl_action_menu_level_set_display_mode as usize;
        JUMP_TABLE[547] = pebble_api::pbl_action_menu_open as usize;
        JUMP_TABLE[548] = pebble_api::pbl_action_menu_set_result_window as usize;
        JUMP_TABLE[549] = pebble_api::pbl_action_menu_unfreeze as usize;

        // GBitmap extended
        JUMP_TABLE[324] = pebble_api::pbl_gbitmap_create_blank_2bit as usize;
        JUMP_TABLE[406] = pebble_api::pbl_gbitmap_create_blank_with_palette as usize;
        JUMP_TABLE[412] = pebble_api::pbl_gbitmap_set_bounds as usize;
        JUMP_TABLE[413] = pebble_api::pbl_gbitmap_set_data as usize;
        JUMP_TABLE[414] = pebble_api::pbl_gbitmap_set_palette as usize;

        // GBitmap sequence
        JUMP_TABLE[415] = pebble_api::pbl_gbitmap_sequence_create_with_resource as usize;
        JUMP_TABLE[416] = pebble_api::pbl_gbitmap_sequence_destroy as usize;
        JUMP_TABLE[417] = pebble_api::pbl_gbitmap_sequence_get_bitmap_size as usize;
        JUMP_TABLE[418] = pebble_api::pbl_gbitmap_sequence_get_current_frame_idx as usize;
        JUMP_TABLE[419] = pebble_api::pbl_gbitmap_sequence_get_total_num_frames as usize;
        JUMP_TABLE[420] = pebble_api::pbl_gbitmap_sequence_update_bitmap_next_frame as usize;
        JUMP_TABLE[441] = pebble_api::pbl_gbitmap_sequence_get_play_count as usize;
        JUMP_TABLE[442] = pebble_api::pbl_gbitmap_sequence_restart as usize;
        JUMP_TABLE[443] = pebble_api::pbl_gbitmap_sequence_set_play_count as usize;
        JUMP_TABLE[457] = pebble_api::pbl_gbitmap_sequence_update_bitmap_by_elapsed as usize;

        // Graphics text attributes
        JUMP_TABLE[585] = pebble_api::pbl_graphics_text_attributes_create as usize;
        JUMP_TABLE[586] = pebble_api::pbl_graphics_text_attributes_destroy as usize;
        JUMP_TABLE[587] = pebble_api::pbl_graphics_text_attributes_enable_paging as usize;
        JUMP_TABLE[588] = pebble_api::pbl_graphics_text_attributes_enable_screen_text_flow as usize;
        JUMP_TABLE[589] = pebble_api::pbl_graphics_text_attributes_restore_default_paging as usize;
        JUMP_TABLE[590] = pebble_api::pbl_graphics_text_attributes_restore_default_text_flow as usize;

        // Graphics draw rotated bitmap
        JUMP_TABLE[460] = pebble_api::pbl_graphics_draw_rotated_bitmap as usize;

        // Persist read string deprecated
        JUMP_TABLE[193] = pebble_api::pbl_persist_read_string_deprecated as usize;

        // Layer coordinate conversion
        JUMP_TABLE[592] = pebble_api::pbl_layer_convert_point_to_screen as usize;
        JUMP_TABLE[593] = pebble_api::pbl_layer_convert_rect_to_screen as usize;

        // Unobstructed area unsubscribe
        JUMP_TABLE[625] = pebble_api::pbl_unobstructed_area_service_unsubscribe as usize;

        // Memory cache flush
        JUMP_TABLE[626] = pebble_api::pbl_memory_cache_flush as usize;

        // text_layer_set_size (470)
        JUMP_TABLE[470] = pebble_api::pbl_text_layer_set_size as usize;
        // text_layer_enable_screen_text_flow_and_paging / restore (no-ops)
        JUMP_TABLE[596] = pebble_api::pbl_unobstructed_area_service_unsubscribe as usize;
        JUMP_TABLE[597] = pebble_api::pbl_unobstructed_area_service_unsubscribe as usize;

        // rot_bitmap_layer_set_corner_clip_color modern (374)
        JUMP_TABLE[374] = pebble_api::pbl_rot_bitmap_layer_set_corner_clip_color as usize;

        // dict remaining
        JUMP_TABLE[310] = pebble_api::pbl_dict_serialize_tuplets_to_buffer as usize;
        JUMP_TABLE[314] = pebble_api::pbl_dict_calc_buffer_size as usize; // dict_size stub

        // Profiler (no-ops)
        JUMP_TABLE[365] = pebble_api::pbl_memory_cache_flush as usize;
        JUMP_TABLE[366] = pebble_api::pbl_memory_cache_flush as usize;
        JUMP_TABLE[367] = pebble_api::pbl_memory_cache_flush as usize;
        JUMP_TABLE[368] = pebble_api::pbl_memory_cache_flush as usize;

        // app_glance (no-ops)
        JUMP_TABLE[614] = pebble_api::pbl_memory_cache_flush as usize;
        JUMP_TABLE[615] = pebble_api::pbl_memory_cache_flush as usize;

        // rocky (no-op)
        JUMP_TABLE[627] = pebble_api::pbl_memory_cache_flush as usize;

        // GDraw command API
        JUMP_TABLE[474] = pebble_api::pbl_gdraw_command_draw as usize;
        JUMP_TABLE[475] = pebble_api::pbl_gdraw_command_frame_draw as usize;
        JUMP_TABLE[476] = pebble_api::pbl_gdraw_command_frame_get_duration as usize;
        JUMP_TABLE[477] = pebble_api::pbl_gdraw_command_frame_set_duration as usize;
        JUMP_TABLE[478] = pebble_api::pbl_gdraw_command_get_fill_color as usize;
        JUMP_TABLE[479] = pebble_api::pbl_gdraw_command_get_hidden as usize;
        JUMP_TABLE[480] = pebble_api::pbl_gdraw_command_get_num_points as usize;
        JUMP_TABLE[481] = pebble_api::pbl_gdraw_command_get_path_open as usize;
        JUMP_TABLE[482] = pebble_api::pbl_gdraw_command_get_point as usize;
        JUMP_TABLE[483] = pebble_api::pbl_gdraw_command_get_radius as usize;
        JUMP_TABLE[484] = pebble_api::pbl_gdraw_command_get_stroke_color as usize;
        JUMP_TABLE[485] = pebble_api::pbl_gdraw_command_get_stroke_width as usize;
        JUMP_TABLE[486] = pebble_api::pbl_gdraw_command_get_type as usize;
        JUMP_TABLE[487] = pebble_api::pbl_gdraw_command_image_clone as usize;
        JUMP_TABLE[488] = pebble_api::pbl_gdraw_command_image_create_with_resource as usize;
        JUMP_TABLE[489] = pebble_api::pbl_gdraw_command_image_destroy as usize;
        JUMP_TABLE[490] = pebble_api::pbl_gdraw_command_image_draw as usize;
        JUMP_TABLE[491] = pebble_api::pbl_gdraw_command_image_get_bounds_size as usize;
        JUMP_TABLE[492] = pebble_api::pbl_gdraw_command_image_get_command_list as usize;
        JUMP_TABLE[493] = pebble_api::pbl_gdraw_command_image_set_bounds_size as usize;
        JUMP_TABLE[494] = pebble_api::pbl_gdraw_command_list_draw as usize;
        JUMP_TABLE[495] = pebble_api::pbl_gdraw_command_list_get_command as usize;
        JUMP_TABLE[496] = pebble_api::pbl_gdraw_command_list_get_num_commands as usize;
        JUMP_TABLE[497] = pebble_api::pbl_gdraw_command_list_iterate as usize;
        JUMP_TABLE[498] = pebble_api::pbl_gdraw_command_sequence_clone as usize;
        JUMP_TABLE[499] = pebble_api::pbl_gdraw_command_sequence_create_with_resource as usize;
        JUMP_TABLE[500] = pebble_api::pbl_gdraw_command_sequence_destroy as usize;
        JUMP_TABLE[501] = pebble_api::pbl_gdraw_command_sequence_get_bounds_size as usize;
        JUMP_TABLE[502] = pebble_api::pbl_gdraw_command_sequence_get_frame_by_elapsed as usize;
        JUMP_TABLE[503] = pebble_api::pbl_gdraw_command_sequence_get_frame_by_index as usize;
        JUMP_TABLE[504] = pebble_api::pbl_gdraw_command_sequence_get_num_frames as usize;
        JUMP_TABLE[505] = pebble_api::pbl_gdraw_command_sequence_get_play_count as usize;
        JUMP_TABLE[506] = pebble_api::pbl_gdraw_command_sequence_get_total_duration as usize;
        JUMP_TABLE[507] = pebble_api::pbl_gdraw_command_sequence_set_bounds_size as usize;
        JUMP_TABLE[508] = pebble_api::pbl_gdraw_command_sequence_set_play_count as usize;
        JUMP_TABLE[509] = pebble_api::pbl_gdraw_command_set_fill_color as usize;
        JUMP_TABLE[510] = pebble_api::pbl_gdraw_command_set_hidden as usize;
        JUMP_TABLE[511] = pebble_api::pbl_gdraw_command_set_path_open as usize;
        JUMP_TABLE[512] = pebble_api::pbl_gdraw_command_set_point as usize;
        JUMP_TABLE[513] = pebble_api::pbl_gdraw_command_set_radius as usize;
        JUMP_TABLE[514] = pebble_api::pbl_gdraw_command_set_stroke_color as usize;
        JUMP_TABLE[515] = pebble_api::pbl_gdraw_command_set_stroke_width as usize;
        JUMP_TABLE[612] = pebble_api::pbl_gdraw_command_frame_get_command_list as usize;

        // GBitmap from PNG / palettized
        JUMP_TABLE[421] = pebble_api::pbl_gbitmap_create_from_png_data as usize;
        JUMP_TABLE[458] = pebble_api::pbl_gbitmap_create_palettized_from_1bit as usize;
    }

    println!("  Jump table built ({} entries, at 0x{:08x})",
        JUMP_TABLE_SIZE,
        unsafe { JUMP_TABLE.as_ptr() as usize });
}

/// SIGSEGV handler — print crash info before dying
extern "C" fn sigsegv_handler(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    unsafe {
        let addr = if !info.is_null() { (*info).si_addr() as usize } else { 0 };
        eprintln!("\n[pebble:CRASH] Signal {} at address 0x{:08x}", sig, addr);
        eprintln!("[pebble:CRASH] Unimplemented calls before crash: {}", UNIMPL_CALL_COUNT);

        // Re-raise to get default behavior (core dump / exit)
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Execute a loaded Pebble binary. Runs in the calling thread.
/// The `stop` flag can be set from another thread to signal app_event_loop to exit.
pub fn execute(binary: &LoadedBinary, stop: Arc<AtomicBool>) -> Result<(), String> {
    // Install SIGSEGV handler for crash diagnostics
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigsegv_handler as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGBUS, &sa, std::ptr::null_mut());
    }

    let _session = crate::runtime::SessionCleanup;
    // Store the stop flag for app_event_loop to check
    if stop.load(Ordering::Relaxed) { return Ok(()); }
    pebble_api::set_stop_flag(stop);

    log_unimplemented_entries();
    println!("  Calling Pebble app entry point...");

    // Cast to function pointer and call. Pebble main() takes no args and returns int.
    // Set Thumb bit (bit 0) for ARM Thumb interworking.
    let entry_addr = (binary.base as usize + binary.entry) | 1;
    let main_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry_addr) };

    let result = main_fn();
    println!("  Pebble app main() returned: {}", result);

    Ok(())
}
