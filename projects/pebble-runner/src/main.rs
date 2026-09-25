#[path = "../../shared/role_command.rs"]
mod role_command;
#[cfg(test)]
mod lifecycle_tests;
mod owned;
mod guest_heap;
mod display_frame;
mod accel;
mod persist;
mod ticks;
mod compass;
mod bluetooth;
mod executor;
#[cfg(not(target_arch = "arm"))]
mod emu;
mod font;
mod gcolor;
mod pbw;
mod pebble_api;
mod runtime;
mod resources;
mod store;

use slint::{Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

slint::include_modules!();

/// Where to look for .pbw files
fn find_pbw_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(&home).join(".pebble-apps");
        if p.exists() {
            return p;
        }
        if std::fs::create_dir_all(&p).is_ok() {
            return p;
        }
    }
    for c in &["test-apps", "../test-apps"] {
        let p = PathBuf::from(c);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(".")
}

/// Scan a directory for .pbw files and parse their headers
fn scan_pbw_dir(dir: &Path) -> Vec<(String, String, String, bool)> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Can't read {}: {}", dir.display(), e);
            return results;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "pbw") {
            let platform = ["chalk", "basalt", "diorite", "emery", "aplite"]
                .iter()
                .find(|p| pbw::extract_from_pbw(&path, p).is_ok());

            if let Some(&platform) = platform {
                match pbw::extract_from_pbw(&path, platform) {
                    Ok((bin, _)) => match pbw::parse_header(&bin) {
                        Ok(info) => {
                            let filename =
                                path.file_name().unwrap().to_string_lossy().to_string();
                            results.push((
                                info.name.clone(),
                                info.company.clone(),
                                filename,
                                info.is_watchface(),
                            ));
                        }
                        Err(e) => eprintln!("  Bad header in {}: {}", path.display(), e),
                    },
                    Err(e) => eprintln!("  Can't extract {}: {}", path.display(), e),
                }
            }
        }
    }

    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

