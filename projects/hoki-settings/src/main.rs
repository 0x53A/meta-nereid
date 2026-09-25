#[allow(dead_code)]
#[path = "../../shared/acoustic_volume.rs"]
mod acoustic_volume;
mod action_worker;
mod acoustic;
mod health_recording;
mod storage;
mod simulated;
mod poll_gate;
mod controls;
mod swipe;
#[cfg(test)]
mod ui_tests;
use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

slint::include_modules!();

/// Crown scroll ticks needed to move one item (reduces sensitivity ~5x).
const SCROLL_TICKS_PER_ITEM: i32 = 5;

fn main() {
    let window = MainWindow::new().unwrap();
    window.set_acoustic_volume(read_volume());

    let scroll_accum = Arc::new(AtomicI32::new(0));

    let gate = Arc::new(poll_gate::PollGate::new(
        std::env::var_os("HOKI_MANAGED_ROLE").is_none(),
    ));
    let poll_version = Arc::new(controls::PollVersion::default());
    start_sysinfo_poller(window.as_weak(), gate.clone(), poll_version.clone());

    install_swipe_callbacks(&window);
    start_stdin_reader(window.as_weak(), scroll_accum.clone(), gate);

    let weak = window.as_weak();
    let completion_version = poll_version.clone();
    let worker = action_worker::ActionWorker::new(run_and_refresh, move |(result, state)| {
        let weak = weak.clone();
        let version = completion_version.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak.upgrade() {
                apply_controls(&win, &state);
                // Reject snapshots whose read started during the action, even
                // if they arrive after this completion has cleared busy.
                version.advance();
                win.set_action_busy(false);
                win.set_action_status(
                    match result {
                        Ok(message) => message,
                        Err(error) => format!("Action failed: {error}"),
                    }.into(),
                );
            }
        });
    }).expect("settings action worker");
    install_action_callback(&window, worker, poll_version);

    {
        let accum = scroll_accum.clone();
        window.on_top_pressed(move || {
            accum.store(0, Ordering::Relaxed);
            println!("go-watchface");
        });
    }

    {
        let weak = window.as_weak();
        window.on_bottom_pressed(move || {
            let window = weak.unwrap();
            let idx = window.get_settings_selected_index();
            if window.get_show_battery_menu() {
                window.set_show_battery_menu(false);
            } else if window.get_show_power_menu() {
                window.set_show_power_menu(false);
            } else if window.get_show_usb_menu() {
                window.set_show_usb_menu(false);
            } else {
                match idx {
                    0 => window.set_show_battery_menu(true),
                    4 => window.invoke_settings_action("toggle-wifi".into()),
                    5 => window.invoke_settings_action("toggle-bt".into()),
                    6 => window.invoke_settings_action("toggle-airplane".into()),
                    7 => window.invoke_settings_action("screen-off".into()),
                    8 => window.set_show_power_menu(true),
                    9 => window.set_show_usb_menu(true),
                    10 if window.get_acoustic_available() => window.invoke_settings_action("toggle-acoustic".into()),
                    index if index == health_recording_index(&window) => window.invoke_settings_action("toggle-recording".into()),
                    _ => {}
                }
            }
        });
    }

    // Explicit simulator-only capture, using the actual Slint renderer.
    let capture_timer = slint::Timer::default();
    if simulated::enabled() {
        if let Ok(index) = std::env::var("HOKI_SETTINGS_SELECTED") {
            window.set_settings_selected_index(index.parse().unwrap_or(0));
        }
        if let Some(path) = std::env::var_os("HOKI_SETTINGS_CAPTURE") {
            let weak = window.as_weak();
            capture_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(2), move || {
                use std::io::Write;
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let window = weak.upgrade().ok_or("window closed")?;
                    let pixels = window.window().take_snapshot()?;
                    let mut file = std::fs::File::create(&path)?;
                    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                    for pixel in pixels.as_bytes().chunks_exact(4) { file.write_all(&pixel[..3])?; }
                    Ok(())
                })();
                if let Err(error) = result { eprintln!("Capture failed: {error}"); }
                let _ = slint::quit_event_loop();
            });
        }
    }
    window.run().unwrap();
}

