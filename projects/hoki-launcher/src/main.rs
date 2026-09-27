mod catalog;
mod desktop;
mod icons;
#[cfg(test)]
mod ui_tests;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use slint::Model;

slint::include_modules!();

/// Crown scroll ticks needed to move one item (reduces sensitivity ~5x).
const SCROLL_TICKS_PER_ITEM: i32 = 5;

fn main() {
    let window = MainWindow::new().unwrap();

    // Accumulated crown scroll ticks (reset when selection moves)
    let scroll_accum = Arc::new(AtomicI32::new(0));

    let config = catalog::read_config();
    let apps = scan_desktop_files();
    let show_icons = config
        .get("icons")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let images = apps
        .iter()
        .map(|app| {
            if show_icons {
                icons::load(&app.icon)
            } else {
                slint::Image::default()
            }
        })
        .collect();
    let browser = Rc::new(RefCell::new(Browser {
        apps,
        images,
        config,
        current: String::new(),
        parent_index: 0,
    }));
    browser.borrow().render(&window, 0);
    {
        let browser = browser.clone();
        let weak = window.as_weak();
        window.on_refresh_catalog(move || {
            let Some(win) = weak.upgrade() else { return };
            let mut browser = browser.borrow_mut();
            let selected = win.get_selected_index();
            browser.apps = scan_desktop_files();
            browser.images = browser.apps.iter().map(|app| {
                if browser.config.get("icons").and_then(serde_json::Value::as_bool).unwrap_or(true) {
                    icons::load(&app.icon)
                } else {
                    slint::Image::default()
                }
            }).collect();
            browser.render(&win, selected);
        });
    }
    start_stdin_reader(window.as_weak(), scroll_accum.clone());
    {
        let weak = window.as_weak();
        let browser = browser.clone();
        let accum = scroll_accum.clone();
        window.on_row_activated(move |index| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            accum.store(0, Ordering::Relaxed);
            browser.borrow_mut().activate(&win, index);
        });
    }
    {
        let weak = window.as_weak();
        let browser = browser.clone();
        let accum = scroll_accum.clone();
        window.on_top_pressed(move || {
            accum.store(0, Ordering::Relaxed);
            let Some(win) = weak.upgrade() else {
                return;
            };
            let mut browser = browser.borrow_mut();
            if browser.current.is_empty() {
                println!("go-settings");
            } else {
                browser.back(&win);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_bottom_pressed(move || {
            if let Some(win) = weak.upgrade() {
                win.invoke_row_activated(win.get_selected_index());
            }
        });
    }

    window.run().unwrap();
}

struct Browser {
    apps: Vec<desktop::DesktopEntry>,
    images: Vec<slint::Image>,
    config: serde_json::Value,
    current: String,
    parent_index: i32,
}
impl Browser {
    fn render(&self, window: &MainWindow, selected: i32) {
        let rows = catalog::rows(&self.apps, &self.config, &self.current);
        let entries: Vec<AppEntry> = rows
            .iter()
            .map(|row| match row {
                catalog::Row::App(i) => AppEntry {
                    name: self.apps[*i].name.clone().into(),
                    icon: self.images[*i].clone(),
                    has_icon: self.images[*i].size().width > 0,
                    folder: false,
                    back: false,
                },
                catalog::Row::Folder(name) => AppEntry {
                    name: name.clone().into(),
                    folder: true,
                    ..Default::default()
                },
                catalog::Row::Back => AppEntry {
                    name: "All apps".into(),
                    back: true,
                    ..Default::default()
                },
            })
            .collect();
        window.set_page_title(
            if self.current.is_empty() {
                "Apps"
            } else {
                &self.current
            }
            .into(),
        );
        window.set_apps(Rc::new(slint::VecModel::from(entries)).into());
        window.set_selected_index(selected.clamp(0, rows.len().saturating_sub(1) as i32));
    }
    fn back(&mut self, window: &MainWindow) {
        self.current.clear();
        self.render(window, self.parent_index);
    }
    fn activate(&mut self, window: &MainWindow, index: i32) {
        if index < 0 {
            return;
        }
        match catalog::rows(&self.apps, &self.config, &self.current).get(index as usize) {
            Some(catalog::Row::App(i)) => {
                launch_app(&serde_json::to_string(&self.apps[*i].argv).unwrap())
            }
            Some(catalog::Row::Folder(name)) => {
                self.parent_index = index;
                self.current = name.clone();
                self.render(window, 1);
            }
            Some(catalog::Row::Back) => self.back(window),
            None => {}
        }
    }
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
    let dirs = if let Some(dir) = std::env::var_os("HOKI_APPLICATIONS_DIR") {
        vec![std::path::PathBuf::from(dir)]
    } else {
        let system = std::path::PathBuf::from("/usr/share/applications");
        let mut dirs = vec![if system.exists() { system } else { "test-apps".into() }];
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")));
        if let Some(data_home) = data_home {
            dirs.push(data_home.join("applications"));
        }
        dirs
    };
    let mut apps = std::collections::HashMap::new();
    for entry in dirs.into_iter().filter_map(|dir| std::fs::read_dir(dir).ok()).flatten().flatten() {
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
                        apps.insert(app.id.clone(), app);
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("Ignoring {}: {e}", path.display()),
            }
        }
    }
    let mut apps: Vec<_> = apps.into_values().collect();
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
            window.invoke_refresh_catalog();
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
