use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

const PAUSE_MERGE_SECS: f64 = 60.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub book_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter_index: Option<usize>,
    pub started: DateTime<Utc>,
    pub from_secs: f64,
    pub to_secs: f64,
}

pub struct HistoryTracker {
    path: PathBuf,
    active: Option<ActiveSession>,
}

struct ActiveSession {
    book_id: String,
    chapter_index: usize,
    started: DateTime<Utc>,
    from_secs: f64,
    to_secs: f64,
    last_update: Instant,
}

impl HistoryTracker {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            path,
            active: None,
        }
    }

    /// Call when playback starts or resumes
    pub fn on_play(&mut self, book_id: &str, chapter_index: usize, position_secs: f64) {
        let now = Instant::now();
        if let Some(ref active) = self.active {
            // Same book, short pause → merge
            if active.book_id == book_id
                && active.chapter_index == chapter_index
                && now.duration_since(active.last_update).as_secs_f64() < PAUSE_MERGE_SECS
            {
                return;
            }
            // Different book or long pause → finalize previous
            self.finalize();
        }
        self.active = Some(ActiveSession {
            book_id: book_id.to_string(),
            chapter_index,
            started: Utc::now(),
            from_secs: position_secs,
            to_secs: position_secs,
            last_update: now,
        });
    }

    /// Call periodically during playback to update position
    pub fn on_position(&mut self, position_secs: f64) {
        if let Some(ref mut active) = self.active {
            active.to_secs = position_secs;
            active.last_update = Instant::now();
        }
    }

    /// Call when playback pauses or stops
    pub fn on_pause(&mut self) {
        // Don't finalize immediately — wait for PAUSE_MERGE_SECS
        // The next on_play will decide whether to merge or finalize
    }

    /// Call when switching books or closing the app
    pub fn finalize(&mut self) {
        if let Some(session) = self.active.take() {
            if (session.to_secs - session.from_secs).abs() < 1.0 {
                return; // Skip trivially short sessions
            }
            let entry = Session {
                book_id: session.book_id,
                chapter_index: Some(session.chapter_index),
                started: session.started,
                from_secs: session.from_secs,
                to_secs: session.to_secs,
            };
            self.append(&entry);
        }
    }

    fn append(&self, session: &Session) {
        if let Err(error) = self.try_append(session) {
            eprintln!("failed to append history file {:?}: {error}", self.path);
        }
    }

    fn try_append(&self, session: &Session) -> std::io::Result<()> {
        let line = serde_json::to_string(session)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        // A previous interrupted write may have left an unterminated record.
        // Preserve it, but keep it from absorbing this complete session.
        if file.seek(SeekFrom::End(0))? > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        file.write_all(format!("{line}\n").as_bytes())
    }

    /// Read the last valid appended session per book, including backward seeks.
    pub fn load_progress(&self) -> HashMap<String, BookProgress> {
        let mut map = HashMap::new();
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return map,
            Err(error) => {
                eprintln!("failed to open history file {:?}: {error}", self.path);
                return map;
            }
        };
        for line in BufReader::new(file).split(b'\n') {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    eprintln!("failed to read history file {:?}: {error}", self.path);
                    break;
                }
            };
            // Malformed records are skippable; a persistent stream I/O error
            // is not. Decode JSON from bytes so invalid UTF-8 stays local to
            // its record instead of being confused with a stream failure.
            let Ok(session) = serde_json::from_slice::<Session>(&line) else {
                continue;
            };
            if !session.to_secs.is_finite() || session.to_secs < 0.0
                || session.to_secs * 1_000_000_000.0 >= u64::MAX as f64 {
                continue;
            }
            // Append order survives wall-clock corrections and chapter changes.
            map.insert(session.book_id, BookProgress {
                position_secs: session.to_secs,
                chapter_index: session.chapter_index,
                last_played: session.started,
            });
        }
        map
    }
}

