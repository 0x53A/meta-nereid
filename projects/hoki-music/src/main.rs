#[cfg(all(feature = "tls-system", feature = "tls-rustcrypto"))]
compile_error!(
    "Choose one TLS backend: use --no-default-features --features tls-rustcrypto for RustCrypto"
);
#[cfg(not(any(feature = "tls-system", feature = "tls-rustcrypto")))]
compile_error!("Enable tls-system (default) or tls-rustcrypto");

mod decode;
mod model;
mod network;
mod preview;
mod pulse;
mod service;
use model::{Request, Source, State};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{rc::Rc, sync::mpsc, time::Duration};
slint::include_modules!();
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--check-tls") {
        let url = reqwest::Url::parse(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing HTTPS URL"))?,
        )?;
        if url.scheme() != "https" {
            anyhow::bail!("TLS check requires an HTTPS URL");
        }
        let response = network::client()?
            .head(url)
            .send()
            .map_err(|_| anyhow::anyhow!("TLS/connection verification failed"))?;
        let backend = if cfg!(feature = "tls-rustcrypto") {
            "RustCrypto"
        } else {
            "system TLS"
        };
        println!("TLS verified ({backend}): HTTP {}", response.status());
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--daemon") {
        return service::run();
    }
    if args.get(1).map(String::as_str) == Some("--request") {
        let req: Request = serde_json::from_str(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing request JSON"))?,
        )?;
        println!("{}", serde_json::to_string(&service::request(&req)?)?);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--decode") {
        let path = args.get(2).ok_or_else(|| anyhow::anyhow!("Missing file"))?;
        let mut decoder = decode::Decoder::from_source(Box::new(std::fs::File::open(path)?))?;
        let start = std::time::Instant::now();
        let mut samples = 0usize;
        let mut secs = 0.0;
        while let Some(block) = decoder.next()? {
            samples += block.samples.len();
            secs += block.samples.len() as f64 / block.rate as f64 / block.channels as f64;
        }
        println!(
            "Decoded {samples} samples ({secs:.3}s audio) in {:.3}s",
            start.elapsed().as_secs_f64()
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--preview") {
        return preview::render(
            args.get(2).map(String::as_str).unwrap_or("player"),
            args.get(3).map(String::as_str).unwrap_or("preview.ppm"),
        );
    }
    let app = App::new()?;
    let (tx, rx) = mpsc::sync_channel::<Request>(32);
    let volume_target =
        std::sync::Arc::new(std::sync::Mutex::new(None::<(std::time::Instant, u8)>));
    let send = tx.clone();
    app.on_action(move |action| {
        let request = match action.as_str() {
            "previous" => Request::Previous,
            "next" => Request::Next,
            "toggle" => Request::Toggle,
            "stop" => Request::Stop,
            _ => Request::Dismiss,
        };
        let _ = send.try_send(request);
    });
    let send = tx.clone();
    app.on_play_track(move |key| {
        if let Ok(source) = serde_json::from_str(key.as_str()) {
            let _ = send.try_send(Request::Play { source });
        }
    });
    let send = tx.clone();
    let target = volume_target.clone();
    let volume_ui = app.as_weak();
    app.on_volume_change(move |value| {
        let value = value.clamp(0, 100) as u8;
        if send.try_send(Request::Volume { value }).is_ok() {
            *target.lock().unwrap() = Some((std::time::Instant::now(), value));
            if let Some(ui) = volume_ui.upgrade() {
                ui.set_volume(value as i32);
            }
        }
    });
    let send = tx.clone();
    app.on_seek(move |seconds| {
        let _ = send.try_send(Request::Seek {
            seconds: seconds as f64,
        });
    });
    app.on_refresh(move |remote| {
        let _ = tx.try_send(Request::Refresh { remote });
    });
    let weak = app.as_weak();
    app.on_close(move || {
        if let Some(app) = weak.upgrade() {
            let _ = app.hide();
        }
        let _ = slint::quit_event_loop();
    });
    let weak = app.as_weak();
    std::thread::spawn(move || {
        if let Err(e) = service::ensure_running() {
            let message = e.to_string();
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_notice(message.into()));
            return;
        }
        let mut first = true;
        let mut last_revision = None;
        loop {
            let command = if first {
                Request::Library
            } else {
                match rx.recv_timeout(Duration::from_millis(750)) {
                    Ok(r) => r,
                    Err(mpsc::RecvTimeoutError::Timeout) => Request::State,
                    Err(_) => break,
                }
            };
            let library = matches!(command, Request::Library);
            match service::request(&command) {
                Ok(mut state) => {
                    let mut update_library = library;
                    if last_revision != Some(state.library_revision) && !library {
                        if let Ok(s) = service::request(&Request::Library) {
                            state = s;
                            update_library = true;
                        }
                    }
                    last_revision = Some(state.library_revision);
                    let empty = state.library.is_empty();
                    let target = volume_target.clone();
                    if weak
                        .upgrade_in_event_loop(move |ui| {
                            if let Some((time, volume)) = *target.lock().unwrap() {
                                if time.elapsed() < Duration::from_secs(2) {
                                    state.volume = volume;
                                }
                            }
                            apply(&ui, &state, update_library)
                        })
                        .is_err()
                    {
                        break;
                    }
                    if first && empty {
                        let _ = service::request(&Request::Refresh { remote: false });
                    }
                    first = false;
                }
                Err(e) => {
                    let message = e.to_string();
                    if weak
                        .upgrade_in_event_loop(move |ui| ui.set_notice(message.into()))
                        .is_err()
                    {
                        break;
                    }
                    first = false;
                }
            }
        }
    });
    app.run()?;
    Ok(())
}
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
fn apply(app: &App, state: &State, library: bool) {
    app.set_playing(state.playing);
    app.set_busy(state.busy);
    app.set_scanning(state.scanning);
    app.set_volume(state.volume as i32);
    app.set_notice(state.notice.clone().into());
    if library {
        app.set_tracks(ModelRc::from(Rc::new(VecModel::from(
            state
                .library
                .iter()
                .map(|t| TrackRow {
                    key: serde_json::to_string(&t.source).unwrap_or_default().into(),
                    title: t.title.clone().into(),
                    subtitle: format!(
                        "{} · {}",
                        t.artist,
                        match t.source {
                            Source::Local { .. } => "Local",
                            Source::Navidrome { .. } => "Navidrome",
                        }
                    )
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))));
    }
    let current = state.current.and_then(|i| state.queue.get(i));
    app.set_has_track(current.is_some());
    if let Some(track) = current {
        app.set_track_title(display_title(&track.title).into());
        app.set_artist(track.artist.clone().into());
        app.set_source_label(
            match track.source {
                Source::Local { .. } => "LOCAL MUSIC",
                Source::Navidrome { .. } => "NAVIDROME",
            }
            .into(),
        );
        app.set_time_label(format!("{} / {}", clock(state.position), clock(track.duration)).into());
        app.set_progress(if track.duration > 0.0 {
            (state.position / track.duration) as f32
        } else {
            0.0
        });
        app.set_position(state.position as f32);
    } else {
        app.set_track_title("Choose some music".into());
        app.set_artist("Open your library to start".into());
        app.set_source_label("LOCAL + NAVIDROME".into());
        app.set_progress(0.0);
    }
}

fn display_title(title: &str) -> String {
    if title.chars().count() <= 42 {
        title.into()
    } else {
        let shortened: String = title.chars().take(39).collect();
        format!("{}…", shortened.trim_end())
    }
}
