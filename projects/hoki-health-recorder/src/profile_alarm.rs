//! A configuration lifetime includes suspend time and can wake for restoration.
use hoki_health_recorder::Result;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};

pub const MAX_PROFILE_SECONDS: u64 = 86_400;

pub fn service_seconds(seconds: u64) -> Result<u64> {
    if !(1..=MAX_PROFILE_SECONDS).contains(&seconds) {
        return Err("profile duration must be 1..86400 seconds".into());
    }
    // Preserve the prior 600s bound for short trials and the 420s activation
    // allowance for longer sessions. Independent restoration has its own bound.
    Ok(600.max(seconds + 420))
}

pub struct ProfileAlarm(File);
impl ProfileAlarm {
    pub fn new() -> Result<Self> {
        Self::with_clock(libc::CLOCK_BOOTTIME_ALARM)
    }
    fn with_clock(clock: libc::clockid_t) -> Result<Self> {
        let fd = unsafe { libc::timerfd_create(clock, libc::TFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    pub fn arm(&self, seconds: u64) -> Result<()> {
        service_seconds(seconds)?;
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: seconds as _,
                tv_nsec: 0,
            },
        };
        if unsafe { libc::timerfd_settime(self.0.as_raw_fd(), 0, &spec, std::ptr::null_mut()) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn wait(mut self) -> Result<()> {
        // read_exact retries EINTR without restarting the armed deadline.
        // SIGTERM retains its normal disposition; ExecStopPost owns recovery.
        let mut count = [0u8; 8];
        self.0.read_exact(&mut count)?;
        if u64::from_ne_bytes(count) != 1 {
            return Err("unexpected profile alarm expiration count".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_shot_deadline_and_bounds() {
        // Host tests do not require CAP_WAKE_ALARM. The watch uses ALARM above.
        let alarm = ProfileAlarm::with_clock(libc::CLOCK_BOOTTIME).unwrap();
        assert!(alarm.arm(0).is_err());
        assert!(alarm.arm(MAX_PROFILE_SECONDS + 1).is_err());
        assert!(alarm.arm(u64::MAX).is_err());
        let start = std::time::Instant::now();
        alarm.arm(1).unwrap();
        alarm.wait().unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_secs(1));
    }
    #[test]
    fn overnight_alarm_and_service_bounds() {
        assert_eq!(service_seconds(1).unwrap(), 600);
        assert_eq!(service_seconds(180).unwrap(), 600);
        assert_eq!(service_seconds(181).unwrap(), 601);
        assert_eq!(service_seconds(28_800).unwrap(), 29_220);
        assert_eq!(service_seconds(MAX_PROFILE_SECONDS).unwrap(), 86_820);
        for seconds in [28_800, MAX_PROFILE_SECONDS] {
            let alarm = ProfileAlarm::with_clock(libc::CLOCK_BOOTTIME).unwrap();
            alarm.arm(seconds).unwrap();
            let mut spec: libc::itimerspec = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::timerfd_gettime(alarm.0.as_raw_fd(), &mut spec) },
                0
            );
            assert!(spec.it_value.tv_sec as u64 <= seconds);
            assert!(spec.it_value.tv_sec as u64 >= seconds - 1);
            assert_eq!(spec.it_interval.tv_sec, 0);
            assert_eq!(spec.it_interval.tv_nsec, 0);
        }
    }
}
