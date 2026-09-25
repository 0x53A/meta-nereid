slint::include_modules!();

use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static SCAN_RUNNING: Mutex<bool> = Mutex::new(false);

const SCROLL_TICKS_PER_ITEM: i32 = 5;

#[derive(Clone)]
struct DeviceInfo {
    name: String,
    address: String,
    paired: bool,
    connected: bool,
    rssi: i32,
}

fn parse_rssi(stdout: &str) -> i32 {
    if let Some(pos) = stdout.find("RSSI:") {
        stdout[pos..]
            .split_whitespace()
            .nth(1)
            .unwrap_or("-100")
            .parse()
            .unwrap_or(-100)
    } else {
        -100
    }
}

/// Get all known devices via `bluetoothctl devices`, then check each one's
/// info to determine paired/connected/rssi status.  This avoids relying on
/// `bluetoothctl paired-devices` which can return empty if the adapter isn't
/// ready yet.
fn get_all_devices() -> Vec<DeviceInfo> {
    let output = match Command::new("bluetoothctl").args(["devices"]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let mut devices = Vec::new();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[0] == "Device" {
            let address = parts[1].to_string();
            let name = parts[2..].join(" ");

            // Skip unnamed / placeholder entries
            if name.is_empty() || name == "unknown" || name == "NULL" {
                continue;
            }
            // Skip entries whose "name" is just the MAC address (no friendly name)
            if name == address {
                continue;
            }

            let (paired, connected, rssi) = Command::new("bluetoothctl")
                .args(["info", &address])
                .output()
                .map(|o| {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let paired = stdout.contains("Paired: yes");
                    let connected = stdout.contains("Connected: yes");
                    let rssi = parse_rssi(&stdout);
                    (paired, connected, rssi)
                })
                .unwrap_or((false, false, -100));

            devices.push(DeviceInfo {
                name,
                address,
                paired,
                connected,
                rssi,
            });
        }
    }

    devices.sort_by(|a, b| b.rssi.cmp(&a.rssi));
    devices
}

fn pair_device(address: &str) -> Result<(), String> {
    let output = Command::new("bluetoothctl")
        .args(["pair", address])
        .output()
        .map_err(|e| format!("Failed: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Pair failed: {}", err));
    }

    // Trust — required for PulseAudio to create the A2DP sink
    let _ = Command::new("bluetoothctl")
        .args(["trust", address])
        .output();

    // Disconnect + reconnect so PA registers the audio sink
    let _ = Command::new("bluetoothctl")
        .args(["disconnect", address])
        .output();

    thread::sleep(Duration::from_secs(2));

    let output = Command::new("bluetoothctl")
        .args(["connect", address])
        .output()
        .map_err(|e| format!("Failed: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Connect failed: {}", err));
    }

    Ok(())
}

