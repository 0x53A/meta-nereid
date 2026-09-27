//! Space on the filesystem holding recordings, rather than the read-only image.
#[derive(Default)]
pub struct Snapshot {
    pub used: String,
    pub available: String,
    pub total: String,
    pub reserved: String,
    pub summary: String,
}
pub fn read() -> Snapshot {
    if crate::simulated::enabled() {
        return Snapshot {
            used: crate::simulated::read("disk-used", "3.2 GiB"),
            available: crate::simulated::read("disk-free", "585 MiB"),
            total: crate::simulated::read("disk-total", "4.0 GiB"),
            reserved: crate::simulated::read("disk-reserved", "234 MiB"),
            summary: crate::simulated::read("disk-summary", "80% used · 3.2/4.0 GiB"),
        };
    }
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL-terminated and statvfs initializes the output on
    // success. No fields are read on failure.
    if unsafe { libc::statvfs(c"/var/lib".as_ptr(), stats.as_mut_ptr()) } != 0 {
        return from_bytes(0, 0, 0);
    }
    let stats = unsafe { stats.assume_init() };
    let unit = stats.f_frsize as u64;
    from_bytes((stats.f_blocks as u64).saturating_mul(unit),
        (stats.f_bfree as u64).saturating_mul(unit),
        (stats.f_bavail as u64).saturating_mul(unit))
}
fn from_bytes(total: u64, free: u64, available: u64) -> Snapshot {
    if total == 0 {
        return Snapshot { used: "—".into(), available: "—".into(), total: "—".into(),
            reserved: "—".into(), summary: "Storage unavailable".into() };
    }
    let used = total.saturating_sub(free);
    let gib = (1_u64 << 30) as f64;
    Snapshot {
        used: format_bytes(used), available: format_bytes(available), total: format_bytes(total),
        reserved: format_bytes(free.saturating_sub(available)),
        summary: format!("{:.0}% used · {:.1}/{:.1} GiB", used as f64 * 100. / total as f64,
            used as f64 / gib, total as f64 / gib),
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GiB", bytes as f64 / (1_u64 << 30) as f64)
    } else {
        format!("{} MiB", bytes / (1 << 20))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_and_reserved_space_are_distinct() {
        let s = from_bytes(4 << 30, 2 << 30, 1536 << 20);
        assert_eq!(s.summary, "50% used · 2.0/4.0 GiB");
        assert_eq!(s.available, "1.5 GiB");
        assert_eq!(s.reserved, "512 MiB");
        assert_eq!(from_bytes(0, 0, 0).summary, "Storage unavailable");
        assert_eq!(from_bytes(4 << 30, 0, 0).summary, "100% used · 4.0/4.0 GiB");
    }
}
