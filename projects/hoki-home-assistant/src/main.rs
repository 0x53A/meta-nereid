#[path = "../../shared/sleep_client.rs"]
mod sleep_client;
mod voice;

slint::include_modules!();

use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::Deserialize;
use serde_json::Value;
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    label: String,
    domain: String,
    service: String,
    #[serde(default)]
    data: Value,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    url: String,
    token: String,
    actions: Vec<Action>,
    #[serde(default)]
    voice: Option<VoiceConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceConfig {
    #[serde(default)]
    pipeline: Option<String>,
}

fn config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    Ok(PathBuf::from(home).join(".config/hoki/home-assistant.json"))
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    let metadata = fs::metadata(&path).context("Set up home-assistant.json")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Config must be private (chmod 600)");
        }
    }
    let config: Config =
        serde_json::from_slice(&fs::read(path)?).context("Invalid Home Assistant config")?;
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<()> {
    let url = Url::parse(&config.url).context("Invalid HA URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Use an HTTP(S) HA URL without credentials or query");
    }
    if config.token.trim().is_empty() {
        bail!("HA token is empty");
    }
    if config.actions.len() > 3 || (config.actions.is_empty() && config.voice.is_none()) {
        bail!("Configure one to three actions or voice");
    }
    if config
        .voice
        .as_ref()
        .and_then(|voice| voice.pipeline.as_ref())
        .is_some_and(|pipeline| pipeline.trim().is_empty())
    {
        bail!("Voice pipeline ID is empty");
    }
    for action in &config.actions {
        if action.label.trim().is_empty() || action.label.chars().count() > 24 {
            bail!("Action labels must be 1–24 characters");
        }
        for segment in [&action.domain, &action.service] {
            if segment.is_empty()
                || !segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                bail!("Invalid HA domain or service");
            }
        }
        if !action.data.is_null() && !action.data.is_object() {
            bail!("Action data must be a JSON object");
        }
    }
    Ok(())
}

