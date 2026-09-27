#[path = "../../shared/sleep_client.rs"]
mod sleep_client;
slint::include_modules!();
mod storage;
mod requests;
mod playback;
mod resume;
mod cache;
mod duration;
mod downloads;
mod crown;
#[cfg(test)]
mod ui_tests;

use rodio::{Decoder, OutputStream, Sink, Source};
use slint::Model;
use std::fs;
use std::io::BufReader;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

const FEEDS: [(&str, &str); 2] = [
    ("TWIG", "https://audiotwig.dauber.kim/feed/podcast/"),
    ("PALE", "https://anchor.fm/s/f09cf90/podcast/rss"),
];
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
    feed_index: usize,
    title: String,
    duration: String,
    audio_url: String,
    key: String,
}

struct Library {
    episodes: Vec<Episode>,
    errors: Vec<Option<String>>,
}

impl Default for Library {
    fn default() -> Self {
        Self { episodes: Vec::new(), errors: vec![None; FEEDS.len()] }
    }
}

fn show_feed(app: &App, library: &Library, queue: &downloads::Queue, current_key: Option<&str>) {
    let feed_index = app.get_selected_feed() as usize;
    let rows: Vec<_> = library.episodes.iter().enumerate()
        .filter(|(_, episode)| episode.feed_index == feed_index)
        .map(|(index, episode)| episode_row(index, episode, queue))
        .collect();
    let count = rows.len();
    app.set_episodes(std::rc::Rc::new(slint::VecModel::from(rows)).into());
    let selected = current_key
        .and_then(|key| library.episodes.iter().enumerate()
            .find(|(_, episode)| episode.feed_index == feed_index && episode.key == key)
            .map(|(index, _)| index))
        .or_else(|| load_state().and_then(|state| {
            let keys: Vec<_> = library.episodes.iter().map(|episode| episode.key.as_str()).collect();
            state.selected_index(&keys).filter(|index| library.episodes[*index].feed_index == feed_index)
        }));
    app.set_selected_index(selected.map_or(-1, |index| index as i32));
    app.set_status_text(if let Some(error) = &library.errors[feed_index] {
        if count == 0 { "Feed unavailable · tap refresh icon".into() }
        else { format!("{count} episodes · refresh failed: {error}") }
    } else { format!("{count} episodes") }.into());
    update_download_views(app, queue, &library.episodes, None);
}

fn show_subscriptions(app: &App, library: &Library) {
    let rows: Vec<_> = FEEDS.iter().enumerate().map(|(index, (name, _))| {
        let count = library.episodes.iter().filter(|episode| episode.feed_index == index).count();
        let detail = if library.errors[index].is_some() {
            if count == 0 { "FEED UNAVAILABLE".into() } else { format!("{count} EPISODES · REFRESH FAILED") }
        } else { format!("{count} EPISODES") };
        EpisodeData { title: (*name).into(), detail: detail.into(), duration: "".into(),
            index: index as i32, downloaded: false, progress: 0.0 }
    }).collect();
    app.set_subscriptions(std::rc::Rc::new(slint::VecModel::from(rows)).into());
}

fn apply_feed_result(library: &mut Library, feed_index: usize, result: Result<Vec<Episode>, String>) {
    match result {
        Ok(episodes) => {
            library.episodes.retain(|episode| episode.feed_index != feed_index);
            library.episodes.extend(episodes);
            library.errors[feed_index] = None;
        }
        Err(error) => {
            eprintln!("podcast feed {}: {error}", FEEDS[feed_index].0);
            library.errors[feed_index] = Some(error);
        }
    }
    library.episodes.sort_by_key(|episode| episode.feed_index);
}

fn apply_feed_results(library: &mut Library, results: Vec<Result<Vec<Episode>, String>>) {
    for (feed_index, result) in results.into_iter().enumerate() {
        apply_feed_result(library, feed_index, result);
    }
}

