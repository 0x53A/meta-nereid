// SPDX-License-Identifier: GPL-3.0-only
mod gps;
mod route;
#[cfg(test)]
mod tests;
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    time::Duration,
};
slint::include_modules!();

fn duration(seconds: &Value) -> String {
    let n = seconds
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .unwrap_or(0.) as u64;
    if n >= 3600 {
        format!("{}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60)
    } else {
        format!("{}:{:02}", n / 60, n % 60)
    }
}
fn pace(seconds: &Value) -> String {
    if seconds.as_f64().is_some_and(|v| v > 0. && v.is_finite()) {
        duration(seconds)
    } else {
        "—".into()
    }
}
fn apply(window: &MainWindow, snapshot: &Value) {
    let a = &snapshot["activity"];
    let s = &snapshot["sensors"];
    let phase = a["state"].as_str().unwrap_or("idle");
    if window.get_phase() != phase && matches!(phase, "stopped" | "interrupted") {
        window.set_track_page(true);
    }
    window.set_phase(phase.into());
    window.set_connected(true);
    window.set_ready(s["ready"] == true);
    let distance = format!("{:.2} km", a["distance_m"].as_f64().unwrap_or(0.) / 1000.);
    let active = duration(&a["active_seconds"]);
    window.set_status(match phase {
        "idle" => "Choose your activity".into(),
        "paused" => "Paused · still recording".into(),
        "stopped" | "interrupted" => format!("{active} active · {distance}").into(),
        _ if a["gps_lock"] == true => "GPS ready".into(),
        "prepared" => "GPS acquiring · start allowed".into(),
        _ => "GPS acquiring".into(),
    });
    window.set_active_time(active.into());
    window.set_distance(distance.into());
    let heart = s["heart_rate"]["bpm"]
        .as_f64()
        .map(|b| format!("{b:.0}"))
        .unwrap_or("—".into());
    window.set_pace_heart(format!("{} /km · {} bpm", pace(&a["pace_seconds_km"]), heart).into());
    window.set_elapsed_average(
        format!(
            "Elapsed {} · avg {}",
            duration(&a["elapsed_seconds"]),
            pace(&a["average_pace_seconds_km"])
        )
        .into(),
    );
    let notice = snapshot["error"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| a["gps_error"].as_str().filter(|s| !s.is_empty()))
        .unwrap_or(if phase == "prepared" && s["ready"] != true {
            "Preparing sensor recording…"
        } else if s["optical_window"] == true
            && !matches!(phase, "stopped" | "interrupted" | "idle")
        {
            "SpO₂ window · beats suspended"
        } else {
            ""
        });
    window.set_notice(notice.into());
    let (image, has_track) = route::render(&a["track"]);
    window.set_route(image);
    window.set_has_track(has_track);
}
fn request(command: &str) -> Result<Value, String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "Activity runtime unavailable")?;
    let mut socket = UnixStream::connect(format!("{runtime}/hoki-activity/control.sock"))
        .map_err(|_| "Activity service unavailable")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(6)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    writeln!(socket, "{}", json!({"command":command,"profile":"running"}))
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(socket.take(1_048_577))
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > 1_048_576 || !line.ends_with('\n') {
        return Err("Invalid activity response".into());
    }
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if reply["ok"] != true {
        return Err(reply["error"]
            .as_str()
            .unwrap_or("Activity command failed")
            .into());
    }
    Ok(reply)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--gps") {
        return gps::run();
    }
    // Service startup and all socket work stay off the UI thread.
    let window = MainWindow::new()?;
    let (tx, rx) = mpsc::sync_channel::<String>(1);
    let weak = window.as_weak();
    window.on_action(move |command| {
        if let Some(w) = weak.upgrade() {
            if !w.get_busy() && tx.try_send(command.to_string()).is_ok() {
                w.set_busy(true);
            }
        }
    });
    window.on_close_app(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = window.as_weak();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "start", "hoki-activity.service"])
            .status();
        let mut command = "status".to_string();
        loop {
            let result = request(&command);
            let action = command != "status";
            if weak
                .upgrade_in_event_loop(move |w| {
                    if action {
                        w.set_busy(false);
                    }
                    match result {
                        Ok(snapshot) => apply(&w, &snapshot),
                        Err(error) => {
                            w.set_notice(error.into());
                            if !action {
                                w.set_connected(false);
                            }
                        }
                    }
                })
                .is_err()
            {
                break;
            }
            command = match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => "status".into(),
                Err(_) => break,
            };
        }
    });
    window.run()?;
    Ok(())
}
