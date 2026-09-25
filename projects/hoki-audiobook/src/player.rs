use gstreamer as gst;
use gst::prelude::*;
use std::path::{Path, PathBuf};

fn audio_file_uri(path: &Path) -> Result<gst::glib::GString, String> {
    let absolute = std::path::absolute(path).map_err(|err| err.to_string())?;
    gst::glib::filename_to_uri(absolute, None).map_err(|err| err.to_string())
}

pub struct Player {
    playbin: gst::Element,
    loaded_path: Option<PathBuf>,
}

pub enum PlaybackEvent {
    End,
    Error(String),
}

impl Player {
    pub fn new() -> Result<Self, String> {
        gst::init().map_err(|e| format!("gstreamer init failed: {e}"))?;
        let playbin = gst::ElementFactory::make("playbin")
            .build()
            .map_err(|e| format!("failed to create playbin: {e}"))?;
        // Use alsasink directly — PulseAudio's ALSA backend uses mmap which
        // doesn't produce audible output on the Qualcomm ASoC driver.
        if let Ok(sink) = gst::ElementFactory::make("alsasink")
            .property("device", "hw:0,0")
            .build()
        {
            playbin.set_property("audio-sink", &sink);
        }
        Ok(Self { playbin, loaded_path: None })
    }

    /// Load a file and optionally seek to a position.
    /// Reuses the same file's pipeline; successful loads are paused and seekable.
    pub fn load_and_seek(&mut self, path: &Path, secs: f64) -> Result<(), String> {
        let result = self.try_load_and_seek(path, secs);
        if result.is_err() {
            self.loaded_path = None;
            self.playbin.set_state(gst::State::Null).ok();
        }
        result
    }

    fn try_load_and_seek(&mut self, path: &Path, secs: f64) -> Result<(), String> {
        if !secs.is_finite() {
            return Err("invalid audio seek position".into());
        }
        let already_loaded = self.loaded_path.as_deref() == Some(path);
        if !already_loaded {
            self.loaded_path = None;
            self.playbin.set_state(gst::State::Null).map_err(|e| e.to_string())?;
            let uri = audio_file_uri(path)?;
            self.playbin.set_property("uri", &uri);
        }
        self.playbin.set_state(gst::State::Paused).map_err(|e| e.to_string())?;
        let (result, state, _) = self.playbin.state(gst::ClockTime::from_seconds(2));
        result.map_err(|e| e.to_string())?;
        if state != gst::State::Paused {
            return Err("audio preroll timed out".into());
        }
        if already_loaded || secs > 0.0 {
            self.seek_to(secs)?;
        }
        self.loaded_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn play(&self) -> Result<(), String> {
        if self.loaded_path.is_none() {
            return Err("no successfully loaded audio file".into());
        }
        self.playbin.set_state(gst::State::Playing).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn pause(&self) -> Result<(), String> {
        // Pausing after a failed load must not start preroll of its stale URI.
        if self.loaded_path.is_none() {
            return Ok(());
        }
        self.playbin.set_state(gst::State::Paused).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn stop(&self) {
        self.playbin.set_state(gst::State::Null).ok();
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.playbin.current_state(), gst::State::Playing)
    }

    /// Current position in seconds
    pub fn position_secs(&self) -> f64 {
        self.playbin
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds() as f64 / 1_000_000_000.0)
            .unwrap_or(0.0)
    }

    /// Duration of current file in seconds
    pub fn duration_secs(&self) -> f64 {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(|t| t.nseconds() as f64 / 1_000_000_000.0)
            .unwrap_or(0.0)
    }

    /// Seek to absolute position in seconds
    pub fn seek_to(&self, secs: f64) -> Result<(), String> {
        if !secs.is_finite() {
            return Err("invalid audio seek position".into());
        }
        let secs = secs.max(0.0);
        let nanos = secs * 1_000_000_000.0;
        if !nanos.is_finite() || nanos >= u64::MAX as f64 {
            return Err("audio seek position exceeds clock range".into());
        }
        let time = gst::ClockTime::from_nseconds(nanos as u64);
        self.playbin
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, time)
            .map_err(|e| e.to_string())
    }

    /// Seek relative (positive or negative seconds)
    pub fn seek_relative(&self, delta_secs: f64) {
        let pos = self.position_secs();
        if let Err(error) = self.seek_to(pos + delta_secs) {
            eprintln!("audio seek failed: {error}");
        }
    }