fn episode_row(index: usize, episode: &Episode, queue: &downloads::Queue) -> EpisodeData {
    let downloaded = episode_path(&episode.key).exists();
    let state = queue.state(&episode.key);
    let status = if downloaded {
        "READY".to_string()
    } else {
        state.map_or_else(|| "TAP TO DOWNLOAD".to_string(), downloads::State::label)
    };
    EpisodeData {
        title: episode.title.clone().into(),
        detail: if episode.duration.is_empty() { status } else { format!("{status}  ·  {}", episode.duration) }.into(),
        duration: episode.duration.clone().into(),
        index: index as i32,
        downloaded,
        progress: state.map_or(0.0, downloads::State::progress),
    }
}

fn update_download_views(app: &App, queue: &downloads::Queue, feed: &[Episode], changed_key: Option<&str>) {
    let episodes = app.get_episodes();
    if let Some(model) = episodes.as_any().downcast_ref::<slint::VecModel<EpisodeData>>() {
        let selected_feed = app.get_selected_feed() as usize;
        for (row_index, (index, episode)) in feed.iter().enumerate()
            .filter(|(_, episode)| episode.feed_index == selected_feed).enumerate() {
            if changed_key.is_none_or(|key| key == episode.key) && row_index < model.row_count() {
                model.set_row_data(row_index, episode_row(index, episode, queue));
            }
        }
    }
    let rows: Vec<_> = queue.entries().iter().map(|entry| {
        let index = feed.iter().position(|episode| episode.key == entry.key);
        let downloaded = index.is_some_and(|index| episode_path(&feed[index].key).exists());
        EpisodeData {
            title: entry.title.clone().into(),
            detail: format!("{} · {}", index.map_or("UNKNOWN", |index| FEEDS[feed[index].feed_index].0),
                if downloaded { "READY · TAP TO PLAY".to_string() } else { entry.state.label() }).into(),
            duration: entry.duration.clone().into(),
            index: index.map_or(-1, |index| index as i32),
            downloaded,
            progress: entry.state.progress(),
        }
    }).collect();
    let existing = app.get_downloads();
    if let Some(model) = existing.as_any().downcast_ref::<slint::VecModel<EpisodeData>>() {
        if model.row_count() == rows.len() {
            for (index, row) in rows.into_iter().enumerate() {
                if changed_key.is_none_or(|key| key == queue.entries()[index].key) {
                    model.set_row_data(index, row);
                }
            }
        } else {
            app.set_downloads(std::rc::Rc::new(slint::VecModel::from(rows)).into());
        }
    } else {
        app.set_downloads(std::rc::Rc::new(slint::VecModel::from(rows)).into());
    }
    app.set_queue_count(queue.active_count() as i32);
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

fn install_crown_scroll(app: &App) {
    let crown = std::rc::Rc::new(std::cell::RefCell::new(crown::CrownScroll::default()));
    let weak = app.as_weak();
    app.on_crown_scroll(move |delta| {
        if let Some(app) = weak.upgrade() {
            let rows = crown.borrow_mut().step(delta, &app.get_view());
            if rows != 0 {
                app.invoke_scroll_rows(rows);
            }
        }
    });
}

// Synthetic states for reviewing the actual Slint renderer without contacting
// the podcast feed or opening an audio device.
fn run_preview(name: &str, capture: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let app = App::new()?;
    install_crown_scroll(&app);
    let subscription_count = if name == "many-shows" { 100 } else { 2 };
    let subscriptions: Vec<_> = (0..subscription_count).map(|index| EpisodeData {
        title: if index < FEEDS.len() { FEEDS[index].0.to_string() } else { format!("Podcast {:03}", index + 1) }.into(),
        detail: (if index == 0 { "7 EPISODES" } else if index == 1 { "161 EPISODES" } else { "25 EPISODES" }).into(),
        duration: "".into(), index: index as i32, downloaded: false, progress: 0.0,
    }).collect();
    app.set_subscriptions(std::rc::Rc::new(slint::VecModel::from(subscriptions)).into());
    if name != "shows" && name != "many-shows" { app.set_view("list".into()); }
    if name == "pale" {
        app.set_selected_feed(1);
        app.set_feed_title("PALE".into());
    }
    let titles = if name == "pale" { [
        "Pale 11.3 Dash to Pieces: Avery",
        "Pale 11.2 Dash to Pieces: Lucy",
        "Pale 11.1 Dash to Pieces: Verona",
        "Pale 10.12 Summer Break",
        "Pale 10.11 Summer Break",
        "Pale 10.10 Summer Break",
        "Pale 10.9 Summer Break",
    ] } else { [
        "A Short Introduction",
        "How We Built the Observatory on a Very Small Island",
        "Night Walks and Other Stories",
        "The Interview: Listening to the City After Dark",
        "A New Beginning",
        "The Long Way Home",
        "Finding Patterns in the Static",
    ] };
    let episodes: Vec<_> = titles.iter().enumerate().map(|(index, title)| EpisodeData {
        title: (*title).into(),
        detail: (if index < 2 { "READY  ·  42:18" } else { "TAP TO DOWNLOAD  ·  42:18" }).into(),
        duration: "42:18".into(),
        index: index as i32,
        downloaded: index < 2,
        progress: 0.0,
    }).collect();
    if name != "empty" && name != "loading" {
        app.set_episodes(std::rc::Rc::new(slint::VecModel::from(episodes)).into());
        app.set_status_text(if name == "pale" { "161 EPISODES" } else { "7 EPISODES" }.into());
    } else if name == "empty" {
        app.set_status_text("Feed unavailable".into());
    } else {
        app.set_loading(true);
        app.set_status_text("Loading feed…".into());
    }
    if name == "player" || name == "long" {
        app.set_view("player".into());
        app.set_now_playing_title(if name == "long" { titles[1] } else { titles[0] }.into());
        app.set_progress_text("12:34 / 42:18".into());
        app.set_progress(0.3);
        app.set_playing(true);
        app.set_status_text("".into());
    } else if name == "list-playing" {
        app.set_now_playing_title(titles[1].into());
        app.set_selected_index(1);
        app.set_playing(true);
    } else if name == "queue" {
        app.set_view("queue".into());
        app.set_queue_count(2);
        app.set_downloads(std::rc::Rc::new(slint::VecModel::from(vec![
            EpisodeData { title: titles[2].into(), detail: "DOWNLOADING 42%".into(), duration: "42:18".into(), index: 2, downloaded: false, progress: 0.42 },
            EpisodeData { title: titles[3].into(), detail: "QUEUED".into(), duration: "42:18".into(), index: 3, downloaded: false, progress: 0.0 },
        ])).into());
    } else if name == "list-queue" {
        app.set_queue_count(2);
        let episodes = app.get_episodes();
        if let Some(model) = episodes.as_any().downcast_ref::<slint::VecModel<EpisodeData>>() {
            if let Some(mut row) = model.row_data(2) {
                row.detail = "DOWNLOADING 42%  ·  42:18".into();
                row.progress = 0.42;
                model.set_row_data(2, row);
            }
        }
    }
    let timer = slint::Timer::default();
    if let Some(path) = capture {
        let weak = app.as_weak();
        timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(500), move || {
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let app = weak.upgrade().ok_or("Window closed")?;
                let pixels = app.window().take_snapshot()?;
                let mut file = fs::File::create(&path)?;
                write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                for pixel in pixels.as_bytes().chunks_exact(4) {
                    file.write_all(&pixel[..3])?;
                }
                Ok(())
            })();
            if let Err(error) = result { eprintln!("Preview capture failed: {error}"); }
            let _ = slint::quit_event_loop();
        });
    }
    app.run()?;
    Ok(())
}

