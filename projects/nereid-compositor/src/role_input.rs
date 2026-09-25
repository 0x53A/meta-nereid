//! Bounded, nonblocking output to a managed role. Only poll writable while queued.
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

const LIMIT: usize = 16 * 1024;
pub struct RoleInput {
    fd: OwnedFd,
    pending: VecDeque<u8>,
}
impl RoleInput {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            pending: VecDeque::new(),
        })
    }
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn send(&mut self, message: &str) -> io::Result<()> {
        self.flush()?;
        if message
            .len()
            .saturating_add(1)
            .saturating_add(self.pending.len())
            > LIMIT
        {
            return Err(io::Error::other(
                "role stopped reading commands (queue full)",
            ));
        }
        self.pending.extend(message.bytes());
        self.pending.push_back(b'\n');
        self.flush()
    }
    pub fn flush(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            let bytes = self.pending.as_slices().0;
            let n = unsafe { libc::write(self.fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(()),
                    _ => return Err(e),
                }
            }
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            self.pending.drain(..n as usize);
        }
        Ok(())
    }
}
impl AsFd for RoleInput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    fn pipe() -> (std::fs::File, RoleInput) {
        let mut fds = [0; 2];
        assert_eq!(
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                RoleInput::new(OwnedFd::from_raw_fd(fds[1])).unwrap(),
            )
        }
    }
    #[test]
    fn full_pipe_is_bounded_and_does_not_block_dispatch() {
        let (_reader, mut input) = pipe();
        let start = std::time::Instant::now();
        let message = "x".repeat(1023);
        let mut full = false;
        for _ in 0..2048 {
            if input.send(&message).is_err() {
                full = true;
                break;
            }
        }
        assert!(full && input.pending());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        let wake = crate::wakeup::Wakeup::new().unwrap();
        wake.notify();
        let mut pfd = libc::pollfd {
            fd: wake.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 0) }, 1);
    }
    #[test]
    fn partial_writes_resume_in_order_and_broken_pipe_is_reported() {
        let (mut reader, mut input) = pipe();
        let message = "a".repeat(12000);
        input.send(&message).unwrap();
        let mut output = Vec::new();
        for _ in 0..10 {
            let mut bytes = [0; 4096];
            match reader.read(&mut bytes) {
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e) => assert_eq!(e.kind(), io::ErrorKind::WouldBlock),
            }
            input.flush().unwrap();
        }
        assert_eq!(output, format!("{message}\n").as_bytes());
        assert!(!input.pending());
        drop(reader);
        // Parallel process-spawn tests may briefly inherit this CLOEXEC read FD
        // between fork and exec. Wait for the kernel to confirm all readers closed.
        let mut pfd = libc::pollfd {
            fd: input.as_fd().as_raw_fd(),
            events: 0,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 1000) }, 1);
        assert_ne!(pfd.revents & libc::POLLERR, 0);
        assert_eq!(
            input.send("next").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
