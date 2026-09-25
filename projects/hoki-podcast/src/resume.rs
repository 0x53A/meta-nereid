use std::io;
use std::path::Path;
use std::time::Duration;

pub fn save_state(
    path: &Path,
    episode: &crate::playback::PlayingEpisode,
    position: f32,
) -> io::Result<()> {
    Duration::try_from_secs_f32(position)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let value = serde_json::json!({ "episode": episode.index, "episode_key": episode.key, "position": position });
    let bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
    crate::storage::replace_bytes(&bytes, path)
}

#[derive(Debug, PartialEq)]
pub struct SavedState {
    pub episode: usize,
    pub position: f32,
    pub episode_key: Option<String>,
}

impl SavedState {
    pub fn selected_index(&self, keys: &[&str]) -> Option<usize> {
        match self.episode_key.as_deref() {
            Some(key) => keys.iter().position(|candidate| *candidate == key),
            None => (self.episode < keys.len()).then_some(self.episode),
        }
    }
}

pub fn decode_state(data: &str) -> Option<SavedState> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let episode = usize::try_from(value["episode"].as_u64()?).ok()?;
    let position = value["position"].as_f64()? as f32;
    Duration::try_from_secs_f32(position).ok()?;
    let episode_key = value["episode_key"].as_str().map(str::to_owned);
    Some(SavedState {
        episode,
        position,
        episode_key,
    })
}

pub fn position_for(state: Option<&SavedState>, key: &str) -> f32 {
    state
        .filter(|saved| saved.episode_key.as_deref() == Some(key))
        .map(|saved| saved.position)
        .unwrap_or(0.0)
}

pub fn seek(sink: &rodio::Sink, seconds: f32) -> Result<(), String> {
    let position = Duration::try_from_secs_f32(seconds).map_err(|err| err.to_string())?;
    sink.try_seek(position).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_state_round_trips_and_invalid_progress_preserves_it() {
        let dir = std::env::temp_dir().join(format!("hoki-state-snapshot-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("state.json");
        let episode = crate::playback::PlayingEpisode {
            index: 3,
            key: "audio-a".into(),
        };
        save_state(&path, &episode, 123.5).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        for invalid in [-1.0, f32::NAN, f32::INFINITY, 1e30] {
            assert!(save_state(&path, &episode, invalid).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let saved = decode_state(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(saved.episode, 3);
        assert_eq!(saved.episode_key.as_deref(), Some("audio-a"));
        assert_eq!(saved.position, 123.5);
        save_state(&path, &episode, 125.0).unwrap();
        assert_eq!(
            decode_state(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .position,
            125.0
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restores_only_the_saved_episode_and_preserves_fractional_position() {
        let saved = decode_state(r#"{"episode":3,"episode_key":"audio-a","position":123.5}"#);
        assert_eq!(position_for(saved.as_ref(), "audio-a"), 123.5);
        assert_eq!(position_for(saved.as_ref(), "audio-b"), 0.0);
        assert_eq!(position_for(None, "audio-a"), 0.0);
        assert_eq!(
            saved.unwrap().selected_index(&["audio-b", "audio-a"]),
            Some(1)
        );
    }

    #[test]
    fn legacy_state_does_not_guess_the_episode_for_saved_progress() {
        let saved = decode_state(r#"{"episode":1,"position":123.5}"#).unwrap();
        assert_eq!(saved.selected_index(&["audio-a", "audio-b"]), Some(1));
        assert_eq!(position_for(Some(&saved), "audio-b"), 0.0);
    }

    #[test]
    fn invalid_state_cannot_reach_duration_conversion() {
        for input in [
            r#"{"episode":3,"position":-1}"#,
            r#"{"episode":3,"position":1e100}"#,
            r#"{"episode":3,"position":1e30}"#,
            r#"{"episode":-1,"position":2}"#,
            r#"{"episode":3.5,"position":2}"#,
            "{unfinished",
        ] {
            assert_eq!(decode_state(input), None, "{input}");
        }
        let max_index = format!(r#"{{"episode":{},"position":2}}"#, u64::MAX);
        assert_eq!(
            decode_state(&max_index).map(|state| state.episode),
            usize::try_from(u64::MAX).ok()
        );
    }

    fn headless_seek(
        source: impl rodio::Source<Item = f32> + Send + 'static,
    ) -> (Result<(), String>, Duration) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let (sink, mut output) = rodio::Sink::new_idle();
        sink.pause();
        sink.append(source);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let consumer = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                output.next();
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        let result = seek(&sink, 3.5);
        let position = sink.get_pos();
        stop.store(true, Ordering::Relaxed);
        consumer.join().unwrap();
        (result, position)
    }

    #[test]
    fn headless_sink_seek_updates_playback_position() {
        let (result, position) = headless_seek(rodio::buffer::SamplesBuffer::new(
            1,
            1000,
            vec![0.0f32; 10_000],
        ));
        result.unwrap();
        assert_eq!(position, Duration::from_millis(3500));
    }

    #[test]
    fn headless_sink_reports_unsupported_seeks_as_failures() {
        struct Unseekable;
        impl Iterator for Unseekable {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                Some(0.0)
            }
        }
        impl rodio::Source for Unseekable {
            fn current_frame_len(&self) -> Option<usize> {
                None
            }
            fn channels(&self) -> u16 {
                1
            }
            fn sample_rate(&self) -> u32 {
                1000
            }
            fn total_duration(&self) -> Option<Duration> {
                None
            }
        }
        let (result, _) = headless_seek(Unseekable);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_seek_is_rejected_without_changing_the_sink_position() {
        let (sink, _output) = rodio::Sink::new_idle();
        for seconds in [-1.0, f32::NAN, f32::INFINITY, 1e30] {
            assert!(seek(&sink, seconds).is_err());
            assert_eq!(sink.get_pos(), Duration::ZERO);
        }
    }
}