fn fetch_feed(feed_index: usize) -> Result<Vec<Episode>, String> {
    let body = ureq::get(FEEDS[feed_index].1)
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?
        .into_string()
        .map_err(|e| format!("Read error: {e}"))?;

    let channel = rss::Channel::read_from(body.as_bytes())
        .map_err(|e| format!("RSS parse error: {e}"))?;

    let episodes: Vec<Episode> = channel
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
                feed_index,
                title,
                duration,
                audio_url,
                key,
            })
        })
        .collect();

    // Keep the feed's newest-first order so recent episodes are reachable
    // without scrolling through the entire archive.
    Ok(episodes)
}

fn fetch_all_feeds() -> Vec<Result<Vec<Episode>, String>> {
    (0..FEEDS.len()).map(fetch_feed).collect()
}

#[cfg(test)]
mod library_tests {
    use super::*;

    fn episode(feed_index: usize, key: &str) -> Episode {
        Episode { feed_index, title: key.into(), duration: "".into(),
            audio_url: format!("https://example.invalid/{key}"), key: key.into() }
    }

    #[test]
    fn refreshing_one_subscription_keeps_the_other_and_its_download_identity() {
        let mut library = Library::default();
        apply_feed_result(&mut library, 0, Ok(vec![episode(0, "twig-a")]));
        apply_feed_result(&mut library, 1, Ok(vec![episode(1, "pale-a")]));
        apply_feed_result(&mut library, 1, Err("offline".into()));
        assert_eq!(library.episodes.iter().map(|episode| episode.key.as_str()).collect::<Vec<_>>(),
            ["twig-a", "pale-a"]);
        assert!(library.errors[1].is_some());
        apply_feed_result(&mut library, 0, Ok(vec![episode(0, "twig-b")]));
        assert_eq!(library.episodes.iter().map(|episode| episode.key.as_str()).collect::<Vec<_>>(),
            ["twig-b", "pale-a"]);
    }
}

