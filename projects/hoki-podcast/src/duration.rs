use std::time::Duration;

pub fn parse_secs(input: &str) -> f32 {
    fn number(input: &str, fraction: bool) -> Option<f32> {
        let mut dot = false;
        if input.is_empty()
            || !input.bytes().all(|byte| {
                if fraction && byte == b'.' && !dot {
                    dot = true;
                    true
                } else {
                    byte.is_ascii_digit()
                }
            })
        {
            return None;
        }
        let value: f32 = input.parse().ok()?;
        Duration::try_from_secs_f32(value).ok()?;
        Some(value)
    }
    let fields: Vec<_> = input.trim().split(':').collect();
    let parsed = (|| match fields.as_slice() {
        [seconds] => number(seconds, true),
        [minutes, seconds] => {
            let seconds = number(seconds, true)?;
            if seconds >= 60.0 {
                return None;
            }
            Some(number(minutes, false)? * 60.0 + seconds)
        }
        [hours, minutes, seconds] => {
            let minutes = number(minutes, false)?;
            let seconds = number(seconds, true)?;
            if minutes >= 60.0 || seconds >= 60.0 {
                return None;
            }
            Some(number(hours, false)? * 3600.0 + minutes * 60.0 + seconds)
        }
        _ => None,
    })();
    parsed
        .filter(|value| Duration::try_from_secs_f32(*value).is_ok())
        .unwrap_or(0.0)
}

pub fn relative_position(current: f32, delta: f32, duration: f32) -> Option<f32> {
    if !current.is_finite() || !delta.is_finite() {
        return None;
    }
    let mut position = (current + delta).max(0.0);
    if duration.is_finite() && duration > 0.0 {
        position = position.min(duration);
    }
    Duration::try_from_secs_f32(position).ok()?;
    Some(position)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_seconds_and_existing_colon_formats() {
        for (input, expected) in [
            ("3661", 3661.0),
            ("61:01", 3661.0),
            ("1:01:01", 3661.0),
            (" 65.5 ", 65.5),
            ("1:05.5", 65.5),
            ("0", 0.0),
        ] {
            assert_eq!(parse_secs(input), expected, "{input}");
        }
    }

    #[test]
    fn invalid_duration_is_unknown_not_a_partial_or_nonfinite_time() {
        for input in [
            "",
            "oops:30",
            "NaN",
            "inf",
            "1:-3",
            "-1",
            "1:60",
            "1:60:00",
            "1:2:3:4",
            "1e30",
            "1.5:20",
            "99999999999999999999999999999999999999999999",
        ] {
            assert_eq!(parse_secs(input), 0.0, "{input}");
        }
    }

    #[test]
    fn seeks_use_current_audio_position_even_without_duration() {
        assert_eq!(relative_position(120.0, 15.0, 0.0), Some(135.0));
        assert_eq!(relative_position(120.0, -15.0, 0.0), Some(105.0));
        assert_eq!(relative_position(5.0, -15.0, 0.0), Some(0.0));
        assert_eq!(relative_position(120.0, 15.0, 130.0), Some(130.0));
        assert_eq!(relative_position(120.0, f32::NAN, 130.0), None);
    }
}
