use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::history::BookProgress;

const AUDIOBOOK_EXTENSIONS: &[&str] = &["opus", "ogg", "mp3", "m4a", "m4b", "flac"];

#[derive(Debug, Clone)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub chapters: Vec<Chapter>,
}

impl Book {
    /// Resolve saved progress without confusing per-file and whole-book positions.
    pub fn resume_position(&self, progress: Option<&BookProgress>) -> Option<(usize, f64)> {
        let first = self.chapters.first()?;
        let Some(progress) = progress else { return Some((0, first.start_secs)); };
        let position = progress.position_secs;
        if !position.is_finite() || position < 0.0 || position * 1_000_000_000.0 >= u64::MAX as f64 {
            return Some((0, first.start_secs));
        }
        let index = match progress.chapter_index {
            Some(index) if index < self.chapters.len() => index,
            Some(_) => return Some((0, first.start_secs)),
            None if self.chapters.iter().all(|chapter| chapter.path == first.path) => {
                self.chapters.iter().rposition(|chapter| chapter.start_secs <= position).unwrap_or(0)
            }
            // Old multi-file history has no recoverable chapter identity.
            None => 0,
        };
        let chapter = &self.chapters[index];
        Some((index, position.clamp(chapter.start_secs, chapter.end_secs.unwrap_or(f64::MAX))))
    }
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    pub path: PathBuf,
    /// Start position within the file (0.0 for file-per-chapter books)
    pub start_secs: f64,
    /// End position within the file (None = end of file)
    pub end_secs: Option<f64>,
}

/// Scan the audiobooks directory.
///
/// Structure:
/// - Directory with audio files → one book, files sorted by name = chapters
/// - Audio file + matching .cue → one book, chapters from cue sheet
/// - Single audio file (no cue) → one book, one chapter
pub fn scan_books(dir: &Path) -> Vec<Book> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("cannot read audiobooks dir: {}", dir.display());
        return Vec::new();
    };

    let mut books = Vec::new();

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            let chapters = scan_chapters(&path);
            if !chapters.is_empty() {
                books.push(Book {
                    id: name.clone(),
                    title: name,
                    chapters,
                });
            }
        } else if is_audio_file(&path) {
            let title = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| name.clone());

            // Look for a .cue file next to the audio file
            let cue_path = path.with_extension("cue");
            let chapters = if cue_path.exists() {
                parse_cue(&cue_path, &path)
            } else {
                vec![]
            };

            let chapters = if chapters.is_empty() {
                vec![Chapter {
                    title: "Full".to_string(),
                    path: path.clone(),
                    start_secs: 0.0,
                    end_secs: None,
                }]
            } else {
                chapters
            };

            books.push(Book {
                id: name,
                title,
                chapters,
            });
        }
    }

    books
}

fn scan_chapters(dir: &Path) -> Vec<Chapter> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut all: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|path| path.is_file())
        .collect();
    all.sort();

    // Check for cue files first — if any audio file has a matching .cue, use it
    for p in &all {
        if p.extension().and_then(|e| e.to_str()) == Some("cue") {
            // Find the audio file this cue references (or the one with the same stem)
            let audio_path = find_audio_for_cue(p, &all);
            if let Some(audio_path) = audio_path {
                let chapters = parse_cue(p, &audio_path);
                if !chapters.is_empty() {
                    return chapters;
                }
            }
        }
    }

    // Fall back to file-per-chapter
    let mut files: Vec<PathBuf> = all.into_iter().filter(|p| is_audio_file(p)).collect();
    files.sort();

    files
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let raw = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("Chapter {}", i + 1));
            let title = strip_chapter_prefix(&raw);
            Chapter {
                title,
                path,
                start_secs: 0.0,
                end_secs: None,
            }
        })
        .collect()
}

