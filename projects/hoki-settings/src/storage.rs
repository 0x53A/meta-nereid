//! Space on the filesystem holding recordings, rather than the read-only image.

pub fn read() -> (String, String) {
    if crate::simulated::enabled() {
        return (
            crate::simulated::read("disk-used", "3.2 GiB"),
            crate::simulated::read("disk-free", "585 MiB"),
        );
    }
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL-terminated and statvfs initializes the output on
    // success. No fields are read on failure.
    if unsafe { libc::statvfs(c"/var/lib".as_ptr(), stats.as_mut_ptr()) } != 0 {
        return ("—".into(), "—".into());
    }
    let stats = unsafe { stats.assume_init() };
    let unit = stats.f_frsize as u64;
    let used = (stats.f_blocks as u64).saturating_sub(stats.f_bfree as u64);
    // Available space excludes filesystem blocks reserved for root, matching
    // the space available to ordinary applications and df's available column.
    (
        format_bytes(used.saturating_mul(unit)),
        format_bytes((stats.f_bavail as u64).saturating_mul(unit)),
    )
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GiB", bytes as f64 / (1_u64 << 30) as f64)
    } else {
        format!("{} MiB", bytes / (1 << 20))
    }
}
