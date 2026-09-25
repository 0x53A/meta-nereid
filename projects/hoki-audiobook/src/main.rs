mod history;
mod library;
mod player;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

slint::include_modules!();

// const AUDIOBOOKS_SUBDIR: &str = "src/__from_home/books/Consider Phlebas [B004ASGI7C]";
const AUDIOBOOKS_SUBDIR: &str = "Music/audiobooks";

const HISTORY_FILENAME: &str = ".audiobook-history.jsonl";
const UPDATE_INTERVAL_MS: u64 = 1000;

fn format_time(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

struct AppState {
    books: Vec<library::Book>,
    player: player::Player,
    history: history::HistoryTracker,
    current_book_idx: Option<usize>,
    current_chapter_idx: usize,
}

impl AppState {
    fn current_book(&self) -> Option<&library::Book> {
        self.current_book_idx.map(|i| &self.books[i])
    }

    fn current_book_id(&self) -> Option<String> {
        self.current_book_idx.map(|i| self.books[i].id.clone())
    }

    fn record_play(&mut self, position: f64) {
        if let Some(id) = self.current_book_id() {
            self.history.on_play(&id, self.current_chapter_idx, position);
        }
    }

    fn record_chapter_start(&mut self) {
        if let Some(book) = self.current_book() {
            let start = book.chapters[self.current_chapter_idx].start_secs;
            self.record_play(start);
        }
    }
}

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/ceres".to_string()));

    let player = player::Player::new().expect("failed to init audio player");
    let history = history::HistoryTracker::new(home.join(HISTORY_FILENAME));

    let books_dir = home.join(AUDIOBOOKS_SUBDIR);
    let books = library::scan_books(&books_dir);
    let progress = history.load_progress();

    let state = Arc::new(Mutex::new(AppState {
        books: books.clone(),
        player,
        history,
        current_book_idx: None,
        current_chapter_idx: 0,
    }));

    // Populate UI book lists
    populate_book_lists(&app, &books, &progress);

    // ── open-book ──
    let weak = app.as_weak();
    let st = state.clone();
    app.on_open_book(move |book_id| {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();

        let book_id_str = book_id.to_string();
        let Some(idx) = s.books.iter().position(|b| b.id == book_id_str) else {
            return;
        };

        // Finalize any previous session
        s.history.finalize();

        s.current_book_idx = Some(idx);
        s.current_chapter_idx = 0;

        let book = &s.books[idx];
        app.set_player_book_title(book.title.clone().into());
        app.set_player_total_chapters(book.chapters.len() as i32);
        app.set_show_player(true);
        app.set_player_is_playing(false);

        // Set up chapter list
        let chapters: Vec<ChapterEntry> = book
            .chapters
            .iter()
            .enumerate()
            .map(|(i, ch)| ChapterEntry {
                index: i as i32,
                title: ch.title.clone().into(),
            })
            .collect();
        let chapters_model = std::rc::Rc::new(slint::VecModel::from(chapters));
        app.set_chapters(chapters_model.into());

        // Load first chapter, or resume where we left off
        let progress = s.history.load_progress();
        let Some((resume_chapter, resume_pos)) = book.resume_position(progress.get(&book_id_str)) else {
            return;
        };

        load_chapter(&mut s, resume_chapter, Some(resume_pos), &app);
    });

    // ── play-pause ──
    let st = state.clone();
    let weak = app.as_weak();
    app.on_play_pause(move || {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();
        if s.player.is_playing() {
            if let Err(error) = s.player.pause() {
                eprintln!("audio pause failed: {error}");
                app.set_player_is_playing(s.player.is_playing());
                return;
            }
            s.history.on_pause();
            app.set_player_is_playing(false);
        } else {
            match s.player.play() {
                Ok(()) => {
                    let pos = s.player.position_secs();
                    s.record_play(pos);
                    app.set_player_is_playing(true);
                }
                Err(error) => {
                    eprintln!("audio playback failed: {error}");
                    app.set_player_is_playing(false);
                }
            }
        }
    });

    // ── seek-relative ──
    let st = state.clone();
    app.on_seek_relative(move |secs| {
        let s = st.lock().unwrap();
        let Some(book_idx) = s.current_book_idx else { return };
        let chapter = &s.books[book_idx].chapters[s.current_chapter_idx];
        let pos = s.player.position_secs() + secs as f64;
        // Clamp within chapter boundaries
        let min = chapter.start_secs;
        let max = chapter.end_secs.unwrap_or(f64::MAX);
        if let Err(error) = s.player.seek_to(pos.clamp(min, max)) {
            eprintln!("audio seek failed: {error}");
        }
    });

    // ── next-chapter ──
    let st = state.clone();
    let weak = app.as_weak();
    app.on_next_chapter(move || {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();
        let Some(book_idx) = s.current_book_idx else { return };
        let next = s.current_chapter_idx + 1;
        if next < s.books[book_idx].chapters.len() {
            s.history.finalize();
            if load_chapter(&mut s, next, None, &app) {
                play_chapter(&mut s, &app);
            }
        }
    });

    // ── prev-chapter ──
    let st = state.clone();
    let weak = app.as_weak();
    app.on_prev_chapter(move || {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();
        if s.current_chapter_idx > 0 {
            s.history.finalize();
            let prev = s.current_chapter_idx - 1;
            if load_chapter(&mut s, prev, None, &app) {
                play_chapter(&mut s, &app);
            }
        }
    });

    // ── select-chapter ──
    let st = state.clone();
    let weak = app.as_weak();
    app.on_select_chapter(move |idx| {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();
        let idx = idx as usize;
        let Some(book_idx) = s.current_book_idx else { return };
        if idx < s.books[book_idx].chapters.len() {
            s.history.finalize();
            if load_chapter(&mut s, idx, None, &app) {
                play_chapter(&mut s, &app);
            }
        }
    });

    // ── back-to-library ──
    let st = state.clone();
    let weak = app.as_weak();
    app.on_back_to_library(move || {
        let app = weak.unwrap();
        let mut s = st.lock().unwrap();
        if let Err(error) = s.player.pause() {
            eprintln!("cannot pause audio before returning to library: {error}");
            app.set_player_is_playing(s.player.is_playing());
            return;
        }
        s.history.finalize();
        app.set_player_is_playing(false);
        app.set_show_player(false);

        // Refresh book lists with updated progress
        let progress = s.history.load_progress();
        populate_book_lists(&app, &s.books, &progress);
    });

    // ── Position update timer ──
    let st = state.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(UPDATE_INTERVAL_MS),
        move || {
            let app = weak.unwrap();
            let mut s = st.lock().unwrap();

            let ended = match s.player.poll_event() {
                Some(player::PlaybackEvent::Error(error)) => {
                    eprintln!("audio pipeline failed: {error}");
                    s.history.finalize();
                    app.set_player_is_playing(false);
                    return;
                }
                Some(player::PlaybackEvent::End) => true,
                None => false,
            };

            if !app.get_show_player() {
                return;
            }

            let Some(book_idx) = s.current_book_idx else { return };
            let chapter = &s.books[book_idx].chapters[s.current_chapter_idx];
            let ch_start = chapter.start_secs;
            let ch_end = chapter.end_secs;

            let file_pos = s.player.position_secs();
            let file_dur = s.player.duration_secs();

            // Chapter-relative position and duration
            let ch_pos = (file_pos - ch_start).max(0.0);
            let ch_dur = ch_end.unwrap_or(file_dur) - ch_start;

            app.set_player_position_str(format_time(ch_pos).into());
            app.set_player_duration_str(format_time(ch_dur.max(0.0)).into());
            app.set_player_progress(if ch_dur > 0.0 { (ch_pos / ch_dur) as f32 } else { 0.0 });

            if s.player.is_playing() {
                s.history.on_position(file_pos);
            }

            // Auto-advance: either EOS or past cue chapter end
            let chapter_ended = ended
                || (s.player.is_playing()
                    && ch_end.is_some()
                    && file_pos >= ch_end.unwrap());

            if chapter_ended {
                let next = s.current_chapter_idx + 1;
                if next < s.books[book_idx].chapters.len() {
                    s.history.finalize();
                    if load_chapter(&mut s, next, None, &app) {
                        play_chapter(&mut s, &app);
                    }
                } else {
                    // Book finished
                    let paused = match s.player.pause() {
                        Ok(()) => true,
                        Err(error) => {
                            eprintln!("audio pause at book end failed: {error}");
                            false
                        }
                    };
                    s.history.finalize();
                    app.set_player_is_playing(!paused && s.player.is_playing());
                }
            }
        },
    );

    app.run().unwrap();

    // Finalize on exit
    state.lock().unwrap().history.finalize();
}

