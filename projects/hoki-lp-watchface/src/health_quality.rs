//! Timestamp diagnostics, not an inference of an exact lost-sample count.
#[derive(Debug, PartialEq)]
pub enum TimingIssue {
    Gap {
        previous: i64,
        current: i64,
        expected_ns: i64,
    },
    NonIncreasing {
        previous: i64,
        current: i64,
    },
}
pub struct TimingMonitor {
    start_ns: i64,
    streams: std::collections::HashMap<u32, (i64, Option<i64>)>,
}
impl TimingMonitor {
    pub fn new(start_ns: i64, periods: impl IntoIterator<Item = (u32, i64)>) -> Self {
        Self {
            start_ns,
            streams: periods.into_iter().map(|(h, p)| (h, (p, None))).collect(),
        }
    }
    pub fn observe(&mut self, handle: u32, timestamp: i64) -> Option<TimingIssue> {
        // Cached startup samples remain in the recording but cannot establish
        // continuity for this session. Only continuous descriptors are registered.
        let (period, previous) = self.streams.get_mut(&handle)?;
        if timestamp < self.start_ns && previous.is_none() {
            return None;
        }
        let old = previous.replace(timestamp)?;
        if timestamp <= old {
            Some(TimingIssue::NonIncreasing {
                previous: old,
                current: timestamp,
            })
        } else if timestamp.saturating_sub(old) > period.saturating_add(*period / 2) {
            Some(TimingIssue::Gap {
                previous: old,
                current: timestamp,
                expected_ns: *period,
            })
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_cache_does_not_create_a_false_gap() {
        let mut m = TimingMonitor::new(1000, [(3, 40)]);
        assert_eq!(m.observe(3, 1), None);
        assert_eq!(m.observe(3, 1000), None);
        assert_eq!(m.observe(3, 1040), None);
        assert_eq!(m.observe(99, 4000), None);
        assert_eq!(
            m.observe(3, 1120),
            Some(TimingIssue::Gap {
                previous: 1040,
                current: 1120,
                expected_ns: 40
            })
        );
    }
    #[test]
    fn reports_reset_without_dropping_following_timestamps() {
        let mut m = TimingMonitor::new(0, [(3, 40)]);
        assert_eq!(m.observe(3, 100), None);
        assert_eq!(
            m.observe(3, 90),
            Some(TimingIssue::NonIncreasing {
                previous: 100,
                current: 90
            })
        );
        assert_eq!(m.observe(3, 130), None);
        assert_eq!(
            m.observe(3, 130),
            Some(TimingIssue::NonIncreasing {
                previous: 130,
                current: 130
            })
        );
    }
    #[test]
    fn reset_below_session_start_is_not_mistaken_for_startup_cache() {
        let mut m = TimingMonitor::new(1000, [(3, 40)]);
        assert_eq!(m.observe(3, 1000), None);
        assert_eq!(
            m.observe(3, 20),
            Some(TimingIssue::NonIncreasing {
                previous: 1000,
                current: 20
            })
        );
    }

    #[test]
    fn interleaved_channels_keep_independent_startup_and_gap_history() {
        let mut m = TimingMonitor::new(1000, [(3, 40), (4, 100)]);
        assert_eq!(m.observe(3, 1000), None);
        // Another channel's cached startup record cannot reset channel 3.
        assert_eq!(m.observe(4, 10), None);
        assert_eq!(m.observe(3, 1040), None);
        assert_eq!(m.observe(4, 1000), None);
        assert_eq!(m.observe(3, 1080), None);
        assert_eq!(m.observe(4, 1100), None);
        assert_eq!(m.observe(3, 1200), Some(TimingIssue::Gap {
            previous: 1080, current: 1200, expected_ns: 40,
        }));
        assert_eq!(m.observe(4, 1200), None);
        assert_eq!(m.observe(3, 20), Some(TimingIssue::NonIncreasing {
            previous: 1200, current: 20,
        }));
        assert_eq!(m.observe(4, 1300), None);
    }
}