fn install_swipe_callbacks(window: &MainWindow) {
    let swipe = std::rc::Rc::new(std::cell::RefCell::new(swipe::Swipe::default()));
    let gesture = swipe.clone();
    window.on_swipe_begin(move |y| gesture.borrow_mut().begin(y));
    window.on_swipe_move(move |y, selected, count| swipe.borrow_mut().move_to(y, selected, count));
}

fn install_action_callback(window: &MainWindow, worker: action_worker::ActionWorker, poll_version: Arc<controls::PollVersion>) {
    let weak = window.as_weak();
    window.on_settings_action(move |action| {
        let Some(win) = weak.upgrade() else { return; };
        if win.get_action_busy() { return; }
        let previous = window_controls(&win);
        let action = match controls::prepare(&action, &previous) {
            Ok(action) => action,
            Err(error) => { win.set_action_status(error.into()); return; }
        };
        poll_version.advance();
        win.set_action_busy(true);
        win.set_action_status("".into());
        if let Some(label) = controls::transition_label(&action) {
            match action.split_once(':').unwrap().0 {
                "set-wifi" => win.set_wifi_status(label.into()),
                "set-bt" => win.set_bt_status(label.into()),
                "set-airplane" => win.set_airplane_status(label.into()),
                "set-acoustic" => win.set_acoustic_status(label.into()),
                "set-recording" => win.set_recording_status(label.into()),
                _ => unreachable!(),
            }
        } else if action.starts_with("set-usb-") {
            let target = match action.as_str() {
                "set-usb-developer" => "SSH",
                "set-usb-adb" => "ADB",
                _ => "Charge",
            };
            win.set_usb_mode(format!("to {target}…").into());
            win.set_action_status(format!("Switching USB to {target}…").into());
        } else if !action.starts_with("acoustic-volume:") {
            win.set_action_status("Working…".into());
        }
        if let Err(error) = worker.submit(action) {
            apply_controls(&win, &previous);
            poll_version.advance();
            win.set_action_busy(false);
            win.set_action_status(error.into());
        }
    });
}

// --- Stdin reader (compositor commands) ---

fn start_stdin_reader(
    window: slint::Weak<MainWindow>,
    scroll_accum: Arc<AtomicI32>,
    gate: Arc<poll_gate::PollGate>,
) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let reader = BufReader::new(stdin.lock());
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            match line.as_str() {
                "visibility:visible" => {
                    gate.set_visible(true);
                    continue;
                }
                "visibility:hidden" => {
                    gate.set_visible(false);
                    continue;
                }
                _ => {}
            }
            let w = window.clone();
            let accum = scroll_accum.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    handle_compositor_message(&win, &line, &accum);
                }
            })
            .ok();
        }
    });
}

