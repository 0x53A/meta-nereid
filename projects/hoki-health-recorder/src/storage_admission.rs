//! Combined archive admission, not a filesystem reservation.
use crate::Result;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub const HAL_LIMIT: u64 = 1024 * 1024 * 1024;
pub const SSC_LIMIT: u64 = 128 * 1024 * 1024;
pub const RESERVE: u64 = 256 * 1024 * 1024;
pub const HEADROOM: u64 = 16 * 1024 * 1024;

pub fn hal_limit() -> Result<u64> {
    match std::env::var("HOKI_HAL_LIMIT_BYTES") {
        Ok(value) => parse_limit(&value),
        Err(std::env::VarError::NotPresent) => Ok(HAL_LIMIT),
        Err(error) => Err(error.into()),
    }
}
fn parse_limit(value: &str) -> Result<u64> {
    let limit: u64 = value.parse()?;
    if !(128 * 1024 * 1024..=HAL_LIMIT).contains(&limit) {
        return Err("HAL budget must be 128 MiB through 1 GiB".into());
    }
    Ok(limit)
}
fn required_units(block: u64, hal_limit: u64) -> Result<u64> {
    if block == 0 {
        return Err("invalid filesystem allocation unit".into());
    }
    // Round each budget separately, as the independent writers do.
    Ok([hal_limit, SSC_LIMIT, RESERVE, HEADROOM]
        .into_iter()
        .map(|bytes| bytes / block + u64::from(bytes % block != 0))
        .sum())
}

fn admit(available: u64, block: u64, hal_limit: u64) -> Result<()> {
    let required = required_units(block, hal_limit)?;
    if available < required {
        return Err(format!(
            "insufficient recording space: {available} allocation units available, {required} required (unit {block} bytes)"
        ).into());
    }
    Ok(())
}

fn directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err("absolute capture directory required".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err("capture directory must be private and root-owned".into());
    }
    Ok(file)
}

/// Both capture directories must already exist on one filesystem. HAL creates
/// its fresh child directory later. No data or configuration is changed here.
pub fn check(root: &Path, ssc: &Path) -> Result<()> {
    let root = directory(root)?;
    let ssc = directory(ssc)?;
    if root.metadata()?.dev() != ssc.metadata()?.dev() {
        return Err("combined recording requires a shared capture filesystem".into());
    }
    let mut space: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(root.as_raw_fd(), &mut space) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    admit(space.f_bavail as u64, space.f_frsize as u64, hal_limit()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_custom_budget() {
        for value in ["0", "134217727", "1073741825", "abc", ""] {
            assert!(parse_limit(value).is_err());
        }
        assert_eq!(parse_limit("536870912").unwrap(), 512 * 1024 * 1024);
        assert_eq!(required_units(1024, 512 * 1024 * 1024).unwrap(), 933888);
    }
    #[test]
    fn combined_budgets_and_boundaries() {
        assert_eq!(required_units(1024, HAL_LIMIT).unwrap(), 1458176);
        for block in [1, 512, 4096, 65536, 10000, u64::MAX] {
            let needed = required_units(block, HAL_LIMIT).unwrap();
            assert!(admit(needed - 1, block, HAL_LIMIT).is_err());
            assert!(admit(needed, block, HAL_LIMIT).is_ok());
            assert!(admit(u64::MAX, block, HAL_LIMIT).is_ok());
        }
        assert!(admit(u64::MAX, 0, HAL_LIMIT).is_err());
        // Both the former 1 GiB check and HAL-only admission are insufficient.
        assert!(admit(HAL_LIMIT / 4096, 4096, HAL_LIMIT).is_err());
        assert!(admit((HAL_LIMIT + RESERVE + 65536) / 4096, 4096, HAL_LIMIT).is_err());
    }
}
