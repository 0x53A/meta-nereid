mod desktop;
use std::io::{BufRead, BufReader};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use slint::Model;

slint::include_modules!();

/// Crown scroll ticks needed to move one item (reduces sensitivity ~5x).
const SCROLL_TICKS_PER_ITEM: i32 = 5;

fn main() {
    let window = MainWindow::new().unwrap();

    // Accumulated crown scroll ticks (reset when selection moves)
    let scroll_accum = Arc::new(AtomicI32::new(0));

    // Scan .desktop files
    let apps = scan_desktop_files();
    let app_model: Vec<AppEntry> = apps
        .iter()
        .map(|a| AppEntry {
            name: a.name.clone().into(),
            exec: serde_json::to_string(&a.argv)
                .expect("serializable arguments")
                .into(),
        })
        .collect();
    window.set_apps(Rc::new(slint::VecModel::from(app_model)).into());

    // Start background threads
    start_stdin_reader(window.as_weak(), scroll_accum.clone());

    // Handle app launch (from touch tap or crown+bottom)
    window.on_app_launched(|exec| {
        launch_app(&exec);
    });

    {
        let accum = scroll_accum.clone();
        window.on_top_pressed(move || {
            accum.store(0, Ordering::Relaxed);
            println!("go-settings");
        });
    }

    {
        let weak = window.as_weak();
        window.on_bottom_pressed(move || {
            let window = weak.unwrap();
            let idx = window.get_selected_index();
            let apps = window.get_apps();
            if idx >= 0 && (idx as usize) < apps.row_count() {
                if let Some(app) = apps.row_data(idx as usize) {
                    launch_app(&app.exec);
                }
            }
        });
    }

    window.run().unwrap();
}

fn launch_app(encoded: &str) {
    // The row retains the argument vector; no shell or whitespace round-trip.
    if let Ok(args) = serde_json::from_str::<Vec<String>>(encoded) {
        if !args.is_empty() {
            println!("launch-argv:{}", serde_json::to_string(&args).unwrap());
        }
    }
}

fn scan_desktop_files() -> Vec<desktop::DesktopEntry> {
    let entries = if let Some(dir) = std::env::var_os("HOKI_APPLICATIONS_DIR") {
        std::fs::read_dir(dir)
    } else {
        std::fs::read_dir("/usr/share/applications").or_else(|_| std::fs::read_dir("test-apps"))
    };
    let Ok(entries) = entries else {
        return Vec::new();
    };
    let mut apps = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            match desktop::parse(&text, &path) {
                Ok(Some(mut app)) => {
                    app.argv = desktop::resolve_exec(app.argv, std::path::Path::new("/usr/lib"));
                    if !app.argv.first().is_some_and(|cmd| {
                        std::path::Path::new(cmd)
                            .file_name()
                            .is_some_and(|n| n == "hoki-launcher")
                    }) {
                        apps.push(app);
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("Ignoring {}: {e}", path.display()),
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

// --- Stdin reader (compositor commands) ---

fn start_stdin_reader(window: slint::Weak<MainWindow>, scroll_accum: Arc<AtomicI32>) {
    std::thread::spawn(move || {
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
        "app-closed" => {
            scroll_accum.store(0, Ordering::Relaxed);
        }
        _ if msg.starts_with("scroll:") => {
            if let Ok(delta) = msg[7..].parse::<i32>() {
                let acc = scroll_accum.load(Ordering::Relaxed) + delta;
                let items_to_move = acc / SCROLL_TICKS_PER_ITEM;
                let apps = window.get_apps();
                let count = apps.row_count() as i32;
                if count > 0 {
                    if items_to_move != 0 {
                        scroll_accum.store(acc % SCROLL_TICKS_PER_ITEM, Ordering::Relaxed);
                        let mut idx = window.get_selected_index() + items_to_move;
                        idx = idx.clamp(0, count - 1);
                        window.set_selected_index(idx);
                    } else {
                        scroll_accum.store(acc, Ordering::Relaxed);
                    }
                }
            }
        }
        _ => {}
    }
}
