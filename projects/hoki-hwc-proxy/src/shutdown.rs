//! Block termination signals before HWC creates threads, and poll them as an FD.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct Shutdown {
    fd: OwnedFd,
}
impl Shutdown {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigaddset(&mut mask, libc::SIGINT);
            let err = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            if err != 0 {
                return Err(io::Error::from_raw_os_error(err));
            }
            let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd: OwnedFd::from_raw_fd(fd),
            })
        }
    }
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
    pub fn requested(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 0) > 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn termination_wakes_an_idle_listener() {
        // Target this test thread, not the process or other concurrently running tests.
        let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut old);
        }
        let shutdown = Shutdown::new().unwrap();
        let path = std::env::temp_dir().join(format!("hoki-shutdown-{}.sock", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        unsafe {
            assert_eq!(libc::pthread_kill(libc::pthread_self(), libc::SIGTERM), 0);
        }
        let result =
            crate::protocol::wait_ready(listener.as_raw_fd(), libc::POLLIN, Some(shutdown.fd()));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(shutdown.requested());
        // Consume the pending signal before restoring this thread's signal mask.
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        unsafe {
            assert_eq!(
                libc::read(
                    shutdown.fd(),
                    (&mut info as *mut libc::signalfd_siginfo).cast(),
                    std::mem::size_of_val(&info)
                ),
                std::mem::size_of_val(&info) as isize
            );
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        }
        std::fs::remove_file(path).unwrap();
    }
}