fn handle_compositor_message(window: &MainWindow, msg: &str, scroll_accum: &AtomicI32) {
    match msg {
        _ if msg.starts_with("scroll:") => {
            if let Ok(delta) = msg[7..].parse::<i32>() {
                let acc = scroll_accum.load(Ordering::Relaxed) + delta;
                let items_to_move = acc / SCROLL_TICKS_PER_ITEM;
                let count = settings_item_count(window);
                if items_to_move != 0 {
                    scroll_accum.store(acc % SCROLL_TICKS_PER_ITEM, Ordering::Relaxed);
                    let mut idx = window.get_settings_selected_index() + items_to_move;
                    idx = idx.clamp(0, count - 1);
                    window.set_settings_selected_index(idx);
                } else {
                    scroll_accum.store(acc, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

// --- System info poller ---

fn start_sysinfo_poller(window: slint::Weak<MainWindow>, gate: Arc<poll_gate::PollGate>, poll_version: Arc<controls::PollVersion>) {
    std::thread::spawn(move || {
        let mut previous = None;
        loop {
            let generation = gate.wait_for_poll(previous, std::time::Duration::from_secs(5));
            let version = poll_version.current();
            let battery_level =
                read_sysfs_int("/sys/class/power_supply/battery/capacity").unwrap_or(-1);
            let charging = read_sysfs_string("/sys/class/power_supply/battery/status")
                .map(|s| s.trim() == "Charging")
                .unwrap_or(false);
            let cpu_cores = count_active_cpu_cores();
            let (disk_used, disk_free) = storage::read();
            let controls = read_controls();

            // BMS detailed metrics
            let current_ua = read_sysfs_int("/sys/class/power_supply/bms/current_now");
            let voltage_uv = read_sysfs_int("/sys/class/power_supply/bms/voltage_now");
            let temp_deci_c = read_sysfs_int("/sys/class/power_supply/bms/temp");
            let charge_now_uah = read_sysfs_int("/sys/class/power_supply/bms/charge_counter");
            let charge_full_uah = read_sysfs_int("/sys/class/power_supply/bms/charge_full");
            let time_to_empty = read_sysfs_int("/sys/class/power_supply/bms/time_to_empty_avg");
            let time_to_full = read_sysfs_int("/sys/class/power_supply/bms/time_to_full_avg");
            let voltage_ocv = read_sysfs_int("/sys/class/power_supply/bms/voltage_ocv");
            let cycle_count = read_sysfs_int("/sys/class/power_supply/bms/cycle_count");
            let resistance = read_sysfs_int("/sys/class/power_supply/bms/resistance");

            let details = BatteryDetails {
                power: match (current_ua, voltage_uv) {
                    (Some(i), Some(v)) => {
                        let mw = (i.abs() as i64 * v.abs() as i64) / 1_000_000_000;
                        format!("{mw} mW").into()
                    }
                    _ => "—".into(),
                },
                current: match current_ua {
                    Some(i) => format!("{} mA", i.abs() / 1000).into(),
                    None => "—".into(),
                },
                voltage: match voltage_uv {
                    Some(v) => format!("{:.3}V", v as f64 / 1e6).into(),
                    None => "—".into(),
                },
                ocv: match voltage_ocv {
                    Some(v) => format!("{:.3}V", v as f64 / 1e6).into(),
                    None => "—".into(),
                },
                charge: match (charge_now_uah, charge_full_uah) {
                    (Some(now), Some(full)) => {
                        format!("{} / {} mAh", now / 1000, full / 1000).into()
                    }
                    _ => "—".into(),
                },
                time: if charging {
                    match time_to_full {
                        Some(secs) if secs > 0 => {
                            let h = secs / 3600;
                            let m = (secs % 3600) / 60;
                            if h > 0 {
                                format!("~{h}h {m}m to full")
                            } else {
                                format!("~{m}m to full")
                            }
                            .into()
                        }
                        _ => "charging".into(),
                    }
                } else {
                    match time_to_empty {
                        Some(secs) if secs > 0 => {
                            let h = secs / 3600;
                            let m = (secs % 3600) / 60;
                            if h > 0 {
                                format!("~{h}h {m}m")
                            } else {
                                format!("~{m}m")
                            }
                            .into()
                        }
                        _ => "—".into(),
                    }
                },
                temp: match temp_deci_c {
                    Some(t) => format!("{:.1} C", t as f64 / 10.0).into(),
                    None => "—".into(),
                },
                cycles: match cycle_count {
                    Some(c) => format!("{c}").into(),
                    None => "—".into(),
                },
                resistance: match resistance {
                    Some(r) => format!("{} mOhm", r / 1000).into(),
                    None => "—".into(),
                },
            };

            let w = window.clone();
            let poll_version = poll_version.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_battery_level(battery_level);
                    win.set_battery_charging(charging);
                    win.set_cpu_cores_active(cpu_cores);
                    win.set_disk_used(disk_used.into());
                    win.set_disk_free(disk_free.into());
                    if poll_version.accepts(version, win.get_action_busy()) {
                        // Do not replace the local volume while the user drags.
                        apply_control_status(&win, &controls);
                    }
                    win.set_battery_details(details);
                }
            })
            .ok();

            previous = Some((generation, std::time::Instant::now()));
        }
    });
}

fn read_volume() -> i32 {
    simulated::volume().unwrap_or_else(acoustic_volume::read)
}

fn read_controls() -> controls::Snapshot {
    let (wifi, bt, airplane) = get_radio_status();
    controls::Snapshot {
        wifi, bt, airplane, usb: get_usb_mode(),
        acoustic: acoustic::status().unwrap_or_default(),
        recording: health_recording::status().unwrap_or_default(),
        volume: read_volume(),
    }
}

fn window_controls(win: &MainWindow) -> controls::Snapshot {
    controls::Snapshot {
        wifi: win.get_wifi_status().to_string(),
        bt: win.get_bt_status().to_string(),
        airplane: win.get_airplane_status().to_string(),
        usb: win.get_usb_mode().to_string(),
        acoustic: acoustic::State {
            available: win.get_acoustic_available(),
            on: win.get_acoustic_on(),
            failed: win.get_acoustic_status() == "error",
            transition: match win.get_acoustic_status().as_str() {
                "turning on" => Some(true), "turning off" => Some(false), _ => None,
            },
        },
        volume: win.get_acoustic_volume(),
        recording: health_recording::State {
            available: win.get_recording_available(),
            on: win.get_recording_on(),
            failed: win.get_recording_status() == "error",
            transition: match win.get_recording_status().as_str() {
                "starting" => Some(true), "stopping" => Some(false), _ => None,
            },
        },
    }
}

fn apply_control_status(win: &MainWindow, state: &controls::Snapshot) {
    win.set_wifi_status(state.wifi.clone().into());
    win.set_bt_status(state.bt.clone().into());
    win.set_airplane_status(state.airplane.clone().into());
    win.set_usb_mode(state.usb.clone().into());
    apply_acoustic_state(win, &state.acoustic);
    apply_recording_state(win, &state.recording);
}

fn apply_controls(win: &MainWindow, state: &controls::Snapshot) {
    apply_control_status(win, state);
    win.set_acoustic_volume(state.volume);
}

fn run_and_refresh(action: &str) -> (Result<String, String>, controls::Snapshot) {
    let result = handle_settings_action(action);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    controls::settle(action, result, read_controls, || {
        if std::time::Instant::now() >= deadline { return false; }
        std::thread::sleep(std::time::Duration::from_millis(250));
        true
    })
}

fn apply_acoustic_state(window: &MainWindow, state: &acoustic::State) {
    window.set_acoustic_available(state.available);
    window.set_acoustic_on(state.on);
    window.set_acoustic_status(state.label().into());
}

fn apply_recording_state(window: &MainWindow, state: &health_recording::State) {
    window.set_recording_available(state.available);
    window.set_recording_on(state.on);
    window.set_recording_status(state.label().into());
    let max_index = settings_item_count(window) - 1;
    if window.get_settings_selected_index() > max_index {
        window.set_settings_selected_index(max_index);
    }
}

fn read_sysfs_int(path: &str) -> Option<i32> {
    read_sysfs_string(path)?.trim().parse().ok()
}

fn read_sysfs_string(path: &str) -> Option<String> {
    std::fs::read_to_string(simulated::sysfs_path(path)).ok()
}

fn count_active_cpu_cores() -> i32 {
    let online = match read_sysfs_string("/sys/devices/system/cpu/online") {
        Some(s) => s.trim().to_string(),
        None => return 0,
    };

    let mut count = 0;
    for part in online.split(',') {
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.parse::<i32>(), end.parse::<i32>()) {
                count += e - s + 1;
            }
        } else if part.parse::<i32>().is_ok() {
            count += 1;
        }
    }
    count
}

fn get_radio_status() -> (String, String, String) {
    if let Some(status) = simulated::radio_status() { return status; }
    let conn = match zbus::blocking::connection::Builder::system().and_then(|b| b.method_timeout(std::time::Duration::from_secs(2)).build()) {
        Ok(c) => c,
        Err(_) => return ("?".into(), "?".into(), "?".into()),
    };

    get_radio_status_on(&conn)
}

fn get_radio_status_on(conn: &zbus::blocking::Connection) -> (String, String, String) {
    let proxy = match zbus::blocking::Proxy::new(
        conn,
        "org.hoki.radio",
        "/org/hoki/radio",
        "org.hoki.radio.Manager",
    ) {
        Ok(p) => p,
        Err(_) => return ("?".into(), "?".into(), "?".into()),
    };

    let reply = match proxy.call_method("Status", &()) {
        Ok(r) => r,
        Err(_) => return ("?".into(), "?".into(), "?".into()),
    };

    match reply.body().deserialize::<(String, bool)>() {
        Ok((status, _wifi_on_boot)) => {
            let s = status.to_lowercase();
            let wifi = if s.contains("wifi") { "on" } else { "off" };
            let bt = if s.contains("bt") || s.contains("bluetooth") {
                "on"
            } else {
                "off"
            };
            let airplane = match connman_offline_mode(conn) {
                Ok(true) => "on",
                Ok(false) => "off",
                Err(_) => "?",
            };
            (wifi.into(), bt.into(), airplane.into())
        }
        Err(_) => ("?".into(), "?".into(), "?".into()),
    }
}

// OfflineMode is separate from the individual technologies' Powered values.
fn connman_offline_mode(conn: &zbus::blocking::Connection) -> zbus::Result<bool> {
    let manager = zbus::blocking::Proxy::new(conn, "net.connman", "/", "net.connman.Manager")?;
    let props: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
        manager.call("GetProperties", &())?;
    props.get("OfflineMode").and_then(|v| bool::try_from(v).ok())
        .ok_or_else(|| zbus::Error::Failure("ConnMan OfflineMode is unavailable".into()))
}

fn format_usb_mode(mode: &str) -> String {
    match mode.trim() {
        "developer_mode" => "SSH".into(),
        "adb_mode" => "ADB".into(),
        "charging_only" => "Charge".into(),
        _ => "off".into(),
    }
}

fn get_usb_mode() -> String {
    if simulated::enabled() { return format_usb_mode(&simulated::read("usb", "charging_only")); }
    let conn = match zbus::blocking::connection::Builder::system().and_then(|b| b.method_timeout(std::time::Duration::from_secs(2)).build()) {
        Ok(c) => c,
        Err(_) => return "?".into(),
    };
    let proxy = match zbus::blocking::Proxy::new(
        &conn,
        "com.meego.usb_moded",
        "/com/meego/usb_moded",
        "com.meego.usb_moded",
    ) {
        Ok(p) => p,
        Err(_) => return "?".into(),
    };
    match proxy.call_method("mode_request", &()) {
        Ok(reply) => {
            let mode: String = reply.body().deserialize().unwrap_or_default();
            format_usb_mode(&mode)
        }
        Err(_) => "?".into(),
    }
}

// --- Settings actions ---

fn handle_settings_action(action: &str) -> Result<String, String> {
    if simulated::enabled() && action != "screen-off" { return simulated::action(action); }
    if let Some(value) = action.strip_prefix("acoustic-volume:") {
        let percent = value.parse::<i32>().map_err(|e| e.to_string())?;
        acoustic_volume::write(percent).map_err(|e| e.to_string())?;
        return Ok(String::new());
    }

    match action {
        "set-acoustic:on" => acoustic::set_enabled(true),
        "set-acoustic:off" => acoustic::set_enabled(false),
        "set-recording:on" => health_recording::set_enabled(true),
        "set-recording:off" => health_recording::set_enabled(false),
        "screen-off" => {
            println!("screen-off");
            Ok(String::new())
        }
        "poweroff" | "reboot" | "bootloader" => {
            let status = if action == "bootloader" {
                Command::new("reboot").arg("bootloader").status()
            } else {
                Command::new("systemctl")
                    .args(["--no-block", action])
                    .status()
            }
            .map_err(|e| e.to_string())?;
            if status.success() {
                Ok(String::new())
            } else {
                Err(format!("{action}: {status}"))
            }
        }
        _ => radio_action(action),
    }
}

fn settings_item_count(window: &MainWindow) -> i32 {
    11 + bool_index(window.get_acoustic_available())
        + bool_index(window.get_acoustic_available() && window.get_acoustic_on())
}

fn health_recording_index(window: &MainWindow) -> i32 {
    10 + bool_index(window.get_acoustic_available())
        + bool_index(window.get_acoustic_available() && window.get_acoustic_on())
}

fn bool_index(value: bool) -> i32 {
    if value { 1 } else { 0 }
}

fn radio_action(action: &str) -> Result<String, String> {
    let conn = zbus::blocking::connection::Builder::system()
        .and_then(|b| b.method_timeout(std::time::Duration::from_secs(30)).build())
        .map_err(|e| e.to_string())?;
    radio_action_on(&conn, action)
}

fn radio_action_on(conn: &zbus::blocking::Connection, action: &str) -> Result<String, String> {
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.hoki.radio",
        "/org/hoki/radio",
        "org.hoki.radio.Manager",
    )
    .map_err(|e| e.to_string())?;
    let reply = match action {
        "set-wifi:on" | "set-wifi:off" | "set-bt:on" | "set-bt:off" => {
            let (kind, target) = action.split_once(':').unwrap();
            proxy.call_method(if kind == "set-wifi" { "SetWifiEnabled" } else { "SetBluetoothEnabled" }, &(target == "on",))
        }
        "set-airplane:on" => proxy.call_method("DisableRadio", &()),
        "set-airplane:off" => proxy.call_method("EnableRadio", &()),
        "set-usb-developer" => proxy.call_method("SetUsbMode", &"developer_mode"),
        "set-usb-adb" => proxy.call_method("SetUsbMode", &"adb_mode"),
        "set-usb-charging" => proxy.call_method("SetUsbMode", &"charging_only"),
        _ => return Err("Unknown settings action".into()),
    }
    .map_err(|e| e.to_string())?;
    let response: String = reply.body().deserialize().map_err(|e| e.to_string())?;
    action_response(&response)
}