fn connect_device(address: &str) -> Result<(), String> {
    let output = Command::new("bluetoothctl")
        .args(["connect", address])
        .output()
        .map_err(|e| format!("Failed: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn disconnect_device(address: &str) -> Result<(), String> {
    let output = Command::new("bluetoothctl")
        .args(["disconnect", address])
        .output()
        .map_err(|e| format!("Failed: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn remove_device(address: &str) -> Result<(), String> {
    let output = Command::new("bluetoothctl")
        .args(["remove", address])
        .output()
        .map_err(|e| format!("Failed: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn start_discovery() {
    let mut running = SCAN_RUNNING.lock().unwrap();
    if *running {
        return;
    }
    *running = true;
    drop(running);

    thread::spawn(|| {
        let _ = Command::new("bluetoothctl")
            .args(["scan", "on"])
            .output();
    });
}

fn stop_discovery() {
    let mut running = SCAN_RUNNING.lock().unwrap();
    *running = false;
    drop(running);

    let _ = Command::new("bluetoothctl")
        .args(["scan", "off"])
        .output();
}

fn update_paired_devices(app: &App) {
    let all = get_all_devices();
    let devices: Vec<_> = all.into_iter().filter(|d| d.paired).collect();

    app.set_device_count(devices.len() as i32);

    let slots: [(
        fn(&App, slint::SharedString),
        fn(&App, slint::SharedString),
        fn(&App, bool),
    ); 6] = [
        (App::set_device_1_name, App::set_device_1_address, App::set_device_1_connected),
        (App::set_device_2_name, App::set_device_2_address, App::set_device_2_connected),
        (App::set_device_3_name, App::set_device_3_address, App::set_device_3_connected),
        (App::set_device_4_name, App::set_device_4_address, App::set_device_4_connected),
        (App::set_device_5_name, App::set_device_5_address, App::set_device_5_connected),
        (App::set_device_6_name, App::set_device_6_address, App::set_device_6_connected),
    ];

    for (i, (set_name, set_addr, set_conn)) in slots.iter().enumerate() {
        if let Some(d) = devices.get(i) {
            set_name(app, d.name.clone().into());
            set_addr(app, d.address.clone().into());
            set_conn(app, d.connected);
        } else {
            set_name(app, "".into());
            set_addr(app, "".into());
            set_conn(app, false);
        }
    }
}

fn update_discovered_devices(app: &App) {
    let all = get_all_devices();
    let devices: Vec<_> = all.into_iter().filter(|d| !d.paired).collect();

    app.set_discovered_count(devices.len() as i32);

    let slots: [(
        fn(&App, slint::SharedString),
        fn(&App, slint::SharedString),
    ); 6] = [
        (App::set_discovered_1_name, App::set_discovered_1_address),
        (App::set_discovered_2_name, App::set_discovered_2_address),
        (App::set_discovered_3_name, App::set_discovered_3_address),
        (App::set_discovered_4_name, App::set_discovered_4_address),
        (App::set_discovered_5_name, App::set_discovered_5_address),
        (App::set_discovered_6_name, App::set_discovered_6_address),
    ];

    for (i, (set_name, set_addr)) in slots.iter().enumerate() {
        if let Some(d) = devices.get(i) {
            set_name(app, d.name.clone().into());
            set_addr(app, d.address.clone().into());
        } else {
            set_name(app, "".into());
            set_addr(app, "".into());
        }
    }
}

fn get_discovered_address(app: &App, index: i32) -> String {
    match index {
        0 => app.get_discovered_1_address().to_string(),
        1 => app.get_discovered_2_address().to_string(),
        2 => app.get_discovered_3_address().to_string(),
        3 => app.get_discovered_4_address().to_string(),
        4 => app.get_discovered_5_address().to_string(),
        5 => app.get_discovered_6_address().to_string(),
        _ => String::new(),
    }
}

// --- Stdin reader (compositor commands) ---

fn start_stdin_reader(window: slint::Weak<App>, scroll_accum: Arc<AtomicI32>) {
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let reader = BufReader::new(stdin.lock());
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let w = window.clone();
            let accum = scroll_accum.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = w.upgrade() {
                    handle_compositor_message(&app, &line, &accum);
                }
            })
            .ok();
        }
    });
}

fn handle_compositor_message(app: &App, msg: &str, scroll_accum: &AtomicI32) {
    match msg {
        "top" => {
            scroll_accum.store(0, Ordering::Relaxed);
            if app.get_show_device_detail() {
                app.set_show_device_detail(false);
            } else if app.get_is_scanning() {
                app.set_is_scanning(false);
                app.set_selected_index(0);
                stop_discovery();
                update_paired_devices(app);
            } else {
                println!("go-watchface");
            }
        }
        "bottom" => {
            if app.get_show_device_detail() {
                app.set_show_device_detail(false);
            } else if app.get_is_scanning() {
                let idx = app.get_selected_index();
                if idx < app.get_discovered_count() {
                    let addr = get_discovered_address(app, idx);
                    if !addr.is_empty() {
                        do_pair(app, &addr);
                    }
                }
            } else {
                let idx = app.get_selected_index();
                let dev_count = app.get_device_count();
                if idx < dev_count {
                    app.set_show_device_detail(true);
                } else if idx == dev_count {
                    do_start_scan(app);
                }
            }
        }
        _ if msg.starts_with("scroll:") => {
            if let Ok(delta) = msg[7..].parse::<i32>() {
                let acc = scroll_accum.load(Ordering::Relaxed) + delta;
                let items_to_move = acc / SCROLL_TICKS_PER_ITEM;
                let total = if app.get_is_scanning() {
                    app.get_discovered_count().max(1)
                } else {
                    app.get_device_count() + 1
                };
                if items_to_move != 0 {
                    scroll_accum.store(acc % SCROLL_TICKS_PER_ITEM, Ordering::Relaxed);
                    let mut idx = app.get_selected_index() + items_to_move;
                    idx = idx.clamp(0, total - 1);
                    app.set_selected_index(idx);
                } else {
                    scroll_accum.store(acc, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

// --- Background poller (keeps paired device list fresh) ---

fn start_paired_poller(window: slint::Weak<App>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(5));
        let w = window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = w.upgrade() {
                // Only refresh when showing the main list (not scanning or in detail)
                if !app.get_is_scanning() && !app.get_show_device_detail() {
                    update_paired_devices(&app);
                }
            }
        });
    });
}

// --- Actions ---

fn do_start_scan(app: &App) {
    app.set_is_scanning(true);
    app.set_selected_index(0);
    start_discovery();
    update_discovered_devices(app);

    let thread_weak = app.as_weak();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(3));
            if !*SCAN_RUNNING.lock().unwrap() {
                break;
            }
            let weak = thread_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    update_discovered_devices(&app);
                }
            });
        }
    });
}