fn playback_inhibitor() -> std::io::Result<sleep_client::Client> {
    let mut guard=sleep_client::Client::connect()?;
    guard.inhibit(true,false,"podcast playback")?;
    Ok(guard)
}

/// Runs the audio player on a dedicated thread. OutputStream must stay on one thread.
fn spawn_audio_thread(desktop_mode: bool) -> (mpsc::Sender<AudioCmd>, mpsc::Receiver<AudioEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AudioCmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<AudioEvent>();

    std::thread::spawn(move || {
        let mut stream: Option<OutputStream> = None;
        let mut sink: Option<Sink> = None;
        let mut duration_secs: f32 = 0.0;
        let mut playback_id = 0;
        let mut awake: Option<sleep_client::Client> = None;

        loop {
            if sink.as_ref().is_some_and(|s|s.empty()) {awake=None;}
            if let Some(guard)=awake.as_mut() {
                if let Err(error)=guard.request(serde_json::json!({"command":"status"})) {
                    if let Some(s)=sink.as_ref(){s.stop();}
                    awake=None;
                    evt_tx.send(AudioEvent {playback_id,kind:AudioEventKind::Error(format!("Sleep coordinator: {error}"))}).ok();
                }
            }
            match cmd_rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(AudioCmd::Play(id, path, dur, position)) => {
                    playback_id = id;
                    // Stop old playback
                    if let Some(ref s) = sink {
                        s.stop();
                    }
                    sink = None;
                    stream = None;
                    awake = None;

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
                    if !desktop_mode {
                        match playback_inhibitor() {
                            Ok(guard)=>awake=Some(guard),
                            Err(error)=>{
                                evt_tx.send(AudioEvent {playback_id,kind:AudioEventKind::Error(format!("Sleep coordinator: {error}"))}).ok();
                                continue;
                            }
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
                        awake=None;
                        evt_tx.send(AudioEvent { playback_id, kind: AudioEventKind::Paused }).ok();
                    }
                }
                Ok(AudioCmd::Resume) => {
                    if let Some(ref s) = sink {
                        if !desktop_mode && awake.is_none() {
                            match playback_inhibitor() {
                                Ok(guard)=>awake=Some(guard),
                                Err(error)=>{
                                    evt_tx.send(AudioEvent {playback_id,kind:AudioEventKind::Error(format!("Sleep coordinator: {error}"))}).ok();
                                    continue;
                                }
                            }
                        }
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
                    awake = None;
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
                Err(mpsc::RecvTimeoutError::Timeout)=>continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // channel closed
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
        let (commands, events) = spawn_audio_thread(true);
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
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--preview") {
        let name = args.get(index + 1).and_then(|arg| arg.to_str()).unwrap_or("list");
        let capture = args.iter().position(|arg| arg == "--capture")
            .and_then(|index| args.get(index + 1)).map(PathBuf::from);
        if let Err(error) = run_preview(name, capture) { eprintln!("Preview failed: {error}"); }
        return;
    }
    let desktop_mode = args.iter().any(|arg| arg == "--windowed");
    if desktop_mode {
        std::env::remove_var("SLINT_FULLSCREEN");
    } else {
        std::env::set_var("SLINT_FULLSCREEN", "1");
    }
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();
    install_crown_scroll(&app);

    let (audio_tx, audio_rx) = spawn_audio_thread(desktop_mode && !cfg!(target_arch = "arm"));
    let audio_tx = Arc::new(Mutex::new(audio_tx));

    // Duration tracked on UI side
    let current_duration = Arc::new(Mutex::new(0.0f32));
    let playback = Arc::new(Mutex::new(playback::Playback::default()));

    // All subscriptions share the audio cache and download queue.
    let feed_episodes: Arc<Mutex<Library>> = Arc::new(Mutex::new(Library::default()));
    let downloads = Arc::new(Mutex::new(downloads::Queue::start()));
    let requests = requests::Requests::default();
    app.set_loading(true);
    show_subscriptions(&app, &feed_episodes.lock().unwrap());

    // --- Fetch feed on startup ---
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let downloads = downloads.clone();
        let request = requests.begin();
        std::thread::spawn(move || {
            let results = fetch_all_feeds();
            let _ = slint::invoke_from_event_loop(move || request.apply_if_current(|| {
                let Some(app) = weak.upgrade() else { return; };
                let mut library = feed_eps.lock().unwrap();
                apply_feed_results(&mut library, results);
                let queue = downloads.lock().unwrap();
                show_subscriptions(&app, &library);
                show_feed(&app, &library, &queue, None);
                app.set_loading(false);
            }));
        });
    }

    // --- Refresh feed ---
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let downloads = downloads.clone();
        let requests = requests.clone();
        let playback = playback.clone();
        app.on_refresh_feed(move || {
            let request = requests.begin();
            let weak = weak.clone();
            let feed_eps = feed_eps.clone();
            let downloads = downloads.clone();
            let playback = playback.clone();
            let feed_index = weak.unwrap().get_selected_feed() as usize;
            weak.unwrap().set_loading(true);
            weak.unwrap().set_status_text("Refreshing…".into());

            std::thread::spawn(move || {
                let result = fetch_feed(feed_index);
                let _ = slint::invoke_from_event_loop(move || request.apply_if_current(|| {
                    let Some(app) = weak.upgrade() else { return; };
                    let mut library = feed_eps.lock().unwrap();
                    apply_feed_result(&mut library, feed_index, result);
                    let queue = downloads.lock().unwrap();
                    let current = playback.lock().unwrap().current_episode();
                    show_subscriptions(&app, &library);
                    show_feed(&app, &library, &queue, current.as_ref().map(|episode| episode.key.as_str()));
                    app.set_loading(false);
                }));
            });
        });
    }

    // A tap on an undownloaded episode enqueues it and leaves browsing available.
    // Tapping a ready episode starts playback; queued taps never create duplicates.
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let downloads = downloads.clone();
        let audio_tx = audio_tx.clone();
        let current_duration = current_duration.clone();
        let playback = playback.clone();
        app.on_select_episode(move |index| {
            let Ok(idx) = usize::try_from(index) else { return; };
            let (url, key, title, duration, feed_index) = {
                let feed = feed_eps.lock().unwrap();
                let Some(episode) = feed.episodes.get(idx) else { return; };
                (episode.audio_url.clone(), episode.key.clone(), episode.title.clone(), episode.duration.clone(), episode.feed_index)
            };
            let Some(app) = weak.upgrade() else { return; };
            let path = episode_path(&key);
            app.set_selected_index(index);
            if path.exists() {
                let position = resume::position_for(load_state().as_ref(), &key);
                let seconds = parse_duration_secs(&duration);
                *current_duration.lock().unwrap() = seconds;
                let playback_id = playback.lock().unwrap().begin(idx, &key);
                audio_tx.lock().unwrap().send(AudioCmd::Play(playback_id, path, seconds, position)).ok();
                app.set_now_playing_title(title.into());
                app.set_now_playing_feed_title(FEEDS[feed_index].0.into());
                app.set_view("player".into());
                app.set_playing(false);
                app.set_progress(if seconds > 0.0 { (position / seconds).clamp(0.0, 1.0) } else { 0.0 });
                app.set_progress_text(format!("{} / {}", format_time(position), format_time(seconds)).into());
                app.set_player_status("".into());
            } else {
                let mut queue = downloads.lock().unwrap();
                queue.enqueue(key.clone(), title, duration, url, path);
                update_download_views(&app, &queue, &feed_eps.lock().unwrap().episodes, Some(&key));
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
            } else {
                tx.send(AudioCmd::Resume).ok();
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

    // The footer goes to the subscription library. A show opens its own list.
    {
        let weak = app.as_weak();
        app.on_show_list(move || {
            weak.unwrap().set_view("shows".into());
        });
    }
    {
        let weak = app.as_weak();
        let feed_eps = feed_episodes.clone();
        let downloads = downloads.clone();
        let playback = playback.clone();
        app.on_select_feed(move |index| {
            let Ok(index) = usize::try_from(index) else { return; };
            let Some((name, _)) = FEEDS.get(index) else { return; };
            let Some(app) = weak.upgrade() else { return; };
            app.set_selected_feed(index as i32);
            app.set_feed_title((*name).into());
            let current = playback.lock().unwrap().current_episode();
            show_feed(&app, &feed_eps.lock().unwrap(), &downloads.lock().unwrap(),
                current.as_ref().map(|episode| episode.key.as_str()));
            app.set_view("list".into());
        });
    }

    // Download events update only the affected rows. The worker is sequential,
    // while the UI remains free to browse and start already cached episodes.
    {
        let weak = app.as_weak();
        let downloads = downloads.clone();
        let feed_eps = feed_episodes.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(250), move || {
            let Some(app) = weak.upgrade() else { return; };
            let mut queue = downloads.lock().unwrap();
            let changed = queue.drain();
            if changed.is_empty() { return; }
            let feed = feed_eps.lock().unwrap();
            for key in &changed {
                update_download_views(&app, &queue, &feed.episodes, Some(key));
            }
        });
        std::mem::forget(timer);
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
                        AudioEventKind::Playing => {
                            app.set_playing(true);
                            app.set_player_status("".into());
                        }
                        AudioEventKind::Paused => {
                            app.set_playing(false);
                        }
                        AudioEventKind::Error(e) => {
                            eprintln!("podcast playback: {e}");
                            app.set_player_status("Playback failed · select again".into());
                            app.set_playing(false);
                        }
                        AudioEventKind::Warning(message) => {
                            app.set_player_status(message.into());
                        }
                    }
                }
            },
        );
        std::mem::forget(timer);
    }

    app.run().unwrap();
}