/// Find the audio file referenced by a cue sheet, or the one with the same stem
fn find_audio_for_cue(cue_path: &Path, all_files: &[PathBuf]) -> Option<PathBuf> {
    // Try parsing the FILE directive from the cue
    if let Ok(content) = std::fs::read_to_string(cue_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("FILE ") {
                if let Some(filename) = parse_cue_filename(trimmed) {
                    let candidate = cue_path.parent().unwrap_or(Path::new(".")).join(&filename);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    // Fallback: same stem, any audio extension
    let stem = cue_path.file_stem()?;
    all_files
        .iter()
        .find(|p| p.file_stem() == Some(stem) && is_audio_file(p))
        .cloned()
}

/// Parse a cue sheet into chapters for a given audio file.
///
/// Supports the standard cue format:
///   FILE "filename.opus" OGG
///     TRACK 01 AUDIO
///       TITLE "Chapter Title"
///       INDEX 01 MM:SS:FF
///
/// Where FF is frames (75 per second for CD, but we also handle decimal seconds).
fn parse_cue(cue_path: &Path, audio_path: &Path) -> Vec<Chapter> {
    let Ok(file) = std::fs::File::open(cue_path) else {
        return Vec::new();
    };

    let mut chapters = Vec::new();
    let mut current_title: Option<String> = None;
    let mut track_num = 0u32;

    for line in std::io::BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("cannot read CUE sheet {}: {error}", cue_path.display());
                return Vec::new();
            }
        };
        let trimmed = line.trim();

        if trimmed.starts_with("TRACK ") {
            // Titles are scoped to a track, even when it has no INDEX 01.
            track_num += 1;
            current_title = None;
        } else if trimmed.starts_with("TITLE ") {
            // Could be album title (outside TRACK) or chapter title (inside TRACK)
            if track_num > 0 {
                current_title = Some(parse_cue_quoted(trimmed, "TITLE "));
            }
        } else if trimmed.starts_with("INDEX 01 ") {
            let time_str = trimmed.strip_prefix("INDEX 01 ").unwrap_or("").trim();
            let Some(start_secs) = parse_cue_time(time_str) else {
                eprintln!("invalid CUE timestamp in {}: {time_str}", cue_path.display());
                return Vec::new();
            };
            if track_num == 0 || chapters.last().is_some_and(|ch: &Chapter| start_secs <= ch.start_secs) {
                eprintln!("invalid CUE chapter order in {}", cue_path.display());
                return Vec::new();
            }

            let title = current_title
                .take()
                .unwrap_or_else(|| format!("Chapter {}", track_num));

            chapters.push(Chapter {
                title,
                path: audio_path.to_path_buf(),
                start_secs,
                end_secs: None, // filled in below
            });
        }
    }

    // Set end_secs: each chapter ends where the next begins
    for i in 0..chapters.len().saturating_sub(1) {
        chapters[i].end_secs = Some(chapters[i + 1].start_secs);
    }
    // Last chapter: end_secs = None (end of file)

    chapters
}

/// Parse a cue FILE line to extract the filename.
/// e.g. `FILE "my file.opus" OGG` → `my file.opus`
fn parse_cue_filename(line: &str) -> Option<String> {
    let rest = line.strip_prefix("FILE ")?.trim();
    if rest.starts_with('"') {
        let end = rest[1..].find('"')?;
        Some(rest[1..1 + end].to_string())
    } else {
        // Unquoted: take until next space
        Some(rest.split_whitespace().next()?.to_string())
    }
}

/// Extract quoted string after a keyword: `TITLE "foo bar"` → `foo bar`
fn parse_cue_quoted(line: &str, keyword: &str) -> String {
    let rest = line.strip_prefix(keyword).unwrap_or(line).trim();
    if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
        rest[1..rest.len() - 1].to_string()
    } else {
        rest.to_string()
    }
}

/// Parse cue timestamp. Supports:
/// - MM:SS:FF (75 frames/sec, standard CD cue)
/// - MM:SS.mmm (decimal seconds)
/// - HH:MM:SS.mmm
fn parse_cue_time(s: &str) -> Option<f64> {
    fn integer(s: &str) -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }

    fn seconds(s: &str) -> Option<f64> {
        if let Some((whole, fraction)) = s.split_once('.') {
            integer(whole)?;
            if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        } else {
            integer(s)?;
        }
        let value: f64 = s.parse().ok()?;
        (value < 60.0).then_some(value)
    }

    let parts: Vec<&str> = s.split(':').collect();
    let value = match parts.as_slice() {
        [hours, minutes, secs] if secs.contains('.') => {
            let minutes = integer(minutes)?;
            if minutes >= 60 { return None; }
            integer(hours)? as f64 * 3600.0 + minutes as f64 * 60.0 + seconds(secs)?
        }
        [minutes, secs, frames] => {
            let secs = integer(secs)?;
            let frames = integer(frames)?;
            if secs >= 60 || frames >= 75 { return None; }
            integer(minutes)? as f64 * 60.0 + secs as f64 + frames as f64 / 75.0
        }
        [minutes, secs] => integer(minutes)? as f64 * 60.0 + seconds(secs)?,
        _ => return None,
    };
    // GStreamer's clock represents nanoseconds in u64, reserving u64::MAX.
    (value.is_finite() && value * 1_000_000_000.0 < u64::MAX as f64).then_some(value)
}

