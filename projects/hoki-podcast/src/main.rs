slint::include_modules!();
mod storage;
mod requests;
mod playback;
mod resume;
mod cache;
mod duration;

use rodio::{Decoder, OutputStream, Sink, Source};
use slint::Model;
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

const FEED_URL: &str = "https://audiotwig.dauber.kim/feed/podcast/";
fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/home/ceres".into()))
                .join(".local/share")
        });
    base.join("hoki-podcast")
}

struct Episode {
    title: String,
    duration: String,
    audio_url: String,
    key: String,
}

#[allow(dead_code)]
enum AudioCmd {
    Play(u64, PathBuf, f32, f32),
    Pause,
    Resume,
    Stop,
    SeekRelative(f32),
    GetPos,
}

enum AudioEventKind {
    Position(f32),
    Duration(f32),
    Playing,
    Paused,
    Stopped,
    Error(String),
    Warning(String),
}

struct AudioEvent {
    playback_id: u64,
    kind: AudioEventKind,
}

fn episodes_dir() -> PathBuf {
    let p = data_dir().join("episodes");
    fs::create_dir_all(&p).ok();
    p
}

fn episode_path(key: &str) -> PathBuf {
    cache::episode_path(&episodes_dir(), key)
}

fn state_path() -> PathBuf {
    let p = data_dir();
    fs::create_dir_all(&p).ok();
    p.join("state.json")
}

fn load_state() -> Option<resume::SavedState> {
    let data = fs::read_to_string(state_path()).ok()?;
    resume::decode_state(&data)
}

fn save_state(episode: &playback::PlayingEpisode, position: f32) {
    if let Err(err) = resume::save_state(&state_path(), episode, position) {
        eprintln!("cannot save podcast progress: {err}");
    }
}

fn parse_duration_secs(dur: &str) -> f32 {
    duration::parse_secs(dur)
}

fn format_time(secs: f32) -> String {
    let total = secs as u32;
    let m = total / 60;
    let s = total % 60;
    format!("{m}:{s:02}")
}

fn fetch_feed() -> Result<Vec<Episode>, String> {
    let body = ureq::get(FEED_URL)
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?
        .into_string()
        .map_err(|e| format!("Read error: {e}"))?;

    let channel = rss::Channel::read_from(body.as_bytes())
        .map_err(|e| format!("RSS parse error: {e}"))?;

    let mut episodes: Vec<Episode> = channel
        .items()
        .iter()
        .filter_map(|item| {
            let title = item.title()?.to_string();
            let enc = item.enclosure()?;
            let audio_url = enc.url().to_string();
            let key = cache::episode_key(&audio_url);
            let duration = item
                .itunes_ext()
                .and_then(|ext| ext.duration().map(String::from))
                .unwrap_or_default();
            Some(Episode {
                title,
                duration,
                audio_url,
                key,
            })
        })
        .collect();

    // RSS feeds are newest-first; reverse so episode 1 is first
    episodes.reverse();
    Ok(episodes)
}

fn download_episode(url: &str, path: &PathBuf) -> Result<(), String> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("Download error: {e}"))?;

    let mut reader = resp.into_reader();
    storage::save_download(&mut reader, path).map_err(|e| format!("Write error: {e}"))
}

