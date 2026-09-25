//! An absolute wall-clock timer plus SIGHUP, with no one-second reload polling.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub struct ClockWait {
    timer: OwnedFd,
    reload: OwnedFd,
}
fn next_boundary(now: i64, seconds: bool) -> i64 {
    let interval = if seconds { 1 } else { 60 };
    (now.div_euclid(interval) + 1) * interval
}
impl ClockWait {
    /// Create before UI/worker threads so all threads inherit the blocked signal.
    pub fn new() -> io::Result<Self> {
        unsafe {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGHUP);
            let e = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            if e != 0 {
                return Err(io::Error::from_raw_os_error(e));
            }
            let fd = libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let reload = OwnedFd::from_raw_fd(fd);
            let fd =
                libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                reload,
                timer: OwnedFd::from_raw_fd(fd),
            })
        }
    }
    /// true means reload preferences; clock steps also wake and recompute the time.
    pub fn wait(&self, now: i64, seconds: bool) -> io::Result<bool> {
        let mut spec: libc::itimerspec = unsafe { std::mem::zeroed() };
        spec.it_value.tv_sec = next_boundary(now, seconds)
            .try_into()
            .map_err(|_| io::Error::other("clock out of range"))?;
        if unsafe {
            libc::timerfd_settime(
                self.timer.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                &spec,
                std::ptr::null_mut(),
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECANCELED) {
                // A clock step can make rearming report ECANCELED even though
                // the timer was armed. Recompute the time/deadline immediately.
                return Ok(false);
            }
            return Err(error);
        }
        let mut fds = [
            libc::pollfd {
                fd: self.timer.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.reload.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let reload = fds[1].revents & libc::POLLIN != 0;
            if reload {
                let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
                while unsafe {
                    libc::read(
                        self.reload.as_raw_fd(),
                        (&mut info as *mut libc::signalfd_siginfo).cast(),
                        std::mem::size_of_val(&info),
                    )
                } > 0
                {}
            }
            if fds[0].revents & libc::POLLIN != 0 {
                let mut count = 0u64;
                // ECANCELED on a clock step is a wakeup, too.
                unsafe {
                    libc::read(self.timer.as_raw_fd(), (&mut count as *mut u64).cast(), 8);
                }
            }
            if reload || fds[0].revents & libc::POLLIN != 0 {
                return Ok(reload);
            }
            return Err(io::Error::other("clock poll failed"));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_align_to_clock_boundaries() {
        assert_eq!(next_boundary(121, false), 180);
        assert_eq!(next_boundary(179, false), 180);
        assert_eq!(next_boundary(180, false), 240);
        assert_eq!(next_boundary(121, true), 122);
    }
    #[test]
    fn reload_interrupts_a_distant_clock_deadline() {
        let mut old = unsafe { std::mem::zeroed() };
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut old);
        }
        let clock = ClockWait::new().unwrap();
        unsafe {
            assert_eq!(libc::pthread_kill(libc::pthread_self(), libc::SIGHUP), 0);
        }
        assert!(clock
            .wait(chrono::Utc::now().timestamp() + 3600, false)
            .unwrap());
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        }
    }
}