fn action_response(response: &str) -> Result<String, String> {
    match response.trim() {
        "ok" => Ok(String::new()),
        "reboot_required" => Ok("Restart the watch to finish switching radios.".into()),
        "" => Err("No response from radio service".into()),
        other => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn radio_failures_and_restart_requirement_are_visible() {
        assert!(action_response("ok").unwrap().is_empty());
        assert!(!action_response("reboot_required").unwrap().is_empty());
        assert_eq!(
            action_response("toggle failed: unavailable"),
            Err("toggle failed: unavailable".into())
        );
        assert!(action_response("").is_err());
    }
    struct MockConnMan(std::sync::Arc<std::sync::Mutex<bool>>);
    #[zbus::interface(name = "net.connman.Manager")]
    impl MockConnMan {
        fn get_properties(&self) -> std::collections::HashMap<String, zbus::zvariant::OwnedValue> {
            std::collections::HashMap::from([("OfflineMode".into(), (*self.0.lock().unwrap()).into())])
        }
    }
    struct MockRadio {
        offline: std::sync::Arc<std::sync::Mutex<bool>>,
        wifi: std::sync::Mutex<bool>,
        bt: std::sync::Mutex<bool>,
    }
    #[zbus::interface(name = "org.hoki.radio.Manager")]
    impl MockRadio {
        fn status(&self) -> (String, bool) {
            let wifi = *self.wifi.lock().unwrap();
            let bt = *self.bt.lock().unwrap();
            (match (wifi, bt) { (true, true) => "wifi+bt", (true, false) => "wifi", (false, true) => "bt", _ => "off" }.into(), false)
        }
        fn set_wifi_enabled(&self, enabled: bool) -> String { *self.wifi.lock().unwrap() = enabled; "ok".into() }
        fn set_bluetooth_enabled(&self, enabled: bool) -> String { *self.bt.lock().unwrap() = enabled; "ok".into() }
        fn disable_radio(&self) -> String { *self.offline.lock().unwrap() = true; "ok".into() }
        fn enable_radio(&self) -> String { *self.offline.lock().unwrap() = false; "ok".into() }
    }
    #[test]
    #[ignore = "run under dbus-run-session to isolate mock ConnMan and radiod"]
    fn airplane_mode_uses_offline_property_when_both_radios_are_off() {
        let offline = std::sync::Arc::new(std::sync::Mutex::new(false));
        let _server = zbus::blocking::connection::Builder::session().unwrap()
            .name("net.connman").unwrap().name("org.hoki.radio").unwrap()
            .serve_at("/", MockConnMan(offline.clone())).unwrap()
            .serve_at("/org/hoki/radio", MockRadio { offline: offline.clone(), wifi: false.into(), bt: false.into() }).unwrap()
            .build().unwrap();
        let client = zbus::blocking::Connection::session().unwrap();
        assert_eq!(get_radio_status_on(&client), ("off".into(), "off".into(), "off".into()));
        radio_action_on(&client, "set-airplane:on").unwrap();
        assert!(*offline.lock().unwrap());
        assert_eq!(get_radio_status_on(&client).2, "on");
        radio_action_on(&client, "set-airplane:off").unwrap();
        assert!(!*offline.lock().unwrap());
        assert_eq!(get_radio_status_on(&client).2, "off");
        for action in ["set-wifi:on", "set-bt:on"] {
            radio_action_on(&client, action).unwrap();
            radio_action_on(&client, action).unwrap();
        }
        assert_eq!(get_radio_status_on(&client), ("on".into(), "on".into(), "off".into()));
        for action in ["set-wifi:off", "set-bt:off"] {
            radio_action_on(&client, action).unwrap();
            radio_action_on(&client, action).unwrap();
        }
        assert_eq!(get_radio_status_on(&client), ("off".into(), "off".into(), "off".into()));
    }

}