/// Runs the audio player on a dedicated thread. OutputStream must stay on one thread.
fn spawn_audio_thread() -> (mpsc::Sender<AudioCmd>, mpsc::Receiver<AudioEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AudioCmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<AudioEvent>();

    std::thread::spawn(move || {
        let mut stream: Option<OutputStream> = None;
        let mut sink: Option<Sink> = None;
        let mut duration_secs: f32 = 0.0;
        let mut playback_id = 0;

        loop {
            match cmd_rx.recv() {
                Ok(AudioCmd::Play(id, path, dur, position)) => {
                    playback_id = id;
                    // Stop old playback
                    if let Some(ref s) = sink {
                        s.stop();
                    }
                    sink = None;
                    stream = None;

                    let (new_stream, stream_handle) = match OutputStream::try_default() {
                        Ok(s) => s,
                        Err(e) => {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Error(format!("Audio output: {e}")) }).ok();
                            continue;
                        }
                    };

                    let file = match fs::File::open(&path) {
                        Ok(f) => f,
                        Err(e) => {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Error(format!("File: {e}")) }).ok();
                            continue;
                        }
                    };

                    let source = match Decoder::new(BufReader::new(file)) {
                        Ok(s) => s,
                        Err(e) => {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Error(format!("Decode: {e}")) }).ok();
                            continue;
                        }
                    };

                    let new_sink = match Sink::try_new(&stream_handle) {
                        Ok(s) => s,
                        Err(e) => {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Error(format!("Sink: {e}")) }).ok();
                            continue;
                        }
                    };

                    duration_secs = source.total_duration().map(|value| value.as_secs_f32())
                        .filter(|value| value.is_finite() && *value > 0.0).unwrap_or(dur);
                    new_sink.pause();
                    new_sink.append(source);
                    if position > 0.0 {
                        if let Err(err) = resume::seek(&new_sink, position) {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Warning(format!("Resume unavailable: {err}")) }).ok();
                        }
                    }
                    new_sink.play();
                    sink = Some(new_sink);
                    stream = Some(new_stream);
                    evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Duration(duration_secs) }).ok();
                    evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Playing }).ok();
                }
                Ok(AudioCmd::Pause) => {
                    if let Some(ref s) = sink {
                        s.pause();
                        evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Paused }).ok();
                    }
                }
                Ok(AudioCmd::Resume) => {
                    if let Some(ref s) = sink {
                        s.play();
                        evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Playing }).ok();
                    } else {
                        evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Error(
                            "No audio loaded; select the episode again to retry".into()
                        ) }).ok();
                    }
                }
                Ok(AudioCmd::Stop) => {
                    if let Some(ref s) = sink {
                        s.stop();
                    }
                    sink = None;
                    stream = None;
                    evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Stopped }).ok();
                }
                Ok(AudioCmd::SeekRelative(delta)) => {
                    if let Some(ref s) = sink {
                        let Some(pos) = duration::relative_position(s.get_pos().as_secs_f32(), delta, duration_secs) else {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Warning("Invalid seek position".into()) }).ok();
                            continue;
                        };
                        match resume::seek(s, pos) {
                            Ok(()) => {
                                evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Position(s.get_pos().as_secs_f32()) }).ok();
                            }
                            Err(err) => {
                                evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Warning(format!("Seek failed: {err}")) }).ok();
                            }
                        }
                    }
                }
                Ok(AudioCmd::GetPos) => {
                    if let Some(ref s) = sink {
                        let pos = s.get_pos().as_secs_f32();
                        let is_playing = !s.is_paused() && !s.empty();
                        evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Position(pos) }).ok();
                        if !is_playing && !s.is_paused() {
                            evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Stopped }).ok();
                        }
                    }
                }
                Err(_) => break, // channel closed
            }
            // Keep stream alive
            let _ = &stream;
        }
    });

    (cmd_tx, evt_rx)
}

#[cfg(test)]
mod audio_worker_tests {
    use super::*;

    #[test]
    fn resume_without_loaded_audio_reports_failure_instead_of_staying_silent() {
        // No Play command: this exercises the same empty-sink state as a failed
        // load without opening an audio device or relying on host configuration.
        let (commands, events) = spawn_audio_thread();
        for _ in 0..2 {
            commands.send(AudioCmd::Resume).unwrap();
            let event = events.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            assert_eq!(event.playback_id, 0);
            match event.kind {
                AudioEventKind::Error(message) => assert!(message.contains("select the episode again")),
                _ => panic!("resume without audio must report an error"),
            }
        }
    }
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    let (audio_tx, audio_rx) = spawn_audio_thread();
    let audio_tx = Arc::new(Mutex::new(audio_tx));

    // Duration tracked on UI side
    let current_duration = Arc::new(Mutex::new(0.0f32));
    let playback = Arc::new(Mutex::new(playback::Playback::default()));

    // Shared episode list (from feed)
    let feed_episodes: Arc<Mutex<Vec<Episode>>> = Arc::new(Mutex::new(Vec::new()));
    let requests = requests::Requests::default();

