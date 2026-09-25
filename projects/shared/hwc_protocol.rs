//! Framed Unix-stream transport shared by compositor and display proxy.
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

pub const MSG_FRAME: u8 = 0x01;
pub const MSG_POWER: u8 = 0x02;
pub const MSG_PING: u8 = 0x03;
pub const MSG_SYNC: u8 = 0x81;
pub const MSG_PONG: u8 = 0x82;
pub const MSG_INFO: u8 = 0x83;
const MAX_MESSAGE: usize = 65536;

fn message(kind: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    let len = payload
        .len()
        .checked_add(5)
        .filter(|n| *n <= MAX_MESSAGE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(&(len as u32).to_le_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

pub fn send_msg<W: Write>(w: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&message(kind, payload)?)
}

fn send_all(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    send_all_with_cancel(fd, bytes, None)
}

fn send_all_with_cancel(fd: RawFd, mut bytes: &[u8], cancel: Option<RawFd>) -> io::Result<()> {
    while !bytes.is_empty() {
        if cancel.is_some() {
            wait_ready(fd, libc::POLLOUT, cancel)?;
        }
        let flags = libc::MSG_NOSIGNAL
            | if cancel.is_some() {
                libc::MSG_DONTWAIT
            } else {
                0
            };
        let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), flags) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted
                || (cancel.is_some() && e.kind() == io::ErrorKind::WouldBlock)
            {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

pub fn send_raw(fd: RawFd, kind: u8, payload: &[u8]) -> io::Result<()> {
    send_all(fd, &message(kind, payload)?)
}

/// Wait without a periodic timeout. A readable cancellation FD takes priority.
pub fn wait_ready(fd: RawFd, events: i16, cancel: Option<RawFd>) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd,
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: cancel.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[1].revents != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "shutdown requested",
            ));
        }
        if fds[0].revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if fds[0].revents != 0 {
            return Ok(());
        }
    }
}

pub fn send_raw_cancellable(fd: RawFd, kind: u8, payload: &[u8], cancel: RawFd) -> io::Result<()> {
    send_all_with_cancel(fd, &message(kind, payload)?, Some(cancel))
}

pub fn send_fd(socket: RawFd, kind: u8, payload: &[u8], fd: RawFd) -> io::Result<()> {
    let bytes = message(kind, payload)?;
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut _,
        iov_len: bytes.len(),
    };
    // usize storage provides cmsghdr alignment on both ARM and the host.
    let mut control = [0usize; 8];
    let mut hdr: libc::msghdr = unsafe { std::mem::zeroed() };
    hdr.msg_iov = &mut iov;
    hdr.msg_iovlen = 1;
    hdr.msg_control = control.as_mut_ptr().cast();
    hdr.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&hdr);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), fd);
    }
    let n = loop {
        let n = unsafe { libc::sendmsg(socket, &hdr, libc::MSG_NOSIGNAL) };
        if n >= 0 {
            break n as usize;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    };
    if n == 0 {
        return Err(io::ErrorKind::WriteZero.into());
    }
    // Ancillary data is transferred by the first successful sendmsg only.
    send_all(socket, &bytes[n..])
}

fn receive_exact(
    socket: RawFd,
    mut bytes: &mut [u8],
    received: &mut Option<OwnedFd>,
    cancel: Option<RawFd>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let mut control = [0usize; 16];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut hdr: libc::msghdr = unsafe { std::mem::zeroed() };
        hdr.msg_iov = &mut iov;
        hdr.msg_iovlen = 1;
        hdr.msg_control = control.as_mut_ptr().cast();
        hdr.msg_controllen = std::mem::size_of_val(&control);
        if cancel.is_some() {
            wait_ready(socket, libc::POLLIN, cancel)?;
        }
        let flags = libc::MSG_CMSG_CLOEXEC
            | if cancel.is_some() {
                libc::MSG_DONTWAIT
            } else {
                0
            };
        let n = unsafe { libc::recvmsg(socket, &mut hdr, flags) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted
                || (cancel.is_some() && e.kind() == io::ErrorKind::WouldBlock)
            {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let mut invalid = hdr.msg_flags & libc::MSG_CTRUNC != 0;
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&hdr);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let base = libc::CMSG_LEN(0) as usize;
                    let size = (*cmsg).cmsg_len as usize;
                    if size < base {
                        invalid = true;
                        break;
                    }
                    let count = (size - base) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        let raw =
                            std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>().add(i));
                        let owned = OwnedFd::from_raw_fd(raw);
                        if received.is_some() {
                            invalid = true;
                        } else {
                            *received = Some(owned);
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&hdr, cmsg);
            }
        }
        if invalid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected descriptor count",
            ));
        }
        bytes = &mut bytes[n as usize..];
    }
    Ok(())
}

pub fn recv_fd(socket: RawFd) -> io::Result<(u8, Vec<u8>, Option<OwnedFd>)> {
    recv_fd_with_cancel(socket, None)
}

