#[path = "../../shared/role_command.rs"]
mod role_command;
mod clock_wait;
mod active_role;
#[path = "../../shared/config_file.rs"]
mod config_file;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

slint::include_modules!();

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--watchface") {
        run_watchface();
    } else {
        run_setup();
    }
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/ceres".into());
    PathBuf::from(home).join(".config/hoki/watchface.conf")
}

fn load_show_seconds() -> bool {
    std::fs::read_to_string(config_path())
        .unwrap_or_default()
        .lines()
        .any(|l| l.trim() == "show_seconds=true")
}

fn save_show_seconds(val: bool) -> std::io::Result<()> {
    config_file::save_values(&config_path(), &[("show_seconds", val.to_string())])
}

// --- Watchface mode: full-screen time display, managed by compositor ---

fn run_watchface() {
    let clock = clock_wait::ClockWait::new().expect("clock wake sources");

    let window = MainWindow::new().unwrap();
    window.set_mode("watchface".into());

    let mut show_secs = load_show_seconds();
    window.set_show_seconds(show_secs);

    // Set time immediately to avoid "00:00" flash on first frame
    let now = chrono::Local::now();
    let fmt = if show_secs { "%H:%M:%S" } else { "%H:%M" };
    window.set_time_text(now.format(fmt).to_string().into());
    window.set_date_text(now.format("%A, %B %-d").to_string().into());

    // Clock updater — sleeps 1s with seconds, 1min without
    let weak = window.as_weak();
    std::thread::spawn(move || loop {
        let now = chrono::Local::now();
        let fmt = if show_secs { "%H:%M:%S" } else { "%H:%M" };
        let time = now.format(fmt).to_string();
        let date = now.format("%A, %B %-d").to_string();

        let w = weak.clone();
        let ss = show_secs;
        slint::invoke_from_event_loop(move || {
            if let Some(win) = w.upgrade() {
                win.set_time_text(time.into());
                win.set_date_text(date.into());
                win.set_show_seconds(ss);
            }
        })
        .ok();

        match clock.wait(now.timestamp(), show_secs) {
            Ok(true) => show_secs = load_show_seconds(),
            Ok(false) => {}
            Err(e) => {
                eprintln!("clock wait failed: {e}");
                std::process::exit(1);
            }
        }
    });

    // Compositor stdin messages (scroll, etc. — currently unused by minimal watchface)
    std::thread::spawn(move || {
        let reader = BufReader::new(std::io::stdin().lock());
        for _line in reader.lines().map_while(Result::ok) {
            // Future: handle scroll for complications, etc.
        }
    });

    window.run().unwrap();
}

fn signal_watchface_reload() {
    // Only notify watchfaces in this runtime, including when several desktop
    // simulator sessions run under the same Unix user.
    let our_pid = std::process::id();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let runtime_entry = format!("XDG_RUNTIME_DIR={runtime}");
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let pid_str = pid_str.to_string_lossy();
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid == our_pid {
                    continue;
                }
                let cmdline =
                    std::fs::read_to_string(format!("/proc/{}/cmdline", pid)).unwrap_or_default();
                let args: Vec<_> = cmdline.split('\0').collect();
                let is_watchface = args.first().is_some_and(|exe|
                    std::path::Path::new(exe).file_name().is_some_and(|name| name == "hoki-watchface"))
                    && args.contains(&"--watchface");
                if !is_watchface { continue; }
                let environment = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
                if environment.split(|b| *b == 0).any(|entry| entry == runtime_entry.as_bytes()) {
                    unsafe { libc::kill(pid as i32, libc::SIGHUP) };
                }
            }
        }
    }
}

// --- Setup mode: launched as app, shows "set as watchface" button ---

fn run_setup() {
    let window = MainWindow::new().unwrap();
    window.set_mode("setup".into());
    window.set_is_active(check_is_active());
    window.set_show_seconds(load_show_seconds());

    let weak = window.as_weak();
    window.on_set_watchface(move || {
        let Some(win) = weak.upgrade() else {
            return;
        };
        if win.get_busy() {
            return;
        }
        win.set_busy(true);
        win.set_status_text("Saving…".into());
        let weak = weak.clone();
        std::thread::spawn(move || {
            let result = std::env::current_exe()
                .and_then(|exe| role_command::watchface(&exe, None))
                .and_then(|command| send_ctl_command(&command));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_busy(false);
                    match result {
                        Ok(()) => {
                            win.set_is_active(true);
                            win.set_status_text("".into());
                        }
                        Err(e) => {
                            win.set_status_text(format!("Could not set watchface: {e}").into())
                        }
                    }
                }
            });
        });
    });

    let weak = window.as_weak();
    window.on_toggle_seconds(move || {
        let Some(win) = weak.upgrade() else {
            return;
        };
        if win.get_busy() {
            return;
        }
        let new_val = !win.get_show_seconds();
        win.set_busy(true);
        win.set_status_text("Saving…".into());
        let weak = weak.clone();
        std::thread::spawn(move || {
            let result = save_show_seconds(new_val);
            if result.is_ok() {
                signal_watchface_reload();
            }
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_busy(false);
                    match result {
                        Ok(()) => {
                            win.set_show_seconds(new_val);
                            win.set_status_text("".into());
                        }
                        Err(e) => {
                            win.set_status_text(format!("Could not save seconds: {e}").into())
                        }
                    }
                }
            });
        });
    });

    window.run().unwrap();
}

fn check_is_active() -> bool {
    let sock = ctl_socket_path();
    if let Ok(mut stream) = UnixStream::connect(&sock) {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .ok();
        if writeln!(stream, "get-watchface").is_ok() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                return active_role::is_watchface(&line);
            }
        }
    }
    false
}

fn send_ctl_command(cmd: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(ctl_socket_path())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    writeln!(stream, "{cmd}")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim() == "ok" {
        Ok(())
    } else {
        Err(std::io::Error::other(if reply.is_empty() {
            "No response".into()
        } else {
            reply.trim().to_string()
        }))
    }
}

fn ctl_socket_path() -> String {
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    format!("{}/hoki-compositor.sock", xdg)
}
