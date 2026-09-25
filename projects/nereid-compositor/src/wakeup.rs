use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

pub struct Wakeup(OwnedFd);
impl Wakeup {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    pub fn notify(&self) {
        let value = 1u64;
        loop {
            let n = unsafe { libc::write(self.0.as_raw_fd(), (&value as *const u64).cast(), 8) };
            if n >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
    pub fn drain(&self) {
        drain(self.0.as_raw_fd());
    }
}
impl AsFd for Wakeup {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

pub struct ChildSignals(OwnedFd);
impl ChildSignals {
    /// Call on the main thread before creating worker threads, so all inherit the mask.
    pub fn new() -> io::Result<Self> {
        unsafe {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGCHLD);
            if libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) != 0 {
                return Err(io::Error::other("could not block SIGCHLD"));
            }
            let fd = libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(OwnedFd::from_raw_fd(fd)))
        }
    }
    pub fn drain(&self) {
        drain(self.0.as_raw_fd());
    }
}
impl AsFd for ChildSignals {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

fn drain(fd: i32) {
    let mut bytes = [0u8; 128];
    loop {
        let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n <= 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_notification_wakes_an_idle_poller() {
        let wake = std::sync::Arc::new(Wakeup::new().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = wake.clone();
        let worker = std::thread::spawn(move || {
            tx.send(42).unwrap();
            writer.notify();
        });
        let mut pfd = libc::pollfd {
            fd: wake.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 1000) }, 1);
        assert_eq!(rx.recv().unwrap(), 42);
        wake.drain();
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 0) }, 0);
        worker.join().unwrap();
    }
}
