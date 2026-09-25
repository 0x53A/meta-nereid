use std::path::{Path, PathBuf};

pub fn episode_key(url: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn episode_path(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("audio-{key}.mp3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_are_stable_and_include_the_entire_url() {
        assert_eq!(
            episode_key("https://example.test/episode.mp3"),
            "249223ef3199e22f997f0dc6d45f7354a2baa6e977536a04cde5b33a6f629b6b"
        );
        assert_ne!(
            episode_key("https://example.test/episode.mp3?a=1"),
            episode_key("https://example.test/episode.mp3?a=2")
        );
    }

    #[test]
    fn reordering_the_feed_keeps_cached_audio_with_its_url() {
        let dir = std::env::temp_dir().join(format!("hoki-cache-identity-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let first = "https://example.test/first.mp3";
        let second = "https://example.test/second.mp3";
        std::fs::write(episode_path(&dir, &episode_key(first)), b"first audio").unwrap();
        std::fs::write(episode_path(&dir, &episode_key(second)), b"second audio").unwrap();
        std::fs::write(dir.join("ep_0.mp3"), b"unknown legacy audio").unwrap();
        let reordered = [second, first];
        let selected_audio = std::fs::read(episode_path(&dir, &episode_key(reordered[0]))).unwrap();
        let old_file = std::fs::read(dir.join("ep_0.mp3")).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(selected_audio, b"second audio");
        assert_eq!(old_file, b"unknown legacy audio");
    }
}