fn do_pair(app: &App, address: &str) {
    let address = address.to_string();
    app.set_is_busy(true);
    app.set_status_text("Pairing...".into());

    let weak = app.as_weak();
    thread::spawn(move || {
        let result = pair_device(&address);
        if result.is_ok() {
            stop_discovery();
        }
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_is_busy(false);
                match result {
                    Ok(_) => {
                        app.set_status_text("Paired!".into());
                        app.set_is_scanning(false);
                        app.set_selected_index(0);
                        update_paired_devices(&app);
                    }
                    Err(e) => {
                        app.set_status_text(format!("Failed: {}", e).into());
                    }
                }
            }
        });
    });
}

fn do_connect(app: &App, address: &str) {
    let address = address.to_string();
    app.set_is_busy(true);
    app.set_status_text("Connecting...".into());

    let weak = app.as_weak();
    thread::spawn(move || {
        let result = connect_device(&address);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_is_busy(false);
                match result {
                    Ok(_) => {
                        app.set_status_text("Connected!".into());
                        update_paired_devices(&app);
                    }
                    Err(e) => {
                        app.set_status_text(format!("Failed: {}", e).into());
                    }
                }
            }
        });
    });
}

fn do_disconnect(app: &App, address: &str) {
    let address = address.to_string();
    app.set_is_busy(true);
    app.set_status_text("Disconnecting...".into());

    let weak = app.as_weak();
    thread::spawn(move || {
        let result = disconnect_device(&address);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_is_busy(false);
                match result {
                    Ok(_) => {
                        app.set_status_text("Disconnected".into());
                        update_paired_devices(&app);
                    }
                    Err(e) => {
                        app.set_status_text(format!("Failed: {}", e).into());
                    }
                }
            }
        });
    });
}

fn do_unpair(app: &App, address: &str) {
    let address = address.to_string();
    app.set_is_busy(true);
    app.set_status_text("Removing...".into());

    let weak = app.as_weak();
    thread::spawn(move || {
        let result = remove_device(&address);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_is_busy(false);
                match result {
                    Ok(_) => {
                        app.set_status_text("Removed".into());
                        app.set_selected_index(0);
                        update_paired_devices(&app);
                    }
                    Err(e) => {
                        app.set_status_text(format!("Failed: {}", e).into());
                    }
                }
            }
        });
    });
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    update_paired_devices(&app);

    let scroll_accum = Arc::new(AtomicI32::new(0));
    start_stdin_reader(app.as_weak(), scroll_accum);
    start_paired_poller(app.as_weak());

    // Slint callbacks — triggered from UI touch (detail view buttons) and from Rust stdin handler

    let weak = app.as_weak();
    app.on_start_scan(move || {
        if let Some(app) = weak.upgrade() {
            do_start_scan(&app);
        }
    });

    let weak = app.as_weak();
    app.on_stop_scan(move || {
        if let Some(app) = weak.upgrade() {
            app.set_is_scanning(false);
            app.set_selected_index(0);
            stop_discovery();
            update_paired_devices(&app);
        }
    });

    let weak = app.as_weak();
    app.on_pair_device(move |address: slint::SharedString| {
        if let Some(app) = weak.upgrade() {
            do_pair(&app, &address);
        }
    });

    let weak = app.as_weak();
    app.on_connect_device(move |address: slint::SharedString| {
        if let Some(app) = weak.upgrade() {
            do_connect(&app, &address);
        }
    });

    let weak = app.as_weak();
    app.on_disconnect_device(move |address: slint::SharedString| {
        if let Some(app) = weak.upgrade() {
            do_disconnect(&app, &address);
        }
    });

    let weak = app.as_weak();
    app.on_unpair_device(move |address: slint::SharedString| {
        if let Some(app) = weak.upgrade() {
            do_unpair(&app, &address);
        }
    });

    let weak = app.as_weak();
    app.on_refresh_devices(move || {
        if let Some(app) = weak.upgrade() {
            update_paired_devices(&app);
        }
    });

    app.run().unwrap();
}
