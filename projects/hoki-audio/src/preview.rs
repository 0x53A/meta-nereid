//! Reproducible software-renderer captures. Never contacts PulseAudio or Avahi.
use crate::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::Rgb8Pixel;
use std::{io::Write, rc::Rc};

struct Platform(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for Platform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
pub fn render(state: &str, path: &str) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Platform(window.clone()))).unwrap();
    let app = App::new().unwrap();
    app.set_loading(false);
    let devices = vec![
        AudioDevice {
            name: "speaker".into(),
            description: "Watch speaker".into(),
            volume_percent: 65,
            muted: false,
            is_default: true,
            airplay: false,
        },
        AudioDevice {
            name: "headphones".into(),
            description: "Headphones".into(),
            volume_percent: 40,
            muted: false,
            is_default: false,
            airplay: false,
        },
        AudioDevice {
            name: "usb".into(),
            description: "USB audio".into(),
            volume_percent: 50,
            muted: false,
            is_default: false,
            airplay: false,
        },
    ];
    app.set_sinks(to_slint_devices(&devices));
    app.set_sink_count(3);
    app.set_sources(to_slint_devices(&[AudioDevice {
        name: "mic".into(),
        description: "Watch microphone".into(),
        volume_percent: 80,
        muted: false,
        is_default: true,
        airplay: false,
    }]));
    app.set_source_count(1);
    app.set_selected_device(device_item(&devices[0]));
    app.set_device_present(true);
    match state {
        "inputs" => app.set_tab(1),
        "detail" | "muted" | "long" | "missing" | "busy" | "airplay-selected" => {
            app.set_page(1);
            let mut d = app.get_selected_device();
            if state == "muted" {
                d.muted = true;
            }
            if state == "long" {
                d.description = "Living room speaker with an unusually long name".into();
                d.is_default = false;
            }
            if state == "missing" {
                app.set_device_present(false);
            }
            if state == "busy" {
                app.set_busy(true);
            }
            if state == "airplay-selected" {
                d.description = "AirPlay: Kitchen".into();
                d.airplay = true;
            }
            app.set_selected_device(d);
        }
        "airplay" | "searching" | "search-empty" | "search-error" => {
            app.set_page(2);
            if state == "airplay" {
                app.set_search_status("Choose a speaker".into());
                app.set_speakers(slint::ModelRc::new(slint::VecModel::from(vec![
                    AirPlayItem {
                        name: "Kitchen".into(),
                        detail: "Tap to use".into(),
                        available: true,
                    },
                    AirPlayItem {
                        name: "Living room with a long name".into(),
                        detail: "Password required".into(),
                        available: false,
                    },
                    AirPlayItem {
                        name: "Office".into(),
                        detail: "Tap to use".into(),
                        available: true,
                    },
                ])));
            } else if state == "searching" {
                app.set_scanning(true);
                app.set_search_status("Searching your network…".into());
            } else if state == "search-error" {
                app.set_search_status("AirPlay discovery is not installed".into());
            } else {
                app.set_search_status("No speakers found. Check Wi-Fi".into());
            }
        }
        "empty" => {
            app.set_sinks(slint::ModelRc::default());
            app.set_sink_count(0);
        }
        "error" => app.set_notice("Audio service did not respond".into()),
        "loading" => app.set_loading(true),
        "outputs" => (),
        _ => panic!("Unknown preview state: {state}"),
    }
    app.show().unwrap();
    window.set_size(slint::PhysicalSize::new(416, 416));
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![Rgb8Pixel::default(); 416 * 416];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 416);
    });
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(b"P6\n416 416\n255\n").unwrap();
    for (i, p) in pixels.iter().enumerate() {
        // The screenshot represents the physical round panel.
        let x = (i % 416) as f32 - 207.5;
        let y = (i / 416) as f32 - 207.5;
        let rgb = if x * x + y * y > 208.0 * 208.0 {
            [0, 0, 0]
        } else {
            [p.r, p.g, p.b]
        };
        file.write_all(&rgb).unwrap();
    }
}