fn strip_chapter_prefix(name: &str) -> String {
    let trimmed = name.trim_start_matches(|c: char| c.is_ascii_digit());
    let trimmed = trimmed.trim_start_matches(|c: char| c == '.' || c == '-' || c == '_' || c == ' ');
    if trimmed.is_empty() {
        name.to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_audio_file(path: &Path) -> bool {
    path.is_file() && path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIOBOOK_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Sort books with recently-played first, then alphabetical
pub fn sort_books_by_recent(books: &mut [Book], progress: &HashMap<String, BookProgress>) {
    books.sort_by(|a, b| {
        let pa = progress.get(&a.id);
        let pb = progress.get(&b.id);
        match (pa, pb) {
            (Some(pa), Some(pb)) => pb.last_played.cmp(&pa.last_played),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.title.cmp(&b.title),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_scan_excludes_directories_with_audio_extensions() {
        let dir = std::env::temp_dir().join(format!("hoki-library-file-types-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let real = dir.join("02 Real.MP3");
        std::fs::write(&real, []).unwrap();
        let nested = dir.join("01 Folder.mp3");
        std::fs::create_dir(&nested).unwrap();
        let cue = dir.join("00 Folder.cue");
        std::fs::write(&cue, "FILE \"01 Folder.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n").unwrap();
        let chapters = scan_chapters(&dir);
        let referenced = find_audio_for_cue(&cue, &[nested.clone(), real.clone()]);
        std::fs::remove_file(&cue).unwrap();
        let without_cue = scan_chapters(&dir);
        #[cfg(unix)]
        {
            let link = dir.join("03 Link.mp3");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(is_audio_file(&link), "symlinks to regular audio files remain supported");
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].path, real);
        assert_eq!(without_cue.len(), 1);
        assert_eq!(without_cue[0].path, real);
        assert!(referenced.is_none(), "a CUE audio reference must be a file");
    }

    #[test]
    fn resume_uses_saved_chapter_for_multiple_files_and_preserves_legacy_cues() {
        let mut book = Book {
            id: "book".into(), title: "book".into(),
            chapters: (0..3).map(|index| Chapter {
                title: index.to_string(), path: PathBuf::from(format!("{index}.opus")),
                start_secs: 0.0, end_secs: None,
            }).collect(),
        };
        let mut progress = BookProgress { position_secs: 20.0, chapter_index: Some(1), last_played: chrono::Utc::now() };
        assert_eq!(book.resume_position(Some(&progress)), Some((1, 20.0)));
        progress.chapter_index = None;
        assert_eq!(book.resume_position(Some(&progress)), Some((0, 20.0)));
        progress.chapter_index = Some(99);
        assert_eq!(book.resume_position(Some(&progress)), Some((0, 0.0)));
        for (index, chapter) in book.chapters.iter_mut().enumerate() {
            chapter.path = PathBuf::from("book.opus");
            chapter.start_secs = index as f64 * 100.0;
            chapter.end_secs = (index < 2).then_some((index + 1) as f64 * 100.0);
        }
        progress.chapter_index = None;
        progress.position_secs = 150.0;
        assert_eq!(book.resume_position(Some(&progress)), Some((1, 150.0)));
        progress.chapter_index = Some(1);
        progress.position_secs = 5.0;
        assert_eq!(book.resume_position(Some(&progress)), Some((1, 100.0)));
        progress.position_secs = 250.0;
        assert_eq!(book.resume_position(Some(&progress)), Some((1, 200.0)));
        progress.position_secs = f64::INFINITY;
        assert_eq!(book.resume_position(Some(&progress)), Some((0, 0.0)));
        book.chapters.clear();
        assert_eq!(book.resume_position(Some(&progress)), None);
    }

    #[test]
    fn cue_timestamp_formats_and_ranges() {
        for (input, expected) in [("00:00:00", 0.0), ("02:03:74", 123.0 + 74.0 / 75.0),
            ("02:03.250", 123.25), ("01:02:03.250", 3723.25), ("123:00", 7380.0)] {
            assert_eq!(parse_cue_time(input), Some(expected), "{input}");
        }
        for input in ["", "bad", "1", "1:2:3:4", "00:NaN", "00:inf", "-1:00", "1:-2",
            "00:60", "00:60:00", "00:00:75", "1:60:00.0", "1.5:00", "1:00:1.5.2",
            "1:00.", "1:1e2", "99999999999999999999:00", "999999999:00"] {
            assert_eq!(parse_cue_time(input), None, "{input}");
        }
    }

    #[test]
    fn invalid_cue_timing_uses_the_full_file_fallback() {
        let dir = std::env::temp_dir().join(format!("hoki-cue-timing-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("book.opus"), []).unwrap();
        let cue = dir.join("book.cue");
        for times in [["00:10:00", "00:05:00"], ["00:10:00", "00:10:00"], ["invalid", "00:20:00"]] {
            std::fs::write(&cue, format!(
                "TRACK 01 AUDIO\nINDEX 01 {}\nTRACK 02 AUDIO\nINDEX 01 {}\n", times[0], times[1]
            )).unwrap();
            let books = scan_books(&dir);
            assert_eq!(books.len(), 1);
            let chapters = &books[0].chapters;
            if chapters.len() != 1 || chapters[0].start_secs != 0.0 || chapters[0].end_secs.is_some() {
                std::fs::remove_dir_all(&dir).unwrap();
                panic!("invalid CUE times {times:?} did not fall back to the full file");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_cue_uses_full_file_instead_of_partial_chapters() {
        let dir = std::env::temp_dir().join(format!("hoki-cue-read-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("book.opus"), []).unwrap();
        let cue = dir.join("book.cue");
        // An adjacent directory passes exists(), but cannot supply CUE lines.
        std::fs::create_dir(&cue).unwrap();
        let books = scan_books(&dir);
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].chapters.len(), 1);
        assert_eq!(books[0].chapters[0].start_secs, 0.0);
        std::fs::remove_dir(&cue).unwrap();
        let bytes = b"TRACK 01 AUDIO\nINDEX 01 00:10:00\n\xff\nTRACK 02 AUDIO\nINDEX 01 00:20:00\n";
        std::fs::write(&cue, bytes).unwrap();
        let books = scan_books(&dir);
        assert_eq!(books.len(), 1);
        let chapters = &books[0].chapters;
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "Full");
        assert_eq!(chapters[0].start_secs, 0.0);
        assert!(chapters[0].end_secs.is_none());
        assert_eq!(std::fs::read(&cue).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cue_titles_belong_to_their_own_tracks() {
        let dir = std::env::temp_dir().join(format!("hoki-cue-titles-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let cue = dir.join("book.cue");
        let audio = dir.join("book.opus");
        std::fs::write(&cue, concat!(
            "FILE \"book.opus\" OGG\n",
            "TITLE \"Album title\"\n",
            "TRACK 01 AUDIO\n",
            "TITLE \"Skipped track\"\n",
            "INDEX 00 00:00:00\n",
            "TRACK 02 AUDIO\n",
            "INDEX 01 00:01:00\n",
            "TRACK 03 AUDIO\n",
            "TITLE \"Über den Wolken\"\n",
            "INDEX 01 01:02:00\n",
            "TRACK 04 AUDIO\n",
            "INDEX 01 02:03:00\n",
        )).unwrap();
        let chapters = parse_cue(&cue, &audio);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].title, "Chapter 2");
        assert_eq!(chapters[1].title, "Über den Wolken");
        assert_eq!(chapters[2].title, "Chapter 4");
        assert!(chapters.iter().all(|chapter| chapter.path == audio));
        assert_eq!(chapters[0].start_secs, 1.0);
        assert_eq!(chapters[0].end_secs, Some(62.0));
        assert_eq!(chapters[1].end_secs, Some(123.0));
        assert_eq!(chapters[2].end_secs, None);
    }
}
