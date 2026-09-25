//! Shared Settings/daemon acoustic playback preference.
use std::{io, path::PathBuf};
pub const DEFAULT: i32 = 30;

fn path() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty())
        .map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or_else(|| io::Error::other("No user configuration directory"))?;
    Ok(base.join("acoustic-link/volume"))
}
fn parse(text: &str) -> i32 {
    text.trim().parse::<i32>().ok().filter(|v| (0..=100).contains(v)).unwrap_or(DEFAULT)
}
pub fn read() -> i32 {
    path().and_then(std::fs::read_to_string).map(|s| parse(&s)).unwrap_or(DEFAULT)
}
pub fn write(percent: i32) -> io::Result<()> {
    let path = path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{}\n", percent.clamp(0, 100)))?;
    std::fs::rename(temp, path)
}
/// Same cubic UI taper as asteroid-crab-rave/src/audio.rs::volume_gain.
/// Applied to PCM, with Pulse stream gain kept at unity (avoid double taper).
pub fn gain(percent: i32) -> f32 {
    (percent.clamp(0, 100) as f32 / 100.0).powi(3)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crab_curve_and_safe_default() {
        assert_eq!(gain(-1), 0.0);
        assert_eq!(gain(0), 0.0);
        assert_eq!(gain(100), 1.0);
        assert_eq!(gain(101), 1.0);
        assert!((gain(30) - 0.027).abs() < 1e-6);
        assert_eq!(gain(50), 0.125);
        for invalid in ["", "bad", "-1", "101", "99999999999999999"] {
            assert_eq!(parse(invalid), 30);
        }
        assert_eq!(parse("0\n"), 0);
        assert_eq!(parse("100\n"), 100);
    }
}
