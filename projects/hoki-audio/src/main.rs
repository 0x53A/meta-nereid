mod airplay;
mod command;
mod preview;
mod requests;
mod worker;

slint::include_modules!();

use slint::{ComponentHandle, Model};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
struct AudioDevice {
    name: String,
    description: String,
    volume_percent: i32,
    muted: bool,
    is_default: bool,
    airplay: bool,
}

fn parse_devices(json_str: &str, default_name: &str) -> Vec<AudioDevice> {
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|obj| {
            let name = obj["name"]
                .as_str()
                .filter(|name| !name.is_empty() && !name.contains('\0'))?;
            let description = obj["description"].as_str().unwrap_or(name).to_string();
            let volume_percent = obj["volume"]
                .as_object()
                .and_then(|ch| ch.values().next())
                .and_then(|ch| ch["value_percent"].as_str())
                .and_then(|s| s.trim_end_matches('%').parse::<i32>().ok())
                .unwrap_or(100);
            Some(AudioDevice {
                name: name.into(),
                description,
                volume_percent,
                muted: obj["mute"].as_bool().unwrap_or(false),
                is_default: name == default_name,
                airplay: airplay::is_owned(obj),
            })
        })
        .collect()
}

fn pactl(args: &[String]) -> Result<String, String> {
    let output = command::output(
        Command::new("pactl").args(args),
        std::time::Duration::from_secs(10),
        4 * 1024 * 1024,
    )
    .map_err(|e| {
        eprintln!("pactl {args:?}: {e}");
        "Audio service did not respond".to_string()
    })?;
    if !output.status.success() {
        eprintln!(
            "pactl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err("Audio command failed".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn list_devices(kind: &str) -> Result<Vec<AudioDevice>, String> {
    let default_name = pactl(&[format!("get-default-{kind}")])?;
    let plural = if kind == "sink" { "sinks" } else { "sources" };
    let json = pactl(&["--format=json".into(), "list".into(), plural.into()])?;
    serde_json::from_str::<Vec<serde_json::Value>>(&json)
        .map_err(|_| "Invalid audio device list")?;
    Ok(parse_devices(&json, &default_name))
}
fn set_default(kind: &str, name: &str) -> Result<(), String> {
    pactl(&[format!("set-default-{kind}"), name.into()]).map(|_| ())
}
fn set_volume(kind: &str, name: &str, percent: i32) -> Result<(), String> {
    pactl(&[
        format!("set-{kind}-volume"),
        name.into(),
        format!("{}%", percent.clamp(0, 150)),
    ])
    .map(|_| ())
}
fn toggle_mute(kind: &str, name: &str) -> Result<(), String> {
    pactl(&[format!("set-{kind}-mute"), name.into(), "toggle".into()]).map(|_| ())
}
fn device_item(d: &AudioDevice) -> AudioDeviceItem {
    AudioDeviceItem {
        name: d.name.clone().into(),
        description: d.description.clone().into(),
        volume: d.volume_percent,
        muted: d.muted,
        is_default: d.is_default,
        airplay: d.airplay,
    }
}
fn to_slint_devices(devices: &[AudioDevice]) -> slint::ModelRc<AudioDeviceItem> {
    slint::ModelRc::new(slint::VecModel::from(
        devices.iter().map(device_item).collect::<Vec<_>>(),
    ))
}
fn apply_devices(app: &App, kind: &str, ticket: requests::Ticket, devices: &[AudioDevice]) {
    ticket.apply_if_current(|| {
        if kind == "sink" {
            app.set_sinks(to_slint_devices(devices));
            app.set_sink_count(devices.len() as i32);
        } else {
            app.set_sources(to_slint_devices(devices));
            app.set_source_count(devices.len() as i32);
        }
        if app.get_page() == 1 && (app.get_tab() == 0) == (kind == "sink") {
            let selected = app.get_selected_device();
            let current = devices.iter().find(|d| d.name == selected.name.as_str());
            app.set_device_present(current.is_some());
            if let Some(d) = current {
                app.set_selected_device(device_item(d));
            }
        }
    });
}
fn apply_result(
    app: &App,
    kind: &str,
    ticket: requests::Ticket,
    result: Result<Vec<AudioDevice>, String>,
) {
    match result {
        Ok(devices) => apply_devices(app, kind, ticket, &devices),
        Err(error) => ticket.apply_if_current(|| {
            app.set_notice(error.into());
            if app.get_page() == 1 {
                app.set_device_present(false);
            }
        }),
    }
}
fn queue_update(
    worker: &worker::Worker,
    requests: &requests::Requests,
    weak: slint::Weak<App>,
    kind: &'static str,
    change: impl FnOnce() -> Result<(), String> + Send + 'static,
) {
    let Some(app) = weak.upgrade() else { return };
    if app.get_busy() {
        return;
    }
    app.set_busy(true);
    app.set_notice("".into());
    let ticket = requests.begin();
    worker.submit(move || {
        let changed = change();
        let devices = list_devices(kind);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(app) = weak.upgrade() else { return };
            apply_result(&app, kind, ticket, devices);
            if let Err(error) = changed {
                app.set_notice(error.into());
            }
            app.set_busy(false);
        });
    });
}
fn refresh_all(
    worker: &worker::Worker,
    sinks: &requests::Requests,
    sources: &requests::Requests,
    weak: slint::Weak<App>,
) {
    let Some(app) = weak.upgrade() else { return };
    if app.get_busy() {
        return;
    }
    app.set_busy(true);
    app.set_notice("".into());
    let sink_ticket = sinks.begin();
    let source_ticket = sources.begin();
    worker.submit(move || {
        let sinks = list_devices("sink");
        let sources = list_devices("source");
        let _ = slint::invoke_from_event_loop(move || {
            let Some(app) = weak.upgrade() else { return };
            apply_result(&app, "sink", sink_ticket, sinks);
            apply_result(&app, "source", source_ticket, sources);
            app.set_loading(false);
            app.set_busy(false);
        });
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--preview") {
        preview::render(
            args.get(2).map(String::as_str).unwrap_or("outputs"),
            args.get(3).expect("--preview STATE OUTPUT.ppm"),
        );
        return;
    }
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");
    let app = App::new().unwrap();
    let worker = worker::Worker::new();
    let sink_requests = requests::Requests::default();
    let source_requests = requests::Requests::default();
    let scans = requests::Requests::default();
    let scanner = worker::Worker::new();
    let speakers = Arc::new(Mutex::new(Vec::<airplay::Speaker>::new()));

    refresh_all(&worker, &sink_requests, &source_requests, app.as_weak());
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let sinks = sink_requests.clone();
        let sources = source_requests.clone();
        app.on_refresh(move || refresh_all(&worker, &sinks, &sources, weak.clone()));
    }
    {
        let weak = app.as_weak();
        app.on_open_device(move |name| {
            let Some(app) = weak.upgrade() else { return };
            let devices = if app.get_tab() == 0 {
                app.get_sinks()
            } else {
                app.get_sources()
            };
            if let Some(device) = devices.iter().find(|d| d.name == name) {
                app.set_selected_device(device);
                app.set_device_present(true);
                app.set_notice("".into());
                app.set_page(1);
            }
        });
    }
    {
        let weak = app.as_weak();
        let scans = scans.clone();
        app.on_back(move || {
            let Some(app) = weak.upgrade() else { return };
            if app.get_busy() {
                return;
            }
            scans.begin();
            app.set_scanning(false);
            app.set_page(0);
            app.set_notice("".into());
        });
    }
    {
        let scans = scans.clone();
        app.on_close_app(move || {
            scans.begin();
            let _ = slint::quit_event_loop();
        });
    }
    {
        let weak = app.as_weak();
        let scans = scans.clone();
        app.on_cancel_scan(move || {
            scans.begin();
            if let Some(app) = weak.upgrade() {
                app.set_scanning(false);
                app.set_search_status("Search stopped".into());
            }
        });
    }
    {
        let weak = app.as_weak();
        let scans = scans.clone();
        let scanner = scanner.clone();
        let speakers = speakers.clone();
        app.on_scan_airplay(move || {
            let Some(app) = weak.upgrade() else { return };
            if app.get_busy() || app.get_scanning() || app.get_page() != 2 {
                return;
            }
            let ticket = scans.begin();
            app.set_scanning(true);
            app.set_search_status("Searching your network…".into());
            app.set_notice("".into());
            app.set_speakers(slint::ModelRc::default());
            speakers.lock().unwrap().clear();
            let weak = weak.clone();
            let speakers = speakers.clone();
            scanner.submit(move || {
                let result = airplay::discover(|| !ticket.is_current());
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else { return };
                    ticket.apply_if_current(|| {
                        app.set_scanning(false);
                        match result {
                            Ok(found) => {
                                app.set_search_status(
                                    if found.is_empty() {
                                        "No speakers found. Check Wi-Fi"
                                    } else {
                                        "Choose a speaker"
                                    }
                                    .into(),
                                );
                                app.set_speakers(slint::ModelRc::new(slint::VecModel::from(
                                    found
                                        .iter()
                                        .map(|s| AirPlayItem {
                                            name: s.name.clone().into(),
                                            available: s.unavailable.is_empty(),
                                            detail: if s.unavailable.is_empty() {
                                                "Tap to use".into()
                                            } else {
                                                s.unavailable.clone().into()
                                            },
                                        })
                                        .collect::<Vec<_>>(),
                                )));
                                *speakers.lock().unwrap() = found;
                            }
                            Err(error) => app.set_search_status(error.into()),
                        }
                    });
                });
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let sinks = sink_requests.clone();
        let speakers = speakers.clone();
        app.on_connect_airplay(move |index| {
            let Some(app) = weak.upgrade() else { return };
            if app.get_busy() || app.get_scanning() || app.get_page() != 2 {
                return;
            }
            let Some(speaker) = usize::try_from(index)
                .ok()
                .and_then(|i| speakers.lock().unwrap().get(i).cloned())
            else {
                return;
            };
            if !speaker.unavailable.is_empty() {
                return;
            }
            app.set_busy(true);
            app.set_search_status(format!("Selecting {}…", speaker.name).into());
            let ticket = sinks.begin();
            let weak = weak.clone();
            worker.submit(move || {
                let result = airplay::connect(&mut airplay::Server, &speaker);
                let devices = list_devices("sink");
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else { return };
                    apply_result(&app, "sink", ticket, devices);
                    app.set_busy(false);
                    match result {
                        Ok(()) => {
                            if let Some(device) = app.get_sinks().iter().find(|d| d.airplay) {
                                app.set_selected_device(device);
                                app.set_device_present(true);
                                app.set_page(1);
                                app.set_notice("Output selected. Start playback to listen".into());
                            } else {
                                app.set_search_status("Speaker unavailable. Try again".into());
                            }
                        }
                        Err(error) => {
                            app.set_search_status("Could not select speaker".into());
                            app.set_notice(error.into());
                        }
                    }
                });
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = sink_requests.clone();
        app.on_disconnect_airplay(move || {
            queue_update(&worker, &requests, weak.clone(), "sink", || {
                airplay::disconnect(&mut airplay::Server)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = sink_requests.clone();
        app.on_set_default_sink(move |name| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "sink", move || {
                set_default("sink", &name)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = sink_requests.clone();
        app.on_set_sink_volume(move |name, vol| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "sink", move || {
                set_volume("sink", &name, vol)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = sink_requests.clone();
        app.on_toggle_sink_mute(move |name| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "sink", move || {
                toggle_mute("sink", &name)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = source_requests.clone();
        app.on_set_default_source(move |name| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "source", move || {
                set_default("source", &name)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = source_requests.clone();
        app.on_set_source_volume(move |name, vol| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "source", move || {
                set_volume("source", &name, vol)
            });
        });
    }
    {
        let weak = app.as_weak();
        let worker = worker.clone();
        let requests = source_requests.clone();
        app.on_toggle_source_mute(move |name| {
            let name = name.to_string();
            queue_update(&worker, &requests, weak.clone(), "source", move || {
                toggle_mute("source", &name)
            });
        });
    }
    app.run().unwrap();
    scans.begin(); // Cancel the finite browser on ordinary window close too.
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    use std::rc::Rc;

    struct HeadlessPlatform;
    impl slint::platform::Platform for HeadlessPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    fn device(volume: i32) -> AudioDevice {
        AudioDevice {
            name: "test".into(),
            description: "Test".into(),
            volume_percent: volume,
            muted: false,
            is_default: true,
            airplay: false,
        }
    }

    #[test]
    fn pulse_json_preserves_zero_amplified_volume_and_device_state() {
        let input = serde_json::json!([
            {"index": 7, "name": "speaker", "description": "Watch speaker",
             "mute": true, "volume": {"mono": {"value": 0, "value_percent": "0%"}}},
            {"index": 12, "name": "headset", "description": "Kopfhörer",
             "mute": false, "volume": {"mono": {"value": 98304, "value_percent": "150%"}}},
            {"index": 13, "name": "microphone", "description": null,
             "mute": false, "volume": {"mono": {"value": 32768, "value_percent": "50%"}}}
        ]);
        let devices = parse_devices(&input.to_string(), "headset");
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].volume_percent, 0);
        assert!(devices[0].muted);
        assert!(!devices[0].is_default);
        assert_eq!(devices[1].name, "headset");
        assert_eq!(devices[1].description, "Kopfhörer");
        assert_eq!(devices[1].volume_percent, 150);
        assert!(devices[1].is_default);
        assert!(!devices[1].muted);
        assert_eq!(devices[2].description, "microphone");
        assert_eq!(devices[2].volume_percent, 50);
    }

    #[test]
    fn malformed_names_do_not_create_blank_or_default_controls() {
        let input = serde_json::json!([
            null, 42, true, {}, {"name":null}, {"name":12}, {"name":""},
            {"name":"speaker\u{0}suffix"},
            {"name":"valid-speaker","index":18446744073709551615u64}
        ]);
        let devices = parse_devices(&input.to_string(), "");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "valid-speaker");
        assert!(!devices[0].is_default);
    }

    #[test]
    fn empty_or_unreadable_device_lists_do_not_create_a_default_device() {
        for input in ["[]", "", "[", "{}", "null", "pactl: connection failed"] {
            assert!(parse_devices(input, "speaker").is_empty(), "input: {input}");
        }
        let devices = parse_devices(
            r#"[{"index":1,"name":"speaker","mute":false,"volume":{"mono":{"value_percent":"25%"}}}]"#,
            "missing-device",
        );
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].is_default);
        assert_eq!(devices[0].description, "speaker");
    }

    #[test]
    fn stale_refresh_cannot_replace_new_slider_value_and_readback_is_applied() {
        slint::platform::set_platform(Box::new(HeadlessPlatform)).unwrap();
        let app = App::new().unwrap();
        let sinks = requests::Requests::default();
        let sources = requests::Requests::default();
        let stale = sinks.begin();
        // The background read has finished, but its UI callback has not run.
        let (stale, old_data) = std::thread::spawn(move || (stale, vec![device(25)]))
            .join()
            .unwrap();
        let source = sources.begin();
        let latest = sinks.begin();
        app.set_sinks(to_slint_devices(&[device(80)])); // optimistic slider update
        app.set_sink_count(1);
        apply_devices(&app, "sink", stale, &old_data);
        assert_eq!(app.get_sinks().row_data(0).unwrap().volume, 80);
        // A sink interaction must not discard the independent source result.
        apply_devices(&app, "source", source, &[device(30)]);
        assert_eq!(app.get_sources().row_data(0).unwrap().volume, 30);
        assert_eq!(app.get_source_count(), 1);
        // Readback is authoritative, including server-side clamping/failure.
        apply_devices(&app, "sink", latest, &[device(75)]);
        assert_eq!(app.get_sinks().row_data(0).unwrap().volume, 75);
        assert_eq!(app.get_sink_count(), 1);
    }

    #[test]
    fn round_layout_touch_targets_route_controls_and_gate_unavailable_devices() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{PointerEventButton, WindowEvent};
        use std::cell::RefCell;
        struct TestPlatform(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for TestPlatform {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).unwrap();
        let app = App::new().unwrap();
        app.set_loading(false);
        app.set_page(1);
        app.set_selected_device(device_item(&device(65)));
        app.set_device_present(true);
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let got = calls.clone();
        app.on_set_sink_volume(move |name, volume| {
            got.borrow_mut().push(format!("sink:{name}:{volume}"))
        });
        let got = calls.clone();
        app.on_toggle_sink_mute(move |name| got.borrow_mut().push(format!("mute:{name}")));
        let got = calls.clone();
        app.on_set_source_volume(move |name, volume| {
            got.borrow_mut().push(format!("source:{name}:{volume}"))
        });
        let got = calls.clone();
        app.on_back(move || got.borrow_mut().push("back".into()));
        app.show().unwrap();
        window.set_size(slint::PhysicalSize::new(416, 416));
        let draw = || {
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|r| {
                r.render(&mut vec![slint::Rgb8Pixel::default(); 416 * 416], 416);
            });
        };
        let tap = |x, y| {
            draw();
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };
        tap(104.0, 310.0);
        tap(312.0, 310.0);
        tap(208.0, 246.0);
        tap(148.0, 372.0);
        assert_eq!(
            *calls.borrow(),
            ["sink:test:60", "sink:test:70", "mute:test", "back"]
        );
        app.set_device_present(false);
        tap(104.0, 310.0);
        tap(208.0, 246.0);
        assert_eq!(calls.borrow().len(), 4);
        app.set_device_present(true);
        app.set_tab(1);
        tap(312.0, 310.0);
        assert_eq!(calls.borrow().last().unwrap(), "source:test:70");
        app.set_busy(true);
        tap(104.0, 310.0);
        tap(148.0, 372.0);
        assert_eq!(calls.borrow().len(), 5);
    }
}