fn load_chapter(state: &mut AppState, chapter_idx: usize, position: Option<f64>, app: &App) -> bool {
    let Some(book_idx) = state.current_book_idx else { return false };
    let book = &state.books[book_idx];
    let chapter = &book.chapters[chapter_idx];

    // Keep the selection visible even when its file cannot be loaded; never
    // leave a previous book's chapter title beside the newly selected book.
    state.current_chapter_idx = chapter_idx;
    app.set_player_chapter_title(chapter.title.clone().into());
    app.set_player_chapter_index(chapter_idx as i32);
    app.set_player_position_str("0:00".into());
    app.set_player_progress(0.0);
    if let Err(error) = state.player.load_and_seek(&chapter.path, position.unwrap_or(chapter.start_secs)) {
        eprintln!("cannot load audio chapter {}: {error}", chapter.path.display());
        app.set_player_is_playing(false);
        return false;
    }
    true
}

fn play_chapter(state: &mut AppState, app: &App) {
    match state.player.play() {
        Ok(()) => {
            state.record_chapter_start();
            app.set_player_is_playing(true);
        }
        Err(error) => {
            eprintln!("audio playback failed: {error}");
            app.set_player_is_playing(false);
        }
    }
}

fn populate_book_lists(
    app: &App,
    books: &[library::Book],
    progress: &std::collections::HashMap<String, history::BookProgress>,
) {
    let mut recent: Vec<BookEntry> = Vec::new();
    let mut all: Vec<BookEntry> = Vec::new();

    // Find recently played books (those with progress), sorted by last_played
    let mut with_progress: Vec<_> = books
        .iter()
        .filter_map(|b| progress.get(&b.id).map(|p| (b, p)))
        .collect();
    with_progress.sort_by(|a, b| b.1.last_played.cmp(&a.1.last_played));

    for (book, prog) in with_progress.iter().take(2) {
        recent.push(BookEntry {
            id: book.id.clone().into(),
            title: book.title.clone().into(),
            num_chapters: book.chapters.len() as i32,
            progress_percent: 0.0, // TODO: need total duration
            last_position: format_time(prog.position_secs).into(),
        });
    }

    // All books alphabetically (excluding those in recent)
    let recent_ids: Vec<&str> = recent.iter().map(|r| r.id.as_str()).collect();
    let mut sorted_books: Vec<_> = books
        .iter()
        .filter(|b| !recent_ids.contains(&b.id.as_str()))
        .collect();
    sorted_books.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));

    for book in sorted_books {
        all.push(BookEntry {
            id: book.id.clone().into(),
            title: book.title.clone().into(),
            num_chapters: book.chapters.len() as i32,
            progress_percent: 0.0,
            last_position: Default::default(),
        });
    }

    let recent_model = std::rc::Rc::new(slint::VecModel::from(recent));
    let all_model = std::rc::Rc::new(slint::VecModel::from(all));
    app.set_recent_books(recent_model.into());
    app.set_books(all_model.into());
}
