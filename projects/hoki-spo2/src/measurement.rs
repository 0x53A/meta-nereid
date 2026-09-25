//! Pure SpO2 report contract shared by the UI worker and offline replay tests.

#[derive(Clone, Copy, Debug)]
pub struct RawReading {
    pub timestamp_us: u64,
    pub oxygen: f64,
    pub confidence: f64,
    pub algorithm: f64,
    pub signal: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcceptedReading {
    pub oxygen: f64,
    pub confidence: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadingDecision {
    Final(AcceptedReading),
    Progress(&'static str),
    Rejected(&'static str),
}

// Recovered stock enum values. These are codes, not bit flags; unknown values
// stay unknown and are never accepted.
pub fn algorithm_code(value: f64) -> Option<u8> {
    if value.is_finite() && value.fract() == 0.0 && (0.0..=5.0).contains(&value) {
        Some(value as u8)
    } else {
        None
    }
}

pub fn signal_code(value: f64) -> Option<u8> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    match value as i32 {
        0 | 1 | 2 | 4 | 8 | 10 | 20 | 40 => Some(value as u8),
        _ => None,
    }
}

pub fn inspect(raw: RawReading) -> ReadingDecision {
    if !raw.oxygen.is_finite()
        || !raw.confidence.is_finite()
        || !raw.algorithm.is_finite()
        || !raw.signal.is_finite()
    {
        return ReadingDecision::Rejected("Invalid non-finite sensor report");
    }
    let algorithm = match algorithm_code(raw.algorithm) {
        Some(value) => value,
        None => return ReadingDecision::Rejected("Unknown algorithm state"),
    };
    let signal = match signal_code(raw.signal) {
        Some(value) => value,
        None => return ReadingDecision::Rejected("Unknown signal state"),
    };
    if algorithm != 5 {
        return ReadingDecision::Progress(progress_text(algorithm, signal));
    }
    if signal != 0 {
        return ReadingDecision::Rejected("Final report has a non-clear signal");
    }
    // Stock checks lower thresholds. These explicit upper and non-negative
    // bounds are additional sanity checks for a D-Bus client.
    if raw.oxygen < 0.0 || raw.oxygen > 100.0 {
        return ReadingDecision::Rejected("Final oxygen value is outside 0..100");
    }
    if raw.confidence < 0.0 || raw.confidence > 100.0 {
        return ReadingDecision::Rejected("Final confidence is outside 0..100");
    }
    if raw.oxygen.trunc() <= 80.0 {
        return ReadingDecision::Rejected("Final oxygen value is below the UI threshold");
    }
    if raw.confidence < 80.0 {
        return ReadingDecision::Rejected("Final confidence is below the UI threshold");
    }
    ReadingDecision::Final(AcceptedReading {
        oxygen: raw.oxygen,
        confidence: raw.confidence,
    })
}

fn progress_text(algorithm: u8, signal: u8) -> &'static str {
    match signal {
        1 => "No signal — adjust the watch.",
        2 => "Signal weak — hold still.",
        4 => "Low perfusion — keep the watch snug.",
        8 => "Movement detected — hold still.",
        10 => "Fit is too tight — adjust the watch.",
        20 => "Fit is too loose — adjust the watch.",
        40 => "Signal out of bounds — adjust the watch.",
        _ => match algorithm {
            0 => "Preparing sensor…",
            1 => "Sensor ready — hold still.",
            2 => "Waiting for no motion…",
            3 => "Collecting signal…",
            4 => "Calculating oxygen…",
            _ => "Waiting for an accepted final report…",
        },
    }
}

pub fn is_fresh(timestamp_us: u64, floor_us: u64, last_us: &mut u64) -> bool {
    if timestamp_us <= floor_us || timestamp_us <= *last_us {
        return false;
    }
    *last_us = timestamp_us;
    true
}

#[cfg(test)]
mod tests {
    use super::{inspect, is_fresh, AcceptedReading, RawReading, ReadingDecision};

    #[test]
    fn malformed_fields_and_threshold_edges_never_pass() {
        for field in 0..4 {
            for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut fields = [98.0, 90.0, 5.0, 0.0];
                fields[field] = invalid;
                assert!(matches!(
                    inspect(raw(20, fields[0], fields[1], fields[2], fields[3])),
                    ReadingDecision::Rejected(_)
                ));
            }
        }
        for (oxygen, confidence, algorithm, signal) in [
            (80.99, 90.0, 5.0, 0.0),
            (98.0, 79.99, 5.0, 0.0),
            (98.0, 90.0, 4.99, 0.0),
            (98.0, 90.0, 5.0, 0.01),
            (98.0, 90.0, 5.0, -1.0),
            (98.0, 90.0, 5.0, 1e30),
        ] {
            assert!(matches!(
                inspect(raw(20, oxygen, confidence, algorithm, signal)),
                ReadingDecision::Rejected(_)
            ));
        }
        for oxygen in [81.0, 100.0] {
            assert!(matches!(
                inspect(raw(20, oxygen, 80.0, 5.0, 0.0)),
                ReadingDecision::Final(_)
            ));
        }
    }

    fn raw(
        timestamp_us: u64,
        oxygen: f64,
        confidence: f64,
        algorithm: f64,
        signal: f64,
    ) -> RawReading {
        RawReading {
            timestamp_us,
            oxygen,
            confidence,
            algorithm,
            signal,
        }
    }

    #[test]
    fn accepts_only_fresh_clear_final_with_bounds() {
        assert_eq!(
            inspect(raw(20, 98.7, 95.0, 5.0, 0.0)),
            ReadingDecision::Final(AcceptedReading {
                oxygen: 98.7,
                confidence: 95.0
            })
        );
        assert!(matches!(
            inspect(raw(20, 100.1, 95.0, 5.0, 0.0)),
            ReadingDecision::Rejected(_)
        ));
        assert!(matches!(
            inspect(raw(20, 98.7, 101.0, 5.0, 0.0)),
            ReadingDecision::Rejected(_)
        ));
    }

    #[test]
    fn rejects_unknown_states_and_bad_final_signal() {
        assert!(matches!(
            inspect(raw(20, 98.0, 90.0, 99.0, 0.0)),
            ReadingDecision::Rejected("Unknown algorithm state")
        ));
        assert!(matches!(
            inspect(raw(20, 98.0, 90.0, 5.0, 3.0)),
            ReadingDecision::Rejected("Unknown signal state")
        ));
        assert!(matches!(
            inspect(raw(20, 98.0, 90.0, 5.0, 8.0)),
            ReadingDecision::Rejected("Final report has a non-clear signal")
        ));
    }

    #[test]
    fn progress_never_becomes_a_result() {
        assert!(matches!(
            inspect(raw(20, 98.0, 99.0, 4.0, 0.0)),
            ReadingDecision::Progress(_)
        ));
        assert!(matches!(
            inspect(raw(20, 98.0, 99.0, 5.0, 0.0)),
            ReadingDecision::Final(_)
        ));
    }

    #[test]
    fn freshness_rejects_baseline_duplicates_and_out_of_order() {
        let mut last = 100;
        assert!(!is_fresh(100, 100, &mut last));
        assert!(!is_fresh(99, 100, &mut last));
        assert!(is_fresh(101, 100, &mut last));
        assert!(!is_fresh(101, 100, &mut last));
        assert!(!is_fresh(100, 100, &mut last));
        assert!(is_fresh(102, 100, &mut last));
    }
}