/// Build Slint model from local app list
fn make_app_model(apps: &[(String, String, String, bool)]) -> ModelRc<PbwEntry> {
    let entries: Vec<PbwEntry> = apps
        .iter()
        .map(|(name, company, filename, is_wf)| PbwEntry {
            name: SharedString::from(name.as_str()),
            company: SharedString::from(company.as_str()),
            filename: SharedString::from(filename.as_str()),
            is_watchface: *is_wf,
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// Build Slint model from store app list
fn make_store_model(items: &[store::StoreApp], pbw_dir: &Path) -> ModelRc<StoreEntry> {
    let entries: Vec<StoreEntry> = items
        .iter()
        .map(|app| {
            let status = if app.is_installed(pbw_dir) {
                "installed"
            } else {
                ""
            };
            StoreEntry {
                title: SharedString::from(app.title.as_str()),
                author: SharedString::from(app.author.as_str()),
                hearts: app.hearts as i32,
                status: SharedString::from(status),
                has_screenshot: app.chalk_screenshot_url().is_some(),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// Refresh installed apps on the UI
fn refresh_installed(window: &MainWindow, pbw_dir: &Path) -> Vec<(String, String, String, bool)> {
    let apps = scan_pbw_dir(pbw_dir);
    window.set_apps(make_app_model(&apps));
    window.set_installed_count(apps.len() as i32);
    apps
}

/// A store collection: its items and pagination offset
#[derive(Default)]
struct CollectionState {
    items: Vec<store::StoreApp>,
    offset: u32,
}

/// All cached store data
struct StoreState {
    /// view_id -> collection state (2=all faces, 3=all apps, 4=most loved)
    collections: std::collections::HashMap<i32, CollectionState>,
    /// Which view was active before entering preview
    prev_view: i32,
}

impl StoreState {
    fn new() -> Self {
        Self {
            collections: std::collections::HashMap::new(),
            prev_view: 0,
        }
    }

    fn get_or_insert(&mut self, view: i32) -> &mut CollectionState {
        self.collections.entry(view).or_default()
    }
}

/// Map view ID to (slug, app_type) for the Rebble API
fn view_to_collection(view: i32) -> (&'static str, &'static str) {
    match view {
        2 => ("all", "watchfaces"),
        3 => ("all", "watchapps"),
        4 => ("most-loved", "watchfaces"),
        _ => ("all", "watchfaces"),
    }
}

/// Decode a PNG or GIF image into an RGBA Slint Image
fn decode_image_to_slint(data: &[u8]) -> Option<slint::Image> {
    let img = image::load_from_memory(data).ok()?;
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();

    let mut pixel_buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
    pixel_buf.make_mut_bytes().copy_from_slice(rgba.as_raw());
    Some(slint::Image::from_rgba8(pixel_buf))
}

fn run_headless(pbw_path: &Path) {
    let pebble_state = runtime::new_shared_state();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Set up framebuffer
    {
        let s = pebble_state.lock().unwrap();
        pebble_api::set_framebuffer_ptr(s.framebuffer.as_ptr() as *mut u8);
    }

    let platform = ["chalk", "basalt", "diorite", "emery", "aplite"]
        .iter()
        .find(|p| pbw::extract_from_pbw(pbw_path, p).is_ok())
        .copied()
        .unwrap_or("chalk");

    match runtime::load_pbw(&pebble_state, pbw_path, platform) {
        Ok((bin_data, res_data)) => {
            pebble_api::set_resource_pack(res_data);
            let info = pbw::parse_header(&bin_data).unwrap();

            {
                let s = pebble_state.lock().unwrap();
                pebble_api::set_framebuffer_ptr(s.framebuffer.as_ptr() as *mut u8);
            }

            #[cfg(target_arch = "arm")]
            {
                match executor::load_binary(&bin_data, &info) {
                    Ok(entry) => {
                        if let Err(e) = executor::execute(&entry, stop_flag) {
                            eprintln!("Pebble app error: {}", e);
                        }
                    }
                    Err(e) => eprintln!("Failed to load binary: {}", e),
                }
            }
            #[cfg(not(target_arch = "arm"))]
            {
                if let Err(e) = emu::load_and_execute(&bin_data, &info, stop_flag) {
                    eprintln!("Pebble app error: {}", e);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to load PBW: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_watchface(pbw_path: &Path) {
    let pebble_state = runtime::new_shared_state();
    let stop_flag = Arc::new(AtomicBool::new(false));

    {
        let s = pebble_state.lock().unwrap();
        pebble_api::set_framebuffer_ptr(s.framebuffer.as_ptr() as *mut u8);
    }

    let platform = ["chalk", "basalt", "diorite", "emery", "aplite"]
        .iter()
        .find(|p| pbw::extract_from_pbw(pbw_path, p).is_ok())
        .copied()
        .unwrap_or("chalk");

    match runtime::load_pbw(&pebble_state, pbw_path, platform) {
        Ok((bin_data, res_data)) => {
            pebble_api::set_resource_pack(res_data);
            let info = pbw::parse_header(&bin_data).unwrap();

            let window = MainWindow::new().unwrap();
            window.set_watchface_mode(true);
            window.set_running(true);
            window.set_current_app(SharedString::from(info.name.as_str()));

            // Start pebble execution thread
            let stop = stop_flag.clone();
            let state_for_thread = pebble_state.clone();

            std::thread::spawn(move || {
                {
                    let s = state_for_thread.lock().unwrap();
                    pebble_api::set_framebuffer_ptr(s.framebuffer.as_ptr() as *mut u8);
                }

                #[cfg(target_arch = "arm")]
                {
                    match executor::load_binary(&bin_data, &info) {
                        Ok(entry) => {
                            if let Err(e) = executor::execute(&entry, stop) {
                                eprintln!("Pebble watchface error: {}", e);
                            }
                        }
                        Err(e) => eprintln!("Failed to load binary: {}", e),
                    }
                }
                #[cfg(not(target_arch = "arm"))]
                {
                    if let Err(e) = emu::load_and_execute(&bin_data, &info, stop) {
                        eprintln!("Pebble watchface error: {}", e);
                    }
                }
            });

            // Framebuffer refresh at 10 FPS
            let timer = slint::Timer::default();
            {
                let window_weak = window.as_weak();
                let mut changed = display_frame::ChangedFrame::default();
                timer.start(
                    slint::TimerMode::Repeated,
                    std::time::Duration::from_millis(100),
                    move || {
                        if let Some(w) = window_weak.upgrade() {
                            let Some(display_buf) = changed.take(pebble_api::get_display_buffer()) else {
                                return;
                            };
                            let out_size = 416;
                            let mut pixel_buf =
                                SharedPixelBuffer::<slint::Rgba8Pixel>::new(out_size, out_size);
                            render_scaled(
                                &display_buf,
                                runtime::DISPLAY_WIDTH,
                                runtime::DISPLAY_HEIGHT,
                                pixel_buf.make_mut_bytes(),
                                out_size as usize,
                            );

                            w.set_framebuffer(slint::Image::from_rgba8(pixel_buf));
                        }
                    },
                );
            }

            // Read compositor stdin in background (mostly unused)
            std::thread::spawn(move || {
                let mut input = std::io::stdin().lock();
                if let Err(error) = std::io::copy(&mut input, &mut std::io::sink()) {
                    eprintln!("compositor stdin drain failed: {error}");
                }
            });

            window.run().unwrap();
            stop_flag.store(true, Ordering::Relaxed);
        }
        Err(e) => {
            eprintln!("Failed to load PBW: {}", e);
            std::process::exit(1);
        }
    }
}

/// Send a command to the compositor's control socket.
fn send_ctl_command(cmd: &str) {
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let sock_path = format!("{}/hoki-compositor.sock", xdg);
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock_path) {
        writeln!(stream, "{}", cmd).ok();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // --watchface <path.pbw>: run as compositor watchface role (full-screen, no UI)
    if let Some(pos) = args.iter().position(|a| a == "--watchface") {
        if let Some(pbw_path_str) = args.get(pos + 1) {
            run_watchface(Path::new(pbw_path_str));
            return;
        } else {
            eprintln!("Usage: pebble-runner --watchface <path.pbw>");
            std::process::exit(1);
        }
    }

    // --load <path.pbw>: headless mode, load and run a single watchface without GUI
    if let Some(pos) = args.iter().position(|a| a == "--load") {
        if let Some(pbw_path_str) = args.get(pos + 1) {
            run_headless(Path::new(pbw_path_str));
            return;
        } else {
            eprintln!("Usage: pebble-runner --load <path.pbw>");
            std::process::exit(1);
        }
    }

    let pbw_dir = find_pbw_dir();
    println!("Scanning {} for .pbw files...", pbw_dir.display());

    let local_apps = Arc::new(Mutex::new(scan_pbw_dir(&pbw_dir)));
    let store_state = Arc::new(Mutex::new(StoreState::new()));
    let pebble_state = runtime::new_shared_state();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let pebble_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> =
        Arc::new(Mutex::new(None));

    // Set up framebuffer pointer
    {
        let s = pebble_state.lock().unwrap();
        pebble_api::set_framebuffer_ptr(s.framebuffer.as_ptr() as *mut u8);
    }

    let window = MainWindow::new().unwrap();

    // Populate initial installed apps
    {
        let apps = local_apps.lock().unwrap();
        window.set_apps(make_app_model(&apps));
        window.set_installed_count(apps.len() as i32);
        window.set_status(SharedString::from(format!(
            "{} apps in {}",
            apps.len(),
            pbw_dir.display()
        )));
    }

    // ─── Navigate between views ───
    {
        let window_weak = window.as_weak();
        let store_state = store_state.clone();
        let local_apps = local_apps.clone();
        let pbw_dir = pbw_dir.clone();

        window.on_navigate(move |view| {
            eprintln!("Navigate to view {}", view);
            let Some(w) = window_weak.upgrade() else {
                return;
            };
            w.set_view(view);

            match view {
                0 => {
                    let apps = local_apps.lock().unwrap();
                    w.set_installed_count(apps.len() as i32);
                }
                1 => {
                    let apps = refresh_installed(&w, &pbw_dir);
                    *local_apps.lock().unwrap() = apps;
                }
                2 | 3 | 4 => {
                    // Store view — check if we have cached data
                    let ss = store_state.lock().unwrap();
                    let has_data = ss
                        .collections
                        .get(&view)
                        .map_or(false, |c| !c.items.is_empty());

                    if has_data {
                        let items = &ss.collections[&view].items;
                        w.set_store_items(make_store_model(items, &pbw_dir));
                        w.set_loading(false);
                        return;
                    }
                    drop(ss);

                    // Fetch from API in background
                    w.set_loading(true);
                    w.set_store_items(ModelRc::new(VecModel::from(Vec::<StoreEntry>::new())));
                    w.set_status(SharedString::from("Connecting to Rebble..."));

                    let ww = w.as_weak();
                    let ss_clone = store_state.clone();
                    let pbw_dir = pbw_dir.clone();
                    let (slug, app_type) = view_to_collection(view);

                    std::thread::spawn(move || {
                        let result = store::fetch_collection(slug, app_type, 0);
                        let _ = slint::invoke_from_event_loop(move || {
                            let Some(w) = ww.upgrade() else { return };
                            match result {
                                Ok(resp) => {
                                    let mut ss = ss_clone.lock().unwrap();
                                    let coll = ss.get_or_insert(view);
                                    coll.items = resp.data;
                                    coll.offset = coll.items.len() as u32;
                                    w.set_store_items(make_store_model(&coll.items, &pbw_dir));
                                    w.set_loading(false);
                                    w.set_status(SharedString::new());
                                }
                                Err(e) => {
                                    eprintln!("Store fetch error: {}", e);
                                    w.set_loading(false);
                                    w.set_status(SharedString::from(format!("Error: {}", e)));
                                }
                            }
                        });
                    });
                }
                _ => {}
            }
        });
    }

    // ─── Load more store items ───
    {
        let window_weak = window.as_weak();
        let store_state = store_state.clone();
        let pbw_dir = pbw_dir.clone();

        window.on_load_more(move || {
            let Some(w) = window_weak.upgrade() else {
                return;
            };
            let view = w.get_view();
            if view != 2 && view != 3 && view != 4 {
                return;
            }

            let offset = {
                let ss = store_state.lock().unwrap();
                ss.collections.get(&view).map_or(0, |c| c.offset)
            };

            w.set_loading(true);

            let ww = w.as_weak();
            let ss_clone = store_state.clone();
            let pbw_dir = pbw_dir.clone();
            let (slug, app_type) = view_to_collection(view);

            std::thread::spawn(move || {
                let result = store::fetch_collection(slug, app_type, offset);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = ww.upgrade() else { return };
                    match result {
                        Ok(resp) => {
                            let mut ss = ss_clone.lock().unwrap();
                            let coll = ss.get_or_insert(view);
                            coll.items.extend(resp.data);
                            coll.offset = coll.items.len() as u32;
                            w.set_store_items(make_store_model(&coll.items, &pbw_dir));
                            w.set_loading(false);
                            w.set_status(SharedString::new());
                        }
                        Err(e) => {
                            w.set_loading(false);
                            w.set_status(SharedString::from(format!("Error: {}", e)));
                        }
                    }
                });
            });
        });
    }

    // ─── Store item selected → show preview ───
    {
        let window_weak = window.as_weak();
        let store_state = store_state.clone();
        let pbw_dir = pbw_dir.clone();

        window.on_store_item_selected(move |idx| {
            let Some(w) = window_weak.upgrade() else {
                return;
            };
            let current_view = w.get_view();
            let idx = idx as usize;

            let ss = store_state.lock().unwrap();
            let items = &ss.collections.get(&current_view);
            let Some(coll) = items else { return };
            let Some(app) = coll.items.get(idx).cloned() else {
                return;
            };
            drop(ss);

            // Save which view we came from
            store_state.lock().unwrap().prev_view = current_view;

            // Set preview info
            w.set_preview_title(SharedString::from(app.title.as_str()));
            w.set_preview_author(SharedString::from(app.author.as_str()));
            w.set_preview_index(idx as i32);
            w.set_preview_has_image(false);
            w.set_preview_status(SharedString::from(if app.is_installed(&pbw_dir) {
                "installed"
            } else {
                ""
            }));
            w.set_view(5);

            // Fetch screenshot in background
            if let Some(url) = app.chalk_screenshot_url() {
                let url = url.to_string();
                let ww = w.as_weak();

                std::thread::spawn(move || {
                    match store::download_screenshot(&url) {
                        Ok(png_data) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                let Some(w) = ww.upgrade() else { return };
                                if let Some(img) = decode_image_to_slint(&png_data) {
                                    w.set_preview_image(img);
                                    w.set_preview_has_image(true);
                                }
                            });
                        }
                        Err(e) => eprintln!("Screenshot error: {}", e),
                    }
                });
            }
        });
    }

    // ─── Preview install button ───
    {
        let window_weak = window.as_weak();
        let store_state = store_state.clone();
        let local_apps = local_apps.clone();
        let pbw_dir = pbw_dir.clone();

        window.on_preview_install(move || {
            let Some(w) = window_weak.upgrade() else {
                return;
            };
            let idx = w.get_preview_index() as usize;

            let ss = store_state.lock().unwrap();
            let prev_view = ss.prev_view;
            let Some(coll) = ss.collections.get(&prev_view) else {
                return;
            };
            let Some(app) = coll.items.get(idx).cloned() else {
                return;
            };
            drop(ss);

            if app.is_installed(&pbw_dir) {
                return;
            }
            if app.latest_release.is_none() {
                w.set_status(SharedString::from("No download available"));
                return;
            }

            w.set_preview_status(SharedString::from("downloading..."));

            let ww = w.as_weak();
            let pbw_dir = pbw_dir.clone();
            let local_apps = local_apps.clone();
            let ss_clone = store_state.clone();

            std::thread::spawn(move || {
                let result = store::download_pbw(&app, &pbw_dir);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = ww.upgrade() else { return };
                    match result {
                        Ok(_) => {
                            w.set_preview_status(SharedString::from("installed"));

                            // Also update the store list model
                            let ss = ss_clone.lock().unwrap();
                            let prev_view = ss.prev_view;
                            drop(ss);
                            // We'll refresh the store model when going back

                            // Refresh local apps
                            let apps = scan_pbw_dir(&pbw_dir);
                            w.set_installed_count(apps.len() as i32);
                            w.set_apps(make_app_model(&apps));
                            *local_apps.lock().unwrap() = apps;

                            w.set_status(SharedString::from(format!(
                                "Installed {}",
                                app.title
                            )));
                        }
                        Err(e) => {
                            w.set_preview_status(SharedString::new());
                            w.set_status(SharedString::from(format!("Failed: {}", e)));
                        }
                    }
                });
            });
        });
    }

    // Shared state for "set as watchface" — tracks the PBW path of the running app
    let current_pbw_path: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    // ─── Set as watchface button ───
    {
        let current_pbw_path = current_pbw_path.clone();
        window.on_set_as_watchface(move || {
            let path = current_pbw_path.lock().unwrap().clone();
            if !path.is_empty() {
                match std::env::current_exe().and_then(|exe| role_command::watchface(&exe, Some(Path::new(&path)))) {
                    Ok(command) => send_ctl_command(&command),
                    Err(error) => eprintln!("[pebble-runner] Cannot set watchface: {error}"),
                }
            }
        });
    }

    // Shared button queue for Pebble click handling
    let button_queue_ref: Arc<Mutex<Option<Arc<Mutex<Vec<u8>>>>>> =
        Arc::new(Mutex::new(None));

    // ─── Launch installed app ───
    {
        let window_weak = window.as_weak();
        let pebble_state = pebble_state.clone();
        let stop_flag = stop_flag.clone();
        let pebble_thread = pebble_thread.clone();
        let local_apps = local_apps.clone();
        let pbw_dir = pbw_dir.clone();
        let current_pbw_path = current_pbw_path.clone();
        let button_queue_ref = button_queue_ref.clone();

        window.on_app_selected(move |idx| {
            let idx = idx as usize;
            let apps = local_apps.lock().unwrap();
            let Some((name, _, filename, is_wf)) = apps.get(idx).cloned() else {
                return;
            };
            drop(apps);

            let pbw_path = pbw_dir.join(&filename);
            println!("Selected: {} ({})", name, pbw_path.display());

            // Stop any running app
            stop_flag.store(true, Ordering::Relaxed);
            {
                let mut thread = pebble_thread.lock().unwrap();
                if thread.as_ref().is_some_and(|handle| !handle.is_finished()) {
                    if let Some(w) = window_weak.upgrade() { w.set_status("Previous app is still stopping".into()); }
                    return;
                }
                if let Some(handle) = thread.take() { let _ = handle.join(); }
            }
            stop_flag.store(false, Ordering::Relaxed);

            pebble_api::reset_state();

            {
                let mut s = pebble_state.lock().unwrap();
                s.clear(crate::gcolor::colors::BLACK);
            }

            let platform = ["chalk", "basalt", "diorite", "emery", "aplite"]
                .iter()
                .find(|p| pbw::extract_from_pbw(&pbw_path, p).is_ok())
                .copied()
                .unwrap_or("chalk");

            // Track current PBW path for "set as watchface"
            *current_pbw_path.lock().unwrap() = pbw_path.display().to_string();

            match runtime::load_pbw(&pebble_state, &pbw_path, platform) {
                Ok((bin_data, res_data)) => {
                    pebble_api::set_resource_pack(res_data);

                    if let Some(ref w) = window_weak.upgrade() {
                        w.set_current_app(SharedString::from(name.as_str()));
                        w.set_current_app_is_watchface(is_wf);
                        w.set_running(true);
                    }

                    let info = pbw::parse_header(&bin_data).unwrap();
                    let stop = stop_flag.clone();
                    let state_for_thread = pebble_state.clone();

                    // Create button queue for click handling
                    let button_queue = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
                    // Store a clone for the UI to push button events
                    *button_queue_ref.lock().unwrap() = Some(button_queue.clone());

                    let handle = std::thread::spawn(move || {
                        {
                            let s = state_for_thread.lock().unwrap();
                            pebble_api::set_framebuffer_ptr(
                                s.framebuffer.as_ptr() as *mut u8,
                            );
                        }

                        #[cfg(target_arch = "arm")]
                        {
                            pebble_api::set_button_queue(button_queue);
                            match executor::load_binary(&bin_data, &info) {
                                Ok(entry) => {
                                    if let Err(e) = executor::execute(&entry, stop) {
                                        eprintln!("Pebble app error: {}", e);
                                    }
                                }
                                Err(e) => eprintln!("Failed to load binary: {}", e),
                            }
                        }
                        #[cfg(not(target_arch = "arm"))]
                        {
                            if let Err(e) = emu::load_and_execute_with_buttons(&bin_data, &info, stop, Some(button_queue)) {
                                eprintln!("Pebble app error: {}", e);
                            }
                        }
                    });

                    *pebble_thread.lock().unwrap() = Some(handle);
                }
                Err(e) => {
                    eprintln!("Failed to load PBW: {}", e);
                }
            }
        });
    }

    // ─── Pebble button presses (keyboard → click handlers) ───
    {
        let button_queue_ref = button_queue_ref.clone();
        window.on_button_pressed(move |button| {
            let button = button as u8;
            if let Some(ref bq) = *button_queue_ref.lock().unwrap() {
                bq.lock().unwrap().push(button);
            }
        });
    }

    // ─── Back button ───
    {
        let stop_flag = stop_flag.clone();
        let pebble_thread = pebble_thread.clone();
        let window_weak = window.as_weak();
        let store_state = store_state.clone();
        let pbw_dir = pbw_dir.clone();

        window.on_back_pressed(move || {
            let Some(w) = window_weak.upgrade() else {
                return;
            };

            if w.get_running() {
                // Back from running app
                stop_flag.store(true, Ordering::Relaxed);
                // The next launch reaps the finished worker; do not block UI here.
                w.set_running(false);
            } else if w.get_view() == 5 {
                // Back from preview → return to store list
                let ss = store_state.lock().unwrap();
                let prev_view = ss.prev_view;
                let items = ss
                    .collections
                    .get(&prev_view)
                    .map(|c| c.items.clone())
                    .unwrap_or_default();
                drop(ss);

                w.set_view(prev_view);
                w.set_store_items(make_store_model(&items, &pbw_dir));
            }
        });
    }

    // ─── Framebuffer refresh at 10 FPS ───
    let timer = slint::Timer::default();
    {
        let window_weak = window.as_weak();
        let mut changed = display_frame::ChangedFrame::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                if let Some(w) = window_weak.upgrade() {
                    if w.get_running() {
                        let Some(display_buf) = changed.take(pebble_api::get_display_buffer()) else {
                            return;
                        };
                        let out_size = 416;
                        let mut pixel_buf =
                            SharedPixelBuffer::<slint::Rgba8Pixel>::new(out_size, out_size);
                        render_scaled(
                            &display_buf,
                            runtime::DISPLAY_WIDTH,
                            runtime::DISPLAY_HEIGHT,
                            pixel_buf.make_mut_bytes(),
                            out_size as usize,
                        );

                        w.set_framebuffer(slint::Image::from_rgba8(pixel_buf));
                    }
                }
            },
        );
    }

    window.run().unwrap();

    // Clean up
    stop_flag.store(true, Ordering::Relaxed);
    // Process exit reclaims an uncooperative native worker. Never join it here.
    // SessionCleanup runs normally for cooperative workers.

}

/// Scale Pebble framebuffer (180x180 GColor8) to output (416x416 RGBA) with circular mask
fn render_scaled(src: &[u8], src_w: usize, src_h: usize, dst: &mut [u8], dst_size: usize) {
    let cx = dst_size as f32 / 2.0;
    let cy = cx;
    let r = cx;
    let scale_x = src_w as f32 / dst_size as f32;
    let scale_y = src_h as f32 / dst_size as f32;

    for y in 0..dst_size {
        for x in 0..dst_size {
            let out_idx = (y * dst_size + x) * 4;
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;

            if dx * dx + dy * dy <= r * r {
                let sx = ((x as f32 + 0.5) * scale_x) as usize;
                let sy = ((y as f32 + 0.5) * scale_y) as usize;
                let sx = sx.min(src_w - 1);
                let sy = sy.min(src_h - 1);
                let gc = src[sy * src_w + sx];
                let rgba = gcolor::gcolor8_to_rgba(gc);
                dst[out_idx] = rgba[0];
                dst[out_idx + 1] = rgba[1];
                dst[out_idx + 2] = rgba[2];
                dst[out_idx + 3] = 255;
            } else {
                dst[out_idx] = 0;
                dst[out_idx + 1] = 0;
                dst[out_idx + 2] = 0;
                dst[out_idx + 3] = 0;
            }
        }
    }
}