    /// Drain pipeline messages, surfacing failures separately from normal EOS.
    pub fn poll_event(&mut self) -> Option<PlaybackEvent> {
        if let Some(bus) = self.playbin.bus() {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gst::MessageView::Eos(..) => return Some(PlaybackEvent::End),
                    gst::MessageView::Error(error) => {
                        let message = error.error().to_string();
                        self.loaded_path = None;
                        self.playbin.set_state(gst::State::Null).ok();
                        return Some(PlaybackEvent::Error(message));
                    }
                    _ => {}
                }
            }
        }
        None
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.playbin.set_state(gst::State::Null).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_error_invalidates_load_and_is_not_end_of_stream() {
        gst::init().unwrap();
        let playbin = gst::ElementFactory::make("playbin").build().unwrap();
        playbin.set_state(gst::State::Ready).unwrap();
        let bus = playbin.bus().unwrap();
        let mut player = Player { playbin, loaded_path: Some(PathBuf::from("fixture.wav")) };
        bus.post(gst::message::Eos::builder().build()).unwrap();
        assert!(matches!(player.poll_event(), Some(PlaybackEvent::End)));
        bus.post(gst::message::Error::builder(gst::ResourceError::Read, "fixture read failure").build()).unwrap();
        match player.poll_event() {
            Some(PlaybackEvent::Error(message)) => assert!(message.contains("fixture read failure")),
            _ => panic!("pipeline error was not surfaced"),
        }
        assert!(player.loaded_path.is_none());
        assert_eq!(player.playbin.current_state(), gst::State::Null);
        assert!(player.play().is_err());
    }

    #[test]
    fn pause_reports_preroll_failure_for_unavailable_cached_source() {
        gst::init().unwrap();
        let dir = std::env::temp_dir().join(format!("hoki-audio-pause-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("missing.wav");
        let playbin = gst::ElementFactory::make("playbin").build().unwrap();
        for sink in ["audio-sink", "video-sink"] {
            playbin.set_property(sink, gst::ElementFactory::make("fakesink").build().unwrap());
        }
        playbin.set_property("uri", audio_file_uri(&path).unwrap());
        // Cached URI in NULL, as after stop(), but its source is unavailable.
        let player = Player { playbin, loaded_path: Some(path) };
        let result = player.pause();
        drop(player);
        std::fs::remove_dir(dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn file_uris_preserve_paths_without_requiring_existing_files() {
        for path in [Path::new("/Music/Über den Wolken #1%.opus"), Path::new("books/Chapter 1?.opus")] {
            let uri = audio_file_uri(path).unwrap();
            let (decoded, hostname) = gst::glib::filename_from_uri(&uri).unwrap();
            assert_eq!(decoded, std::path::absolute(path).unwrap());
            assert!(hostname.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_uris_preserve_non_utf8_filename_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/Music/chapter-\xff.opus"));
        let uri = audio_file_uri(path).unwrap();
        assert_eq!(gst::glib::filename_from_uri(&uri).unwrap().0, path);
    }

    #[test]
    fn loads_audio_with_uri_delimiters_in_its_filename() {
        gst::init().unwrap();
        let dir = std::env::temp_dir().join(format!("hoki-audio-uri-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("Chapter #1 100% ready?.wav");
        // 100 ms of mono, 8 kHz, 16-bit PCM silence.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&1636u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&1600u32.to_le_bytes());
        wav.resize(1644, 0);
        let playbin = gst::ElementFactory::make("playbin").build().unwrap();
        for sink in ["audio-sink", "video-sink"] {
            playbin.set_property(sink, gst::ElementFactory::make("fakesink").build().unwrap());
        }
        let mut player = Player { playbin, loaded_path: None };
        assert!(player.play().is_err());
        assert!(player.load_and_seek(&path, 0.0).is_err());
        assert!(player.loaded_path.is_none());
        assert!(player.play().is_err());
        player.pause().unwrap();
        assert_eq!(player.playbin.current_state(), gst::State::Null);
        std::fs::write(&path, wav).unwrap();
        // Retrying the same path must rebuild after the failed preroll.
        player.load_and_seek(&path, 0.0).unwrap();
        assert_eq!(player.loaded_path.as_deref(), Some(path.as_path()));
        let duration = player.duration_secs();
        let state = player.playbin.current_state();
        player.pause().unwrap();
        assert!(player.seek_to(f64::INFINITY).is_err());
        assert!(player.seek_to(f64::MAX).is_err());
        assert!(player.load_and_seek(&path, f64::NAN).is_err());
        assert!(player.loaded_path.is_none());
        assert!(player.play().is_err());
        player.pause().unwrap();
        assert_eq!(player.playbin.current_state(), gst::State::Null);
        drop(player);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(state, gst::State::Paused);
        assert!((duration - 0.1).abs() < 0.001, "duration: {duration}");
    }
}
