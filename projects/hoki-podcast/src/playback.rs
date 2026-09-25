/// The playing episode is independent of the list selection: a selected episode
/// may still be downloading while the previous episode continues playing.
#[derive(Default)]
pub struct Playback {
    next_id: u64,
    current: Option<(u64, PlayingEpisode)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayingEpisode {
    pub index: usize,
    pub key: String,
}

impl Playback {
    pub fn current_episode(&self) -> Option<PlayingEpisode> {
        self.current.as_ref().map(|(_, episode)| episode.clone())
    }

    pub fn begin(&mut self, episode: usize, key: &str) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.current = Some((
            self.next_id,
            PlayingEpisode {
                index: episode,
                key: key.to_owned(),
            },
        ));
        self.next_id
    }

    pub fn episode_for(&self, playback_id: u64) -> Option<PlayingEpisode> {
        self.current
            .as_ref()
            .filter(|(id, _)| *id == playback_id)
            .map(|(_, episode)| episode.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_positions_and_errors_from_old_playback_are_rejected() {
        let mut playback = Playback::default();
        let first = playback.begin(3, "audio-a");
        let second = playback.begin(8, "audio-b");
        assert_eq!(playback.episode_for(first), None);
        assert_eq!(
            playback.episode_for(second),
            Some(PlayingEpisode {
                index: 8,
                key: "audio-b".into()
            })
        );
    }

    #[test]
    fn restarting_the_same_episode_still_supersedes_its_old_events() {
        let mut playback = Playback::default();
        let first = playback.begin(3, "audio-a");
        let restarted = playback.begin(3, "audio-a");
        assert_ne!(first, restarted);
        assert_eq!(playback.episode_for(first), None);
        assert_eq!(
            playback.episode_for(restarted),
            Some(PlayingEpisode {
                index: 3,
                key: "audio-a".into()
            })
        );
    }

    #[test]
    fn events_without_playback_have_no_episode() {
        assert_eq!(Playback::default().episode_for(0), None);
    }
}