pub fn recv_fd_with_cancel(
    socket: RawFd,
    cancel: Option<RawFd>,
) -> io::Result<(u8, Vec<u8>, Option<OwnedFd>)> {
    let mut fd = None;
    let mut header = [0u8; 4];
    // Never read past the current message, including when messages are coalesced.
    receive_exact(socket, &mut header, &mut fd, cancel)?;
    let len = u32::from_le_bytes(header) as usize;
    if !(5..=MAX_MESSAGE).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid message length",
        ));
    }
    let mut body = vec![0u8; len - 4];
    receive_exact(socket, &mut body, &mut fd, cancel)?;
    Ok((body[0], body[1..].to_vec(), fd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn power_then_frame_preserves_message_and_fd_boundaries() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        send_msg(&mut tx, MSG_POWER, &[2]).unwrap();
        send_fd(tx.as_raw_fd(), MSG_FRAME, &[1; 12], file.as_raw_fd()).unwrap();
        let (kind, data, fd) = recv_fd(rx.as_raw_fd()).unwrap();
        assert_eq!((kind, data), (MSG_POWER, vec![2]));
        assert!(fd.is_none());
        let (kind, data, fd) = recv_fd(rx.as_raw_fd()).unwrap();
        assert_eq!((kind, data), (MSG_FRAME, vec![1; 12]));
        let fd = fd.unwrap();
        assert_ne!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    #[test]
    fn fragmented_header_and_late_descriptor() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let worker = std::thread::spawn(move || {
            let file = std::fs::File::open("/dev/null").unwrap();
            tx.write_all(&[10, 0]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            tx.write_all(&[0, 0, MSG_FRAME]).unwrap();
            // Append a five-byte payload carrying the FD after the outer header.
            send_fd(tx.as_raw_fd(), MSG_PING, &[], file.as_raw_fd()).unwrap();
        });
        let (kind, payload, fd) = recv_fd(rx.as_raw_fd()).unwrap();
        assert_eq!(kind, MSG_FRAME);
        assert_eq!(payload, [5, 0, 0, 0, MSG_PING]);
        assert!(fd.is_some());
        worker.join().unwrap();
    }

    #[test]
    fn rejecting_extra_descriptors_closes_every_received_copy() {
        use std::io::Read;
        let (tx, rx) = UnixStream::pair().unwrap();
        let (sent, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let bytes = message(MSG_FRAME, &[]).unwrap();
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr() as *mut _,
            iov_len: bytes.len(),
        };
        let mut control = [0usize; 8];
        let mut hdr: libc::msghdr = unsafe { std::mem::zeroed() };
        hdr.msg_iov = &mut iov;
        hdr.msg_iovlen = 1;
        hdr.msg_control = control.as_mut_ptr().cast();
        hdr.msg_controllen = unsafe { libc::CMSG_SPACE(8) } as _;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&hdr);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(8) as _;
            let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
            std::ptr::write_unaligned(data, sent.as_raw_fd());
            std::ptr::write_unaligned(data.add(1), sent.as_raw_fd());
            assert_eq!(
                libc::sendmsg(tx.as_raw_fd(), &hdr, libc::MSG_NOSIGNAL),
                bytes.len() as isize
            );
        }
        drop(sent);
        assert_eq!(
            recv_fd(rx.as_raw_fd()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            peer.read(&mut [0]).unwrap(),
            0,
            "all transferred copies must be closed"
        );
    }

    #[test]
    fn cancellation_interrupts_idle_and_partial_receives() {
        for prefix in [&[][..], &[10, 0][..], &[10, 0, 0, 0, MSG_FRAME][..]] {
            let (mut tx, rx) = UnixStream::pair().unwrap();
            let (mut cancel_tx, cancel_rx) = UnixStream::pair().unwrap();
            tx.write_all(prefix).unwrap();
            let worker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                cancel_tx.write_all(&[1]).unwrap();
            });
            assert_eq!(
                recv_fd_with_cancel(rx.as_raw_fd(), Some(cancel_rx.as_raw_fd()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Interrupted
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn cancellation_interrupts_a_blocked_response_write() {
        let (tx, _rx) = UnixStream::pair().unwrap();
        let size: libc::c_int = 4096;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    tx.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of_val(&size) as _,
                )
            },
            0
        );
        let (mut cancel_tx, cancel_rx) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            cancel_tx.write_all(&[1]).unwrap();
        });
        assert_eq!(
            send_raw_cancellable(
                tx.as_raw_fd(),
                MSG_INFO,
                &vec![0; MAX_MESSAGE - 5],
                cancel_rx.as_raw_fd()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        worker.join().unwrap();
    }

    #[test]
    fn coalesced_replies_and_invalid_lengths() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        tx.write_all(&[5, 0, 0, 0, MSG_SYNC, 5, 0, 0, 0, MSG_PONG])
            .unwrap();
        assert_eq!(recv_fd(rx.as_raw_fd()).unwrap().0, MSG_SYNC);
        assert_eq!(recv_fd(rx.as_raw_fd()).unwrap().0, MSG_PONG);
        tx.write_all(&u32::MAX.to_le_bytes()).unwrap();
        assert_eq!(
            recv_fd(rx.as_raw_fd()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