    // --- Fetch feed on startup ---
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let request = requests.begin();
        std::thread::spawn(move || {
            let result = fetch_feed();
            let _ = slint::invoke_from_event_loop(move || request.apply_if_current(|| {
                let Some(app) = weak.upgrade() else { return; };
                match result {
                    Ok(eps) => {
                        let slint_eps: Vec<EpisodeData> = eps
                            .iter()
                            .enumerate()
                            .map(|(i, ep)| EpisodeData {
                                title: ep.title.clone().into(),
                                duration: ep.duration.clone().into(),
                                index: i as i32,
                                downloaded: episode_path(&ep.key).exists(),
                            })
                            .collect();
                        let model = std::rc::Rc::new(slint::VecModel::from(slint_eps));
                        app.set_episodes(model.into());
                        app.set_status_text(format!("{} episodes", eps.len()).into());
                        app.set_loading(false);

                        if let Some(state) = load_state() {
                            let keys: Vec<_> = eps.iter().map(|ep| ep.key.as_str()).collect();
                            if let Some(idx) = state.selected_index(&keys) {
                                app.set_selected_index(idx as i32);
                            }
                        }

                        *feed_eps.lock().unwrap() = eps;
                    }
                    Err(e) => {
                        app.set_status_text(format!("Error: {e}").into());
                        app.set_loading(false);
                    }
                }
            }));
        });
    }

    // --- Refresh feed ---
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let requests = requests.clone();
        let playback = playback.clone();
        app.on_refresh_feed(move || {
            let request = requests.begin();
            let weak = weak.clone();
            let feed_eps = feed_eps.clone();
            let playback = playback.clone();
            weak.unwrap().set_loading(true);
            weak.unwrap().set_status_text("Refreshing...".into());

            std::thread::spawn(move || {
                let result = fetch_feed();
                let _ = slint::invoke_from_event_loop(move || request.apply_if_current(|| {
                    let Some(app) = weak.upgrade() else { return; };
                    match result {
                        Ok(eps) => {
                            let slint_eps: Vec<EpisodeData> = eps
                                .iter()
                                .enumerate()
                                .map(|(i, ep)| EpisodeData {
                                    title: ep.title.clone().into(),
                                    duration: ep.duration.clone().into(),
                                    index: i as i32,
                                    downloaded: episode_path(&ep.key).exists(),
                                })
                                .collect();
                            let model = std::rc::Rc::new(slint::VecModel::from(slint_eps));
                            app.set_episodes(model.into());
                            app.set_status_text(format!("{} episodes", eps.len()).into());
                            app.set_loading(false);
                            let keys: Vec<_> = eps.iter().map(|ep| ep.key.as_str()).collect();
                            let selected = if let Some(current) = playback.lock().unwrap().current_episode() {
                                keys.iter().position(|key| *key == current.key)
                            } else {
                                load_state().and_then(|state| state.selected_index(&keys))
                            };
                            app.set_selected_index(selected.map(|index| index as i32).unwrap_or(-1));
                            *feed_eps.lock().unwrap() = eps;
                        }
                        Err(e) => {
                            app.set_status_text(format!("Error: {e}").into());
                            app.set_loading(false);
                        }
                    }
                }));
            });
        });
    }

    // --- Select episode (download if needed, then play) ---
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let audio_tx = audio_tx.clone();
        let current_duration = current_duration.clone();
        let requests = requests.clone();
        let playback = playback.clone();
        app.on_select_episode(move |index| {
            let idx = index as usize;
            let weak = weak.clone();
            let feed_eps = feed_eps.clone();
            let audio_tx = audio_tx.clone();
            let current_duration = current_duration.clone();
            let playback = playback.clone();

            let eps = feed_eps.lock().unwrap();
            if idx >= eps.len() {
                return;
            }
            let url = eps[idx].audio_url.clone();
            let key = eps[idx].key.clone();
            let title = eps[idx].title.clone();
            let dur_str = eps[idx].duration.clone();
            drop(eps);

            let request = requests.begin();
            let path = episode_path(&key);
            let position = resume::position_for(load_state().as_ref(), &key);

            let app = weak.unwrap();
            app.set_selected_index(index);

            if path.exists() {
                app.set_loading(false);
                let dur = parse_duration_secs(&dur_str);
                *current_duration.lock().unwrap() = dur;
                let playback_id = playback.lock().unwrap().begin(idx, &key);
                audio_tx.lock().unwrap().send(AudioCmd::Play(playback_id, path, dur, position)).ok();
                app.set_now_playing_title(title.into());
                app.set_view("player".into());
                app.set_playing(true);
                app.set_progress(if dur > 0.0 { (position / dur).clamp(0.0, 1.0) } else { 0.0 });
                app.set_progress_text(format!("{} / {}", format_time(position), format_time(dur)).into());
                app.set_status_text("".into());
            } else {
                app.set_loading(true);
                app.set_status_text("Downloading...".into());

                std::thread::spawn(move || {
                    let result = download_episode(&url, &path);
                    let _ = slint::invoke_from_event_loop(move || request.apply_if_current(|| {
                        let Some(app) = weak.upgrade() else { return; };
                        app.set_loading(false);
                        match result {
                            Ok(()) => {
                                // Update downloaded flag
                                let eps_model = app.get_episodes();
                                if let Some(model) = eps_model
                                    .as_any()
                                    .downcast_ref::<slint::VecModel<EpisodeData>>()
                                {
                                    if let Some(mut ep) = model.row_data(idx) {
                                        ep.downloaded = true;
                                        model.set_row_data(idx, ep);
                                    }
                                }

                                let dur = parse_duration_secs(&dur_str);
                                *current_duration.lock().unwrap() = dur;
                                let playback_id = playback.lock().unwrap().begin(idx, &key);
                                audio_tx.lock().unwrap().send(AudioCmd::Play(playback_id, path, dur, position)).ok();
                                app.set_now_playing_title(title.into());
                                app.set_view("player".into());
                                app.set_playing(true);
                                app.set_progress(if dur > 0.0 { (position / dur).clamp(0.0, 1.0) } else { 0.0 });
                                app.set_progress_text(
                                    format!("{} / {}", format_time(position), format_time(dur)).into(),
                                );
                                app.set_status_text("".into());
                            }
                            Err(e) => {
                                app.set_status_text(format!("Download failed: {e}").into());
                            }
                        }
                    }));
                });
            }
        });
    }

    // --- Play/Pause ---
    {
        let weak = app.as_weak();
        let audio_tx = audio_tx.clone();
        app.on_play_pause(move || {
            let app = weak.unwrap();
            let tx = audio_tx.lock().unwrap();
            if app.get_playing() {
                tx.send(AudioCmd::Pause).ok();
                app.set_playing(false);
            } else {
                tx.send(AudioCmd::Resume).ok();
                app.set_playing(true);
            }
        });
    }

    // Relative seeks use the worker's actual audio position, including when
    // feed metadata has no duration and the UI progress fraction is unknown.
    {
        let audio_tx = audio_tx.clone();
        app.on_seek_back(move || {
            audio_tx.lock().unwrap().send(AudioCmd::SeekRelative(-15.0)).ok();
        });
    }
    {
        let audio_tx = audio_tx.clone();
        app.on_seek_forward(move || {
            audio_tx.lock().unwrap().send(AudioCmd::SeekRelative(15.0)).ok();
        });
    }

    // --- Show list ---
    {
        let weak = app.as_weak();
        app.on_show_list(move || {
            weak.unwrap().set_view("list".into());
        });
    }

    // --- Progress update timer (polls audio thread) ---
    {
        let weak = app.as_weak();
        let audio_tx = audio_tx.clone();
        let current_duration = current_duration.clone();
        let playback = playback.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(1),
            move || {
                let app = weak.unwrap();
                if app.get_playing() {
                    // Keep progress while browsing the list as audio continues.
                    audio_tx.lock().unwrap().send(AudioCmd::GetPos).ok();
                }

                // Drain even while paused, so queued errors are not stranded.
                while let Ok(evt) = audio_rx.try_recv() {
                    let Some(episode) = playback.lock().unwrap().episode_for(evt.playback_id) else {
                        continue;
                    };
                    match evt.kind {
                        AudioEventKind::Position(pos) => {
                            let dur = *current_duration.lock().unwrap();
                            app.set_progress(if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) } else { 0.0 });
                            app.set_progress_text(
                                format!("{} / {}", format_time(pos), format_time(dur)).into(),
                            );
                            save_state(&episode, pos);
                        }
                        AudioEventKind::Duration(seconds) => {
                            *current_duration.lock().unwrap() = seconds;
                        }
                        AudioEventKind::Stopped => {
                            app.set_playing(false);
                        }
                        AudioEventKind::Error(e) => {
                            app.set_status_text(format!("Error: {e}").into());
                            app.set_playing(false);
                        }
                        AudioEventKind::Warning(message) => {
                            app.set_status_text(message.into());
                        }
                        _ => {}
                    }
                }
            },
        );
        std::mem::forget(timer);
    }

    app.run().unwrap();
}
