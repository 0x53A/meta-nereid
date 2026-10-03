//! The Sidekick RTC is UTC; its datetime resource needs a local-time offset.
pub fn offset_millis_at(timestamp: libc::time_t) -> Result<i32, String> {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return Err("could not resolve local timezone for Sidekick clock".into());
    }
    i32::try_from(local.tm_gmtoff)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1000))
        .ok_or_else(|| "Sidekick timezone offset exceeds millisecond field".into())
}

pub fn apply(time: &mut [u8; 48]) -> Result<i32, String> {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    if now == -1 { return Err("could not read clock for Sidekick timezone".into()); }
    let millis = offset_millis_at(now)?;
    // Signed base millisecond offset, separate from the base day offset at 0.
    time[4..8].copy_from_slice(&millis.to_le_bytes());
    Ok(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timezone_offsets() {
        // Isolated processes avoid changing TZ underneath other parallel tests.
        if let Ok(expected) = std::env::var("HOKI_TEST_ZONE_OFFSETS") {
            let expected: Vec<i32> = expected.split(',').map(|v| v.parse().unwrap()).collect();
            // 2026-01-01, 2026-07-01, and either side of Berlin's spring DST jump.
            for (timestamp, expected) in [1767225600, 1782864000, 1774745999, 1774746000].into_iter().zip(expected) {
                assert_eq!(offset_millis_at(timestamp).unwrap(), expected);
            }
            let mut wire = [0u8; 48];
            let offset = apply(&mut wire).unwrap();
            assert_eq!(i32::from_le_bytes(wire[4..8].try_into().unwrap()), offset);
            assert!(wire[..4].iter().chain(&wire[8..]).all(|b| *b == 0));
            return;
        }
        for (zone, expected) in [
            ("UTC0", "0,0,0,0"),
            ("CET-1CEST,M3.5.0,M10.5.0/3", "3600000,7200000,3600000,7200000"),
            ("EST5", "-18000000,-18000000,-18000000,-18000000"),
            ("NPT-5:45", "20700000,20700000,20700000,20700000"),
        ] {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "timezone::tests::timezone_offsets"])
                .env("TZ", zone).env("HOKI_TEST_ZONE_OFFSETS", expected)
                .status().unwrap().success(), "timezone {zone}");
        }
    }
}
