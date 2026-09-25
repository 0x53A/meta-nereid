use crate::model::{Config, Source, Track};
use anyhow::{bail, Context, Result};
use std::{fs::File, path::Path};
use symphonia::core::{
    codecs::audio::AudioDecoder,
    formats::probe::Hint,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
    io::{MediaSource, MediaSourceStream},
    meta::{MetadataOptions, StandardTag},
    units::{Time, TimeBase},
};

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    id: u32,
    time_base: Option<TimeBase>,
    pub position: f64,
    pub duration: f64,
}
pub struct Block {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub channels: u8,
}
fn probe(source: Box<dyn MediaSource>) -> Result<Box<dyn FormatReader>> {
    let options = MetadataOptions::default()
        .limit_tag_bytes(symphonia::core::common::Limit::Maximum(64 * 1024))
        .limit_visual_bytes(symphonia::core::common::Limit::Maximum(0));
    Ok(symphonia::default::get_probe().probe(
        &Hint::new(),
        MediaSourceStream::new(source, Default::default()),
        FormatOptions::default(),
        options,
    )?)
}
impl Decoder {
    pub fn open(track: &Track, config: &Config) -> Result<Self> {
        let source: Box<dyn MediaSource> = match &track.source {
            Source::Local { path } => Box::new(File::open(path).context("Cannot open music file")?),
            Source::Navidrome { server, id } => {
                let configured = config
                    .navidrome
                    .as_ref()
                    .filter(|s| &s.url == server)
                    .context("Configure this track's Navidrome server first")?;
                let mut url = crate::network::endpoint(configured, "stream")?;
                url.query_pairs_mut()
                    .extend_pairs([("id", id.as_str()), ("format", "raw")]);
                Box::new(crate::network::HttpSource::new(url)?)
            }
        };
        Self::from_source(source)
    }
    pub fn from_source(source: Box<dyn MediaSource>) -> Result<Self> {
        let format = probe(source)?;
        let track = format
            .default_track(TrackType::Audio)
            .context("No audio track found")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .context("Unsupported audio codec")?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &Default::default())
            .context("Unsupported audio codec")?;
        let duration = track
            .time_base
            .zip(track.duration)
            .and_then(|(t, d)| t.calc_duration(d))
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        let (id, time_base) = (track.id, track.time_base);
        Ok(Self {
            format,
            decoder,
            id,
            time_base,
            position: 0.0,
            duration,
        })
    }
    pub fn next(&mut self) -> Result<Option<Block>> {
        let mut corrupt = 0;
        while let Some(packet) = self.format.next_packet()? {
            if packet.track_id != self.id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let rate = buf.spec().rate();
                    let channels = buf.spec().channels().count();
                    if !matches!(channels, 1 | 2) || rate == 0 {
                        bail!("Only mono and stereo music are supported");
                    }
                    let mut samples = vec![0f32; buf.samples_interleaved()];
                    buf.copy_to_slice_interleaved(&mut samples);
                    let duration = buf.frames() as f64 / rate as f64;
                    self.position = self
                        .time_base
                        .and_then(|t| t.calc_time(packet.pts))
                        .map(|t| t.as_secs_f64())
                        .unwrap_or(self.position)
                        + duration;
                    return Ok(Some(Block {
                        samples,
                        rate,
                        channels: channels as u8,
                    }));
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) if corrupt < 8 => {
                    corrupt += 1;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        let time = Time::try_from_secs_f64(seconds.max(0.0)).context("Invalid seek time")?;
        let result = self.format.seek(
            SeekMode::Coarse,
            SeekTo::Time {
                time,
                track_id: Some(self.id),
            },
        )?;
        self.decoder.reset();
        self.position = self
            .time_base
            .and_then(|t| t.calc_time(result.actual_ts))
            .map(|t| t.as_secs_f64())
            .unwrap_or(seconds);
        Ok(())
    }
}
pub fn inspect(path: &Path) -> Result<Track> {
    let mut format = probe(Box::new(File::open(path)?))?;
    let audio = format.default_track(TrackType::Audio).context("No audio")?;
    let duration = audio
        .time_base
        .zip(audio.duration)
        .and_then(|(t, d)| t.calc_duration(d))
        .map(|t| t.as_secs_f64())
        .unwrap_or(0.0);
    let mut result = Track {
        title: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        artist: "Unknown artist".into(),
        album: String::new(),
        duration,
        source: Source::Local {
            path: path.to_path_buf(),
        },
    };
    let mut metadata = format.metadata();
    while let Some(revision) = metadata.pop() {
        for tag in revision.media.tags {
            match tag.std {
                Some(StandardTag::TrackTitle(v)) => result.title = v.chars().take(256).collect(),
                Some(StandardTag::Artist(v)) => result.artist = v.chars().take(256).collect(),
                Some(StandardTag::Album(v)) => result.album = v.chars().take(256).collect(),
                _ => (),
            }
        }
    }
    // Metadata::pop retains the latest revision.
    if let Some(revision) = metadata.current() {
        for tag in &revision.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(v)) => result.title = v.chars().take(256).collect(),
                Some(StandardTag::Artist(v)) => result.artist = v.chars().take(256).collect(),
                Some(StandardTag::Album(v)) => result.album = v.chars().take(256).collect(),
                _ => (),
            }
        }
    }
    Ok(result)
}
pub fn local_library(config: &Config) -> Result<Vec<Track>> {
    let mut paths = Vec::new();
    for dir in &config.music_dirs {
        scan(dir, 0, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    let mut tracks = Vec::new();
    for path in paths {
        match inspect(&path) {
            Ok(track) => tracks.push(track),
            Err(_) => tracks.push(Track {
                title: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                artist: "Unknown artist".into(),
                album: String::new(),
                duration: 0.0,
                source: Source::Local { path },
            }),
        }
    }
    Ok(tracks)
}
fn scan(dir: &Path, depth: usize, paths: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if depth > 16 {
        bail!("Music folders nested more than 16 levels");
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            scan(&path, depth + 1, paths)?;
        } else if kind.is_file()
            && path.extension().is_some_and(|s| {
                matches!(
                    s.to_string_lossy().to_lowercase().as_str(),
                    "mp3" | "flac" | "m4a" | "aac" | "ogg" | "oga" | "wav" | "aiff" | "aif"
                )
            })
        {
            if paths.len() >= 10000 {
                bail!("Local library exceeds 10,000 tracks");
            }
            paths.push(std::fs::canonicalize(path)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn wave() -> Vec<u8> {
        let samples = 8000u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(samples * 2).to_le_bytes());
        for n in 0..samples {
            b.extend_from_slice(&((n % 100) as i16 * 100).to_le_bytes());
        }
        b
    }
    #[test]
    fn decode_pcm_and_seek() {
        let mut decoder = Decoder::from_source(Box::new(Cursor::new(wave()))).unwrap();
        assert!((decoder.duration - 1.0).abs() < 0.001);
        let mut frames = 0;
        while let Some(b) = decoder.next().unwrap() {
            assert_eq!(b.rate, 8000);
            assert_eq!(b.channels, 1);
            assert!(b.samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
            frames += b.samples.len();
        }
        assert_eq!(frames, 8000);
        decoder.seek(0.5).unwrap();
        assert!(decoder.next().unwrap().is_some());
        assert!(decoder.position >= 0.5);
    }
    #[test]
    fn reject_garbage() {
        assert!(Decoder::from_source(Box::new(Cursor::new(vec![1u8; 128]))).is_err());
    }
}