impl Drop for HistoryTracker {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[derive(Debug, Clone)]
pub struct BookProgress {
    pub position_secs: f64,
    pub chapter_index: Option<usize>,
    pub last_played: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_directory_read_error_returns_without_retrying_forever() {
        let path = std::env::temp_dir().join(format!("hoki-history-directory-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let tracker = HistoryTracker::new(path.clone());
        assert!(tracker.load_progress().is_empty());
        assert!(path.is_dir());
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn invalid_utf8_record_does_not_hide_later_valid_progress() {
        let path = std::env::temp_dir().join(format!("hoki-history-utf8-{}.jsonl", std::process::id()));
        let mut data = b"\xff invalid record\n".to_vec();
        data.extend_from_slice(b"{\"book_id\":\"book\",\"started\":\"2026-01-01T12:00:00Z\",\"from_secs\":0,\"to_secs\":40}\n");
        fs::write(&path, &data).unwrap();
        let tracker = HistoryTracker::new(path.clone());
        assert_eq!(tracker.load_progress()["book"].position_secs, 40.0);
        assert_eq!(fs::read(&path).unwrap(), data);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn appending_after_unterminated_tail_preserves_new_progress() {
        for (case, tail) in [
            ("truncated", "{\"book_id\":\"old"),
            ("complete", "{\"book_id\":\"old\",\"started\":\"2026-01-01T12:00:00Z\",\"from_secs\":0,\"to_secs\":40}"),
        ] {
            let path = std::env::temp_dir().join(format!(
                "hoki-history-tail-{case}-{}.jsonl", std::process::id()
            ));
            fs::write(&path, tail).unwrap();
            let mut tracker = HistoryTracker::new(path.clone());
            tracker.on_play("new", 3, 10.0);
            tracker.on_position(30.0);
            tracker.finalize();
            let progress = tracker.load_progress();
            let text = fs::read_to_string(&path).unwrap();
            fs::remove_file(&path).unwrap();
            assert!(text.starts_with(tail), "existing tail must remain untouched");
            let saved = progress.get("new").expect("new session must be readable");
            assert_eq!(saved.position_secs, 30.0);
            assert_eq!(saved.chapter_index, Some(3));
            if case == "complete" {
                assert_eq!(progress["old"].position_secs, 40.0);
            }
        }
    }

    #[test]
    fn chapter_changes_persist_separate_sessions_and_resume_the_latest_file() {
        let path = std::env::temp_dir().join(format!("hoki-history-chapters-{}.jsonl", std::process::id()));
        let mut tracker = HistoryTracker::new(path.clone());
        tracker.on_play("book", 1, 0.0);
        tracker.on_position(500.0);
        // Even an immediate chapter switch must not merge into the old chapter.
        tracker.on_play("book", 2, 0.0);
        tracker.on_position(20.0);
        tracker.on_pause();
        tracker.on_play("book", 2, 20.0);
        tracker.on_position(22.0);
        tracker.finalize();
        let text = std::fs::read_to_string(&path).unwrap();
        let sessions: Vec<Session> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let progress = tracker.load_progress();
        std::fs::remove_file(path).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].chapter_index, Some(1));
        assert_eq!(sessions[1].chapter_index, Some(2));
        assert_eq!(progress["book"].chapter_index, Some(2));
        assert_eq!(progress["book"].position_secs, 22.0);
    }

    #[test]
    fn append_order_survives_clock_changes_and_bad_trailing_records() {
        let path = std::env::temp_dir().join(format!("hoki-history-order-{}.jsonl", std::process::id()));
        std::fs::write(&path, concat!(
            "{\"book_id\":\"book\",\"started\":\"2026-01-01T13:00:00Z\",\"from_secs\":0,\"to_secs\":500}\n",
            "{\"book_id\":\"book\",\"chapter_index\":1,\"started\":\"2026-01-01T12:00:00Z\",\"from_secs\":0,\"to_secs\":20}\n",
            "{\"book_id\":\"book\",\"started\":\"2026-01-01T14:00:00Z\",\"from_secs\":0,\"to_secs\":-10}\n",
            "{\"book_id\":\"book\",\"started\":\"2026-01-01T14:00:00Z\",\"from_secs\":0,\"to_secs\":1e100}\n",
            "{unfinished\n",
        )).unwrap();
        let progress = HistoryTracker::new(path.clone()).load_progress();
        std::fs::remove_file(path).unwrap();
        assert_eq!(progress["book"].chapter_index, Some(1));
        assert_eq!(progress["book"].position_secs, 20.0);
    }

    #[test]
    fn legacy_history_resumes_latest_position_after_rewinding() {
        let path = std::env::temp_dir().join(format!("hoki-history-rewind-{}.jsonl", std::process::id()));
        std::fs::write(&path, concat!(
            "{\"book_id\":\"book\",\"started\":\"2026-01-01T12:00:00Z\",\"from_secs\":0,\"to_secs\":500}\n",
            "{\"book_id\":\"book\",\"started\":\"2026-01-01T13:00:00Z\",\"from_secs\":500,\"to_secs\":200}\n",
        )).unwrap();
        let progress = HistoryTracker::new(path.clone()).load_progress();
        std::fs::remove_file(path).unwrap();
        assert_eq!(progress["book"].position_secs, 200.0);
    }
}
