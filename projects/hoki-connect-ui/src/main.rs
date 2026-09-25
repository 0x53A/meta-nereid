mod crown;
mod volume;
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::{
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
slint::include_modules!();

fn state_dir() -> PathBuf {
    std::env::var_os("HOKI_CONNECT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/home/ceres".into()))
                .join(".config/hoki-connect")
        })
}
fn request(command: &str) -> Result<String, String> {
    let mut socket = UnixStream::connect(state_dir().join("control.sock"))
        .map_err(|_| "Connect service unavailable".to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    socket
        .write_all(command.as_bytes())
        .map_err(|e| e.to_string())?;
    socket
        .shutdown(Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut reply = String::new();
    socket
        .take(65536)
        .read_to_string(&mut reply)
        .map_err(|e| e.to_string())?;
    Ok(reply)
}
fn apply(window: &MainWindow, snapshot: &Value) {
    let state = snapshot["status"]["state"]
        .as_str()
        .unwrap_or("disconnected");
    window.set_online(state == "connected" || state == "pairing");
    window.set_paired(snapshot["status"]["paired"].as_bool().unwrap_or(false));
    window.set_pairing(state == "pairing");
    window.set_peer_name(
        snapshot["peer"]["name"]
            .as_str()
            .unwrap_or("Your laptop")
            .into(),
    );
    let m = &snapshot["media"];
    window.set_player(
        m["player"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .into(),
    );
    window.set_player_count(m["players"].as_array().map_or(0, |p| p.len() as i32));
    window.set_track(m["title"].as_str().unwrap_or_default().into());
    window.set_artist(m["artist"].as_str().unwrap_or_default().into());
    let playing = m["playing"].as_bool().unwrap_or(false);
    window.set_playing(playing);
    window.set_can_toggle(
        m[if playing { "can_pause" } else { "can_play" }]
            .as_bool()
            .unwrap_or(false),
    );
    window.set_can_next(m["can_next"].as_bool().unwrap_or(false));
    window.set_can_previous(m["can_previous"].as_bool().unwrap_or(false));
    window.set_volume(m["volume"].as_i64().map_or(-1, |v| v.clamp(0, 100) as i32));
}
fn preview(name: &str) -> Value {
    let mut s = json!({"status":{"state":"connected","paired":true},"peer":{"name":"Lukas’s laptop"},"media":{"players":["Spotify","Firefox"],"player":"Spotify","title":"Everything In Its Right Place","artist":"Radiohead · Kid A","playing":true,"can_pause":true,"can_play":true,"can_next":true,"can_previous":true,"volume":65}});
    match name {
        "offline" => s["status"]["state"] = json!("disconnected"),
        "pair" => s["status"]["paired"] = json!(false),
        "pairing" => {
            s["status"]["paired"] = json!(false);
            s["status"]["state"] = json!("pairing");
        }
        "empty" => s["media"] = json!({}),
        "long" => {
            s["peer"]["name"] = json!("A laptop with an unusually long name");
            s["media"]["title"] =
                json!("An extraordinarily long track title with no convenient place to finish");
            s["media"]["artist"] = json!("A very long artist name featuring another musician");
        }
        _ => {}
    }
    s
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    let args: Vec<String> = std::env::args().collect();
    let demo = args
        .iter()
        .position(|s| s == "--preview")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let capture = args
        .iter()
        .position(|s| s == "--capture")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let (tx, rx) = mpsc::sync_channel::<String>(4);
    let feedback_timer = std::rc::Rc::new(slint::Timer::default());
    let weak = window.as_weak();
    window.on_close_app(|| {
        let _ = slint::quit_event_loop();
    });
    let is_demo = demo.is_some();
    let volume = Arc::new(Mutex::new(volume::Volume::default()));
    let crown_volume = volume.clone();
    let crown = std::rc::Rc::new(std::cell::RefCell::new(crown::Crown::default()));
    let reset = crown.clone();
    let reset_volume = volume.clone();
    window.on_reset_crown(move || {
        reset.borrow_mut().reset();
        reset_volume.lock().unwrap().clear();
    });
    let crown_window = window.as_weak();
    window.on_crown_scroll(move |delta| {
        if let Some(w) = crown_window.upgrade() {
            let enabled = w.get_music_page()
                && w.get_online()
                && w.get_paired()
                && !w.get_player().is_empty();
            let command = crown.borrow_mut().step(delta, enabled, w.get_volume());
            if let Some(change) = command {
                let target = (w.get_volume() + change).clamp(0, 100);
                w.set_volume(target);
                if !is_demo {
                    crown_volume
                        .lock()
                        .unwrap()
                        .set(w.get_player().to_string(), target);
                }
            }
        }
    });
    let action_volume = volume.clone();
    window.on_action(move |command| {
        if let Some(w) = weak.upgrade() {
            if is_demo {
                return;
            }
            if matches!(command.as_str(), "volume-up" | "volume-down") {
                if w.get_online()
                    && w.get_paired()
                    && w.get_volume() >= 0
                    && !w.get_player().is_empty()
                {
                    let target = (w.get_volume() + if command == "volume-up" { 5 } else { -5 })
                        .clamp(0, 100);
                    w.set_volume(target);
                    action_volume
                        .lock()
                        .unwrap()
                        .set(w.get_player().to_string(), target);
                }
                return;
            }
            if w.get_busy() {
                return;
            }
            if command == "next-player" || command == "previous-player" {
                action_volume.lock().unwrap().clear();
            }
            if tx.try_send(command.to_string()).is_ok() {
                w.set_busy(true);
            }
        }
    });
    if let Some(ref name) = demo {
        apply(&window, &preview(name));
        window.set_music_page(matches!(name.as_str(), "music" | "empty" | "long"));
        if name == "ping" {
            window.set_feedback("Hello from your laptop".into());
        }
        if name == "notice" {
            window.set_feedback("A very long incoming message that should remain inside the notification area without covering the surrounding controls".into());
        }
    } else {
        let weak = window.as_weak();
        thread::spawn(move || {
            let mut last_ping = None;
            let mut last_action = None;
            let mut last_snapshot = Instant::now() - Duration::from_secs(1);
            loop {
                let mut notice = None;
                if let Ok(command) = rx.try_recv() {
                    match request(&command) {
                        Ok(s) if s == "queued" => {
                            if command == "pair" {
                                notice = Some("Accept on your laptop".to_string());
                            }
                        }
                        Ok(_) => notice = Some("Busy. Try again.".to_string()),
                        Err(e) => notice = Some(e),
                    }
                }
                let target = volume.lock().unwrap().take();
                if let Some((player, value)) = target {
                    let command = format!("volume-set:{}", json!({"player":player,"volume":value}));
                    if request(&command).as_deref() != Ok("queued") {
                        volume.lock().unwrap().clear();
                        notice = Some("Volume update failed".into());
                    }
                }
                if notice.is_none() && last_snapshot.elapsed() < Duration::from_millis(500) {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                last_snapshot = Instant::now();
                let snapshot = request("snapshot")
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or_else(|| json!({}));
                let ping = snapshot["ping"]["received_ms"].as_u64().unwrap_or(0);
                if last_ping.is_some_and(|old| ping > old) {
                    notice = Some(
                        snapshot["ping"]["message"]
                            .as_str()
                            .unwrap_or("Ping received")
                            .chars()
                            .take(90)
                            .collect(),
                    );
                }
                last_ping = Some(ping);
                let sent = snapshot["action"]["sent_ms"].as_u64().unwrap_or(0);
                if last_action.is_some_and(|old| sent > old) {
                    notice = Some("Ping sent".into());
                }
                last_action = Some(sent);
                let weak = weak.clone();
                let ui_volume = volume.clone();
                if slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        apply(&w, &snapshot);
                        let value = ui_volume.lock().unwrap().displayed(
                            &w.get_player(),
                            w.get_volume(),
                            w.get_online() && w.get_paired(),
                        );
                        w.set_volume(value);
                        w.set_busy(false);
                        if let Some(message) = notice {
                            w.set_feedback(message.into());
                        }
                    }
                })
                .is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
    }
    // A single UI timer expires transient notices; no animation or rendering loop.
    let weak = window.as_weak();
    let mut previous = String::new();
    let mut ticks = 0;
    feedback_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            if let Some(w) = weak.upgrade() {
                let current = w.get_feedback().to_string();
                if current != previous {
                    ticks = 0;
                    previous = current;
                }
                ticks += 1;
                if ticks >= 6 && !previous.is_empty() {
                    w.set_feedback("".into());
                }
            }
        },
    );
    let capture_timer = slint::Timer::default();
    if let Some(path) = capture {
        // Real Slint renderer output. Preview mode never contacts the daemon.
        let weak = window.as_weak();
        capture_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(2),
            move || {
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let w = weak.upgrade().ok_or("Window closed")?;
                    let pixels = w.window().take_snapshot()?;
                    image::save_buffer(
                        &path,
                        pixels.as_bytes(),
                        pixels.width(),
                        pixels.height(),
                        image::ColorType::Rgba8,
                    )?;
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("Capture failed: {e}");
                }
                let _ = slint::quit_event_loop();
            },
        );
    }
    window.run()?;
    Ok(())
}