fn call_action(client: &Client, config: &Config, action: &Action) -> Result<()> {
    let mut url = Url::parse(&config.url)?;
    let base = url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base}/api/services/{}/{}",
        action.domain, action.service
    ));
    let body = if action.data.is_null() {
        &serde_json::json!({})
    } else {
        &action.data
    };
    let response = client
        .post(url)
        .bearer_auth(&config.token)
        .json(body)
        .send()
        .context("Cannot reach Home Assistant")?;
    if !response.status().is_success() {
        bail!("HA returned HTTP {}", response.status().as_u16());
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let preview_voice = args.iter().any(|arg| arg == "--preview-voice");
    let preview_reply = args.iter().any(|arg| arg == "--preview-reply");
    let preview = args.iter().any(|arg| arg == "--preview") || preview_voice || preview_reply;
    let capture = args
        .iter()
        .position(|arg| arg == "--capture")
        .and_then(|index| args.get(index + 1))
        .cloned();
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");
    let app = App::new()?;
    let mut voice_runtime = None;
    let loaded = if preview {
        Ok(Config {
            url: "https://example.invalid".into(),
            token: "preview".into(),
            actions: vec![
                Action {
                    label: "Living room lights".into(),
                    domain: "light".into(),
                    service: "turn_on".into(),
                    data: Value::Null,
                },
                Action {
                    label: "Good night".into(),
                    domain: "script".into(),
                    service: "turn_on".into(),
                    data: Value::Null,
                },
                Action {
                    label: "Very long action label".into(),
                    domain: "scene".into(),
                    service: "turn_on".into(),
                    data: Value::Null,
                },
            ],
            voice: Some(VoiceConfig { pipeline: None }),
        })
    } else {
        load_config()
    };
    match loaded {
        Ok(config) => {
            app.set_voice_enabled(config.voice.is_some());
            let labels = [
                config.actions.first(),
                config.actions.get(1),
                config.actions.get(2),
            ];
            app.set_first_label(labels[0].map_or("", |a| &a.label).into());
            app.set_second_label(labels[1].map_or("", |a| &a.label).into());
            app.set_third_label(labels[2].map_or("", |a| &a.label).into());
            app.set_status("Choose an action".into());
            let config = Arc::new(config);
            if config.voice.is_some() {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let worker_config = (*config).clone();
                let weak = app.as_weak();
                let thread = std::thread::spawn(move || {
                    let runtime = tokio::runtime::Runtime::new().unwrap();
                    runtime.block_on(voice::worker(rx, worker_config, weak));
                });
                let start_tx = tx.clone();
                let weak = app.as_weak();
                app.on_start_voice(move || {
                    if preview {
                        return;
                    }
                    if let Some(app) = weak.upgrade() {
                        app.set_voice_mode(true);
                        app.set_voice_busy(true);
                        app.set_voice_listening(false);
                        app.set_voice_status("Connecting".into());
                        app.set_voice_detail("Opening Home Assistant".into());
                    }
                    let _ = start_tx.send(voice::Control::Start);
                });
                let send_tx = tx.clone();
                let weak = app.as_weak();
                app.on_send_voice(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_voice_listening(false);
                        app.set_voice_status("Thinking".into());
                        app.set_voice_detail("Waiting for Home Assistant".into());
                    }
                    let _ = send_tx.send(voice::Control::Send);
                });
                let cancel_tx = tx.clone();
                let weak = app.as_weak();
                app.on_cancel_voice(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_voice_mode(false);
                    }
                    let _ = cancel_tx.send(voice::Control::Cancel);
                });
                voice_runtime = Some((tx, thread));
            }
            let client = Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(12))
                .redirect(Policy::none())
                .build()?;
            let weak = app.as_weak();
            app.on_trigger(move |index| {
                if preview {
                    return;
                }
                if index < 0 || weak.upgrade().is_none_or(|app| app.get_busy()) {
                    return;
                }
                let Some(action) = config.actions.get(index as usize).cloned() else {
                    return;
                };
                if let Some(app) = weak.upgrade() {
                    app.set_busy(true);
                    app.set_status(format!("Sending {}…", action.label).into());
                }
                let weak = weak.clone();
                let config = Arc::clone(&config);
                let client = client.clone();
                std::thread::spawn(move || {
                    let status = match call_action(&client, &config, &action) {
                        Ok(()) => "Request accepted".to_string(),
                        Err(error) => format!("{error:#}"),
                    };
                    weak.upgrade_in_event_loop(move |app| {
                        app.set_busy(false);
                        app.set_status(status.into());
                    })
                    .ok();
                });
            });
        }
        Err(error) => app.set_status(format!("{error:#}").into()),
    }
    if preview_voice {
        app.set_voice_mode(true);
        app.set_voice_busy(true);
        app.set_voice_listening(true);
        app.set_voice_status("Listening".into());
        app.set_voice_detail("Speak now. Tap Send when finished.".into());
    }
    if preview_reply {
        app.set_voice_mode(true);
        app.set_voice_busy(false);
        app.set_voice_listening(false);
        app.set_voice_status("Reply".into());
        app.set_voice_detail("The living room lights are on, and your evening scene is ready. This long answer checks text wrapping on the round screen.".into());
    }
    let screenshot = slint::Timer::default();
    if let Some(path) = capture {
        let weak = app.as_weak();
        screenshot.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(2),
            move || {
                if let Some(app) = weak.upgrade() {
                    match app.window().take_snapshot() {
                        Ok(pixels) => {
                            if let Err(error) = image::save_buffer(
                                &path,
                                pixels.as_bytes(),
                                pixels.width(),
                                pixels.height(),
                                image::ColorType::Rgba8,
                            ) {
                                eprintln!("Capture failed: {error}");
                            }
                        }
                        Err(error) => eprintln!("Capture failed: {error}"),
                    }
                }
                let _ = slint::quit_event_loop();
            },
        );
    }
    app.run()?;
    if let Some((tx, thread)) = voice_runtime {
        let _ = tx.send(voice::Control::Shutdown);
        let _ = thread.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_action_paths_and_bounds() {
        let mut config = Config {
            url: "https://ha.example.test/".into(),
            token: "secret".into(),
            actions: vec![Action {
                label: "Lights".into(),
                domain: "light".into(),
                service: "turn_on".into(),
                data: serde_json::json!({"entity_id":"light.office"}),
            }],
            voice: None,
        };
        assert!(validate(&config).is_ok());
        config.actions[0].service = "../bad".into();
        assert!(validate(&config).is_err());
    }

    #[test]
    fn sends_service_call_with_bearer_token() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if count == 0 || request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request);
            assert!(headers.starts_with("POST /api/services/light/turn_on HTTP/1.1"));
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer secret"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]")
                .unwrap();
        });
        let config = Config {
            url: format!("http://{address}"),
            token: "secret".into(),
            actions: vec![],
            voice: None,
        };
        let action = Action {
            label: "Lights".into(),
            domain: "light".into(),
            service: "turn_on".into(),
            data: Value::Null,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        assert!(call_action(&client, &config, &action).is_ok());
        server.join().unwrap();
    }
}
