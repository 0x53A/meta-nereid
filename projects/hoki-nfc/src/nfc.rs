//! Direct Linux NFC reader transport. One persistent netlink socket owns polling.
#[path = "nfc_debug.rs"]
mod debug;
pub use debug::DebugLog;

use std::{
    cell::RefCell,
    collections::VecDeque,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
pub const NFC_PROTO_JEWEL: u32 = 1;
pub const NFC_PROTO_MIFARE: u32 = 2;
pub const NFC_PROTO_FELICA: u32 = 3;
pub const NFC_PROTO_ISO14443: u32 = 4;
pub const NFC_PROTO_ISO14443_B: u32 = 6;
// PN553 reader mode only; NFC-DEP needs unsupported general-byte setup.
const READER_PROTOCOLS: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 6);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
fn u16n(b: &[u8]) -> u16 {
    u16::from_ne_bytes(b[..2].try_into().unwrap())
}
fn u32n(b: &[u8]) -> u32 {
    u32::from_ne_bytes(b[..4].try_into().unwrap())
}
fn align(n: usize) -> usize {
    (n + 3) & !3
}
fn attrs(mut b: &[u8]) -> io::Result<Vec<(u16, &[u8])>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 4 {
            return Err(invalid("short attribute header"));
        }
        let n = u16n(b) as usize;
        if n < 4 || n > b.len() {
            return Err(invalid("invalid attribute length"));
        }
        out.push((u16n(&b[2..]) & 0x3fff, &b[4..n]));
        if n == b.len() {
            break;
        }
        if align(n) > b.len() {
            return Err(invalid("short attribute padding"));
        }
        b = &b[align(n)..];
    }
    Ok(out)
}
fn attr<'a>(a: &[(u16, &'a [u8])], key: u16) -> io::Result<Option<&'a [u8]>> {
    let mut found = a.iter().filter(|(k, _)| *k == key);
    let v = found.next().map(|(_, v)| *v);
    if found.next().is_some() {
        return Err(invalid("duplicate attribute"));
    }
    Ok(v)
}
fn number(a: &[(u16, &[u8])], key: u16) -> io::Result<u32> {
    let v = attr(a, key)?.ok_or_else(|| invalid("missing integer attribute"))?;
    if v.len() != 4 {
        return Err(invalid("invalid integer attribute"));
    }
    Ok(u32n(v))
}
fn add_attr(b: &mut Vec<u8>, key: u16, data: &[u8]) {
    b.extend_from_slice(&((data.len() + 4) as u16).to_ne_bytes());
    b.extend_from_slice(&key.to_ne_bytes());
    b.extend_from_slice(data);
    b.resize(align(b.len()), 0);
}
#[derive(Debug)]
struct Message {
    kind: u16,
    flags: u16,
    seq: u32,
    body: Vec<u8>,
}
fn messages(mut b: &[u8]) -> io::Result<Vec<Message>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 16 {
            return Err(invalid("short netlink header"));
        }
        let n = u32n(b) as usize;
        if n < 16 || n > b.len() {
            return Err(invalid("invalid netlink length"));
        }
        out.push(Message {
            kind: u16n(&b[4..]),
            flags: u16n(&b[6..]),
            seq: u32n(&b[8..]),
            body: b[16..n].to_vec(),
        });
        if n == b.len() {
            break;
        }
        if align(n) > b.len() {
            return Err(invalid("short netlink padding"));
        }
        b = &b[align(n)..];
    }
    Ok(out)
}
fn check_status(b: &[u8]) -> io::Result<()> {
    if b.len() < 4 {
        return Err(invalid("short netlink status"));
    }
    let status = i32::from_ne_bytes(b[..4].try_into().unwrap());
    if status == 0 {
        Ok(())
    } else if status < 0 && status != i32::MIN {
        Err(io::Error::from_raw_os_error(-status))
    } else {
        Err(invalid("invalid netlink status"))
    }
}
fn wait(fd: RawFd, events: i16, deadline: Instant, running: Option<&AtomicBool>) -> io::Result<()> {
    loop {
        if running.is_some_and(|v| !v.load(Ordering::Relaxed)) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "NFC operation timed out",
            ));
        }
        let mut p = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let ms = remaining.as_millis().clamp(1, 50) as i32;
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if p.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if p.revents & libc::POLLERR != 0 {
            // AF_NFC reports asynchronous driver errors through sk_err.
            // Preserve that errno instead of replacing it with BrokenPipe.
            let mut error: libc::c_int = 0;
            let mut len = std::mem::size_of_val(&error) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut error as *mut _ as _,
                    &mut len,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            return Err(if error != 0 {
                io::Error::from_raw_os_error(error)
            } else {
                io::Error::new(io::ErrorKind::BrokenPipe, "NFC socket reported an error")
            });
        }
        if p.revents & events != 0 {
            return Ok(());
        }
        if p.revents & libc::POLLHUP != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "NFC socket closed",
            ));
        }
    }
}
fn receive(fd: RawFd) -> io::Result<Vec<u8>> {
    let mut b = vec![0; 65536];
    let mut iov = libc::iovec {
        iov_base: b.as_mut_ptr() as _,
        iov_len: b.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    // Linux 4.14 AF_NFC returns only the copied length, even with input
    // MSG_TRUNC. Inspect recvmsg's output flags to detect a shortened packet.
    let n = unsafe { libc::recvmsg(fd, &mut message, libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n as usize > b.len() || message.msg_flags & libc::MSG_TRUNC != 0 {
        return Err(invalid("truncated NFC packet"));
    }
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty NFC packet",
        ));
    }
    b.truncate(n as usize);
    Ok(b)
}
fn socket(domain: i32, kind: i32, protocol: i32) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::socket(
            domain,
            kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            protocol,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
pub struct NfcTarget {
    pub idx: u32,
    pub protocol: u32,
    pub sens_res: u16,
    pub sel_res: u8,
    pub nfcid1: Vec<u8>,
}

impl NfcTarget {
    pub fn tag_type_str(&self) -> String {
        match self.protocol {
            NFC_PROTO_ISO14443_B => return "ISO-DEP (Type B)".into(),
            NFC_PROTO_FELICA => return "FeliCa".into(),
            NFC_PROTO_JEWEL => return "Jewel (Type 1)".into(),
            _ => (),
        }
        match self.sel_res {
            0x00 => "NTAG / Ultralight".into(),
            0x08 => "MIFARE Classic 1K".into(),
            0x09 => "MIFARE Mini".into(),
            0x18 => "MIFARE Classic 4K".into(),
            0x20 => "ISO-DEP (Type 4)".into(),
            0x28 => "JCOP".into(),
            0x60 => "NFC-DEP".into(),
            _ => format!("NFC-A (sel_res {:02X})", self.sel_res),
        }
    }

    pub fn uid_hex(&self) -> String {
        self.nfcid1
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":")
    }
}

pub struct NfcNetlink {
    fd: OwnedFd,
    family: u16,
    seq: u32,
    events: VecDeque<Message>,
    adapter: Option<u32>,
    powered: Option<u32>,
    debug: Option<DebugLog>,
}
impl NfcNetlink {
    pub fn open() -> io::Result<Self> {
        Self::open_with_debug(None)
    }
    pub fn open_with_debug(debug: Option<DebugLog>) -> io::Result<Self> {
        let fd = socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_GENERIC)?;
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &address as *const _ as _,
                std::mem::size_of_val(&address) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // Send requests to the kernel (port ID zero).
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &address as *const _ as _,
                std::mem::size_of_val(&address) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut nl = Self {
            fd,
            family: 0,
            seq: 0,
            events: VecDeque::new(),
            adapter: None,
            powered: None,
            debug,
        };
        let mut a = Vec::new();
        add_attr(&mut a, 2, b"nfc\0");
        let reply = nl.request(0x10, 3, &a, false)?;
        let m = reply.first().ok_or_else(|| invalid("missing NFC family"))?;
        let a = attrs(&m.body[4..])?;
        let id = attr(&a, 1)?.ok_or_else(|| invalid("missing family ID"))?;
        if id.len() != 2 {
            return Err(invalid("invalid family ID"));
        }
        nl.family = u16n(id);
        let groups = attr(&a, 7)?.ok_or_else(|| invalid("missing NFC multicast groups"))?;
        let mut group = None;
        for (_, entry) in attrs(groups)? {
            let a = attrs(entry)?;
            if attr(&a, 1)? == Some(b"events\0".as_slice()) {
                group = Some(number(&a, 2)?);
            }
        }
        let group = group.ok_or_else(|| invalid("missing NFC events group"))?;
        if unsafe {
            libc::setsockopt(
                nl.fd.as_raw_fd(),
                libc::SOL_NETLINK,
                libc::NETLINK_ADD_MEMBERSHIP,
                &group as *const _ as _,
                4,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(nl)
    }
    fn request(&mut self, family: u16, cmd: u8, a: &[u8], dump: bool) -> io::Result<Vec<Message>> {
        self.seq = self.seq.wrapping_add(1).max(1);
        let seq = self.seq;
        let mut b = vec![0; 20];
        b[4..6].copy_from_slice(&family.to_ne_bytes());
        let flags = (libc::NLM_F_REQUEST
            | if dump {
                libc::NLM_F_DUMP
            } else {
                libc::NLM_F_ACK
            }) as u16;
        b[6..8].copy_from_slice(&flags.to_ne_bytes());
        b[8..12].copy_from_slice(&seq.to_ne_bytes());
        b[16] = cmd;
        b[17] = 1;
        b.extend_from_slice(a);
        let len = b.len() as u32;
        b[..4].copy_from_slice(&len.to_ne_bytes());
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        wait(self.fd.as_raw_fd(), libc::POLLOUT, deadline, None)?;
        if let Some(log) = &self.debug {
            log.bytes("NETLINK TX", &b);
        }
        let n = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                b.as_ptr() as _,
                b.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != b.len() {
            return Err(invalid("short netlink send"));
        }
        let mut result = Vec::new();
        loop {
            wait(self.fd.as_raw_fd(), libc::POLLIN, deadline, None)?;
            let mut done = false;
            for m in messages(&self.receive()?)? {
                if m.seq != seq {
                    self.event(m)?;
                    continue;
                }
                if m.flags & 0x10 != 0 {
                    return Err(invalid("interrupted netlink dump"));
                }
                match m.kind {
                    2 => {
                        check_status(&m.body)?;
                        if !dump {
                            done = true;
                        }
                    }
                    3 => {
                        if !m.body.is_empty() {
                            check_status(&m.body)?;
                        }
                        if !dump {
                            return Err(invalid("unexpected dump completion"));
                        }
                        done = true;
                    }
                    k if k == family => {
                        if m.body.len() < 4 {
                            return Err(invalid("short generic netlink header"));
                        }
                        result.push(m);
                    }
                    _ => return Err(invalid("unexpected netlink response")),
                }
            }
            if done {
                return Ok(result);
            }
        }
    }
    fn receive(&self) -> io::Result<Vec<u8>> {
        let result = receive(self.fd.as_raw_fd());
        if let Some(log) = &self.debug {
            match &result {
                Ok(data) => log.bytes("NETLINK RX", data),
                Err(e) => log.text(&format!("NETLINK RX ERROR {e}")),
            }
        }
        result
    }
    fn event(&mut self, m: Message) -> io::Result<()> {
        if m.seq == 0 && m.kind == self.family && m.body.len() >= 4 && m.body[0] == 9 {
            if self.events.len() >= 64 {
                return Err(invalid("NFC event queue overflow"));
            }
            self.events.push_back(m);
        }
        Ok(())
    }
    fn command(&mut self, index: u32, cmd: u8) -> io::Result<()> {
        let mut a = Vec::new();
        add_attr(&mut a, 1, &index.to_ne_bytes());
        self.request(self.family, cmd, &a, false).map(|_| ())
    }
    pub fn is_powered(&mut self, index: u32) -> io::Result<bool> {
        let mut a = Vec::new();
        add_attr(&mut a, 1, &index.to_ne_bytes());
        let reply = self.request(self.family, 1, &a, false)?;
        let m = reply
            .first()
            .ok_or_else(|| invalid("missing device response"))?;
        if m.body[0] != 1 {
            return Err(invalid("unexpected device command"));
        }
        let a = attrs(&m.body[4..])?;
        if number(&a, 1)? != index {
            return Err(invalid("wrong device response"));
        }
        match attr(&a, 12)? {
            Some([0]) => Ok(false),
            Some([1]) => Ok(true),
            _ => Err(invalid("invalid powered attribute")),
        }
    }
    pub fn dev_up(&mut self, index: u32) -> io::Result<()> {
        match self.command(index, 2) {
            Ok(()) => {
                self.powered = Some(index);
                Ok(())
            }
            Err(e) if e.raw_os_error() == Some(libc::EALREADY) => Ok(()),
            Err(e) => Err(e),
        }
    }
    /// Kept for CLI compatibility. Initialization is acknowledged, never a blind power cycle.
    pub fn reset(&mut self, index: u32) -> io::Result<()> {
        self.dev_up(index)
    }
    pub fn start_poll(&mut self, index: u32) -> io::Result<()> {
        self.events.clear();
        let mut a = Vec::new();
        add_attr(&mut a, 1, &index.to_ne_bytes());
        add_attr(&mut a, 3, &READER_PROTOCOLS.to_ne_bytes());
        // Keep the same netlink port until stop/close: the kernel records poll ownership.
        self.request(self.family, 6, &a, false)?;
        self.adapter = Some(index);
        Ok(())
    }
    pub fn finish(&mut self) -> io::Result<()> {
        let stop = if let Some(index) = self.adapter {
            self.stop_poll(index)
        } else {
            Ok(())
        };
        let power = if let Some(index) = self.powered {
            // Closing AF_NFC can leave a request in flight until its completion
            // callback releases the target. The NCI data timeout is 700 ms.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match self.command(index, 3) {
                    Ok(()) => {
                        self.powered = None;
                        break Ok(());
                    }
                    Err(e) if e.raw_os_error() == Some(libc::EALREADY) => {
                        self.powered = None;
                        break Ok(());
                    }
                    Err(e)
                        if e.raw_os_error() == Some(libc::EBUSY) && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => break Err(e),
                }
            }
        } else {
            Ok(())
        };
        stop.and(power)
    }
    pub fn stop_poll(&mut self, index: u32) -> io::Result<()> {
        let result = self.command(index, 7);
        self.adapter = None;
        match result {
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            r => r,
        }
    }
    pub fn wait_target(&mut self, timeout_ms: i32) -> io::Result<Option<NfcTarget>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        loop {
            while let Some(m) = self.events.pop_front() {
                let index = number(&attrs(&m.body[4..])?, 1)?;
                if Some(index) != self.adapter {
                    continue;
                }
                let mut a = Vec::new();
                add_attr(&mut a, 1, &index.to_ne_bytes());
                let replies = self.request(self.family, 8, &a, true)?;
                for m in replies {
                    if m.body[0] != 8 {
                        return Err(invalid("unexpected target command"));
                    }
                    if let Some(target) = parse_target(&m.body[4..])? {
                        return Ok(Some(target));
                    }
                }
                return Ok(None);
            }
            match wait(self.fd.as_raw_fd(), libc::POLLIN, deadline, None) {
                Err(e) if e.kind() == io::ErrorKind::TimedOut => return Ok(None),
                r => r?,
            }
            for m in messages(&self.receive()?)? {
                self.event(m)?;
            }
        }
    }
}
fn parse_target(data: &[u8]) -> io::Result<Option<NfcTarget>> {
    let a = attrs(data)?;
    let protocols = number(&a, 3)?;
    let Some(protocol) = [4, 6, 2, 3, 1]
        .into_iter()
        .find(|p| protocols & (1 << p) != 0)
    else {
        return Ok(None);
    };
    let sens = attr(&a, 5)?.unwrap_or(&[0, 0]);
    let sel = attr(&a, 6)?.unwrap_or(&[0]);
    let uid = attr(&a, 7)?.unwrap_or(&[]);
    if sens.len() != 2 || sel.len() != 1 || uid.len() > 10 {
        return Err(invalid("invalid target attributes"));
    }
    Ok(Some(NfcTarget {
        idx: number(&a, 4)?,
        protocol,
        sens_res: u16n(sens),
        sel_res: sel[0],
        nfcid1: uid.to_vec(),
    }))
}

impl Drop for NfcNetlink {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            eprintln!("NFC cleanup failed: {e}");
        }
        // OwnedFd closes even on cleanup errors; kernel release also stops owned polling.
    }
}
#[repr(C)]
struct SockaddrNfc {
    family: u16,
    index: u32,
    target: u32,
    protocol: u32,
}
pub struct NfcRawSock {
    fd: RefCell<Option<OwnedFd>>,
    running: Option<Arc<AtomicBool>>,
    debug: Option<DebugLog>,
}
impl NfcRawSock {
    pub fn connect(index: u32, target: u32, protocol: u32) -> io::Result<Self> {
        Self::connect_with_debug(index, target, protocol, None)
    }
    pub fn connect_with_debug(
        index: u32,
        target: u32,
        protocol: u32,
        debug: Option<DebugLog>,
    ) -> io::Result<Self> {
        if let Some(log) = &debug {
            log.text(&format!(
                "CONNECT adapter={index} target={target} protocol={protocol}"
            ));
        }
        let fd = socket(39, libc::SOCK_SEQPACKET, 0)?;
        let address = SockaddrNfc {
            family: 39,
            index,
            target,
            protocol,
        };
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &address as *const _ as _,
                std::mem::size_of_val(&address) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd: RefCell::new(Some(fd)),
            running: None,
            debug,
        })
    }
    pub fn is_connected(&self) -> bool {
        self.fd.borrow().is_some()
    }
    pub fn set_running(&mut self, running: Arc<AtomicBool>) {
        self.running = Some(running);
    }
    pub fn transceive(&self, cmd: &[u8], timeout_ms: i32) -> io::Result<Vec<u8>> {
        let mut stage = "validate";
        let result = (|| {
            let guard = self.fd.borrow();
            let fd = guard
                .as_ref()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotConnected,
                        "NFC exchange invalidated; reconnect required",
                    )
                })?
                .as_raw_fd();
            if cmd.is_empty() || timeout_ms <= 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty command or invalid timeout",
                ));
            }
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            stage = "wait-send";
            wait(fd, libc::POLLOUT, deadline, self.running.as_deref())?;
            stage = "send";
            if let Some(log) = &self.debug {
                log.bytes("NFC TX", cmd);
            }
            let n = unsafe {
                libc::send(
                    fd,
                    cmd.as_ptr() as _,
                    cmd.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n as usize != cmd.len() {
                return Err(invalid("short NFC send"));
            }
            stage = "wait-receive";
            wait(fd, libc::POLLIN, deadline, self.running.as_deref())?;
            stage = "receive";
            let data = receive(fd)?;
            if let Some(log) = &self.debug {
                log.bytes("NFC RX (kernel status prefix)", &data);
            }
            decode_raw(data)
        })();
        // A late unnumbered response must never satisfy a later command.
        if let Err(ref error) = result {
            if let Some(log) = &self.debug {
                log.text(&format!(
                    "NFC ERROR stage={stage} kind={:?} errno={:?} error={error}",
                    error.kind(),
                    error.raw_os_error()
                ));
            }
            // Raw traffic goes only to the explicitly enabled private log.
            eprintln!(
                "NFC exchange failed: stage={stage}, kind={:?}, errno={:?}, error={error}",
                error.kind(),
                error.raw_os_error()
            );
            self.fd.borrow_mut().take();
        }
        result
    }
}
fn decode_raw(mut b: Vec<u8>) -> io::Result<Vec<u8>> {
    match b.first() {
        Some(0) => {
            b.remove(0);
            Ok(b)
        }
        Some(_) => Err(invalid("NFC kernel response error")),
        None => Err(invalid("missing NFC status byte")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (NfcRawSock, OwnedFd) {
        let mut fds = [0; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        (
            NfcRawSock {
                fd: RefCell::new(Some(unsafe { OwnedFd::from_raw_fd(fds[0]) })),
                running: None,
                debug: None,
            },
            unsafe { OwnedFd::from_raw_fd(fds[1]) },
        )
    }
    #[test]
    fn malformed_attributes() {
        for b in [
            &[1][..],
            &[3, 0, 1, 0],
            &[8, 0, 1, 0, 0],
            &[5, 0, 1, 0, 0, 0],
        ] {
            assert!(attrs(b).is_err());
        }
        let mut b = Vec::new();
        add_attr(&mut b, 1 | 0x8000, &9u32.to_ne_bytes());
        assert_eq!(number(&attrs(&b).unwrap(), 1).unwrap(), 9);
        add_attr(&mut b, 1, &8u32.to_ne_bytes());
        assert!(number(&attrs(&b).unwrap(), 1).is_err());
    }
    #[test]
    fn multipart_messages() {
        let mut b = vec![0; 16];
        b[..4].copy_from_slice(&16u32.to_ne_bytes());
        assert_eq!(messages(&[b.clone(), b.clone()].concat()).unwrap().len(), 2);
        for n in 1..16 {
            assert!(messages(&b[..n]).is_err());
        }
        b[..4].copy_from_slice(&17u32.to_ne_bytes());
        assert!(messages(&b).is_err());
    }
    #[test]
    fn kernel_errors() {
        assert_eq!(
            check_status(&(-libc::EBUSY).to_ne_bytes())
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBUSY)
        );
        assert!(check_status(&[]).is_err());
        assert!(check_status(&0i32.to_ne_bytes()).is_ok());
    }
    #[test]
    fn response_status() {
        assert_eq!(decode_raw(vec![0]).unwrap(), Vec::<u8>::new());
        assert_eq!(decode_raw(vec![0, 9, 8]).unwrap(), vec![9, 8]);
        assert!(decode_raw(vec![1, 9]).is_err());
        assert!(decode_raw(vec![]).is_err());
    }
    #[test]
    fn timeout_invalidates_socket() {
        let (sock, _peer) = pair();
        assert_eq!(
            sock.transceive(&[1], 10).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(sock.fd.borrow().is_none());
        assert_eq!(
            sock.transceive(&[2], 10).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }
    #[test]
    fn cancelled_before_send() {
        let (mut sock, peer) = pair();
        sock.set_running(Arc::new(AtomicBool::new(false)));
        assert_eq!(
            sock.transceive(&[1], 1000).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(receive(peer.as_raw_fd()).is_err());
    }
    #[test]
    fn packet_exchange() {
        let (sock, peer) = pair();
        let worker = std::thread::spawn(move || {
            wait(
                peer.as_raw_fd(),
                libc::POLLIN,
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .unwrap();
            assert_eq!(receive(peer.as_raw_fd()).unwrap(), vec![1, 2]);
            let reply = [0u8, 3, 4];
            assert_eq!(
                unsafe { libc::send(peer.as_raw_fd(), reply.as_ptr() as _, 3, libc::MSG_NOSIGNAL) },
                3
            );
        });
        assert_eq!(sock.transceive(&[1, 2], 1000).unwrap(), vec![3, 4]);
        worker.join().unwrap();
    }
    #[test]
    fn truncation() {
        let (sock, peer) = pair();
        let packet = vec![0u8; 65537];
        assert_eq!(
            unsafe {
                libc::send(
                    peer.as_raw_fd(),
                    packet.as_ptr() as _,
                    packet.len(),
                    libc::MSG_NOSIGNAL,
                )
            },
            packet.len() as isize
        );
        let fd = sock.fd.borrow();
        assert_eq!(
            receive(fd.as_ref().unwrap().as_raw_fd())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fn packet(kind: u16, seq: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 16];
        b[..4].copy_from_slice(&((16 + body.len()) as u32).to_ne_bytes());
        b[4..6].copy_from_slice(&kind.to_ne_bytes());
        b[8..12].copy_from_slice(&seq.to_ne_bytes());
        b.extend_from_slice(body);
        b.resize(align(b.len()), 0);
        b
    }
    fn control_pair() -> (NfcNetlink, OwnedFd) {
        let (raw, peer) = pair();
        let fd = raw.fd.borrow_mut().take().unwrap();
        (
            NfcNetlink {
                fd,
                family: 42,
                seq: 0,
                events: VecDeque::new(),
                adapter: None,
                powered: None,
                debug: None,
            },
            peer,
        )
    }
    fn respond(peer: OwnedFd, data: Vec<u8>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            wait(
                peer.as_raw_fd(),
                libc::POLLIN,
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .unwrap();
            let request = receive(peer.as_raw_fd()).unwrap();
            assert_eq!(u32n(&request[8..]), 1);
            assert_eq!(
                unsafe {
                    libc::send(
                        peer.as_raw_fd(),
                        data.as_ptr() as _,
                        data.len(),
                        libc::MSG_NOSIGNAL,
                    )
                },
                data.len() as isize
            );
        })
    }
    #[test]
    fn events_and_stale_ack_do_not_complete_request() {
        let (mut nl, peer) = control_pair();
        let worker = respond(
            peer,
            [
                packet(2, 99, &0i32.to_ne_bytes()),
                packet(42, 0, &[9, 1, 0, 0]),
                packet(2, 1, &(-libc::EBUSY).to_ne_bytes()),
            ]
            .concat(),
        );
        assert_eq!(
            nl.command(0, 6).unwrap_err().raw_os_error(),
            Some(libc::EBUSY)
        );
        assert_eq!(nl.events.len(), 1);
        worker.join().unwrap();
    }
    #[test]
    fn dump_collects_every_message_before_done() {
        let (mut nl, peer) = control_pair();
        let worker = respond(
            peer,
            [
                packet(42, 1, &[8, 1, 0, 0]),
                packet(42, 1, &[8, 1, 0, 0]),
                packet(3, 1, &0i32.to_ne_bytes()),
            ]
            .concat(),
        );
        assert_eq!(nl.request(42, 8, &[], true).unwrap().len(), 2);
        worker.join().unwrap();
    }
    #[test]
    fn target_uses_linux_uapi_attribute_numbers() {
        let mut a = Vec::new();
        add_attr(&mut a, 3, &(1u32 << 2).to_ne_bytes());
        add_attr(&mut a, 4, &17u32.to_ne_bytes());
        add_attr(&mut a, 5, &0x4400u16.to_ne_bytes());
        add_attr(&mut a, 6, &[8]);
        add_attr(&mut a, 7, &[1, 2, 3, 4]);
        let t = parse_target(&a).unwrap().unwrap();
        assert_eq!(t.idx, 17);
        assert_eq!(t.sens_res, 0x4400);
        assert_eq!(t.sel_res, 8);
        assert_eq!(t.nfcid1, vec![1, 2, 3, 4]);
        assert_eq!(t.tag_type_str(), "MIFARE Classic 1K");
    }
    #[test]
    fn asynchronous_socket_error_keeps_errno() {
        // Reserve a loopback TCP port without listening, so connect must fail.
        let reserved = socket(libc::AF_INET, libc::SOCK_STREAM, 0).unwrap();
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_family = libc::AF_INET as u16;
        address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
        let mut len = std::mem::size_of_val(&address) as libc::socklen_t;
        assert_eq!(
            unsafe { libc::bind(reserved.as_raw_fd(), &address as *const _ as _, len) },
            0
        );
        assert_eq!(
            unsafe {
                libc::getsockname(reserved.as_raw_fd(), &mut address as *mut _ as _, &mut len)
            },
            0
        );
        let client = socket(libc::AF_INET, libc::SOCK_STREAM, 0).unwrap();
        assert_eq!(
            unsafe { libc::connect(client.as_raw_fd(), &address as *const _ as _, len) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINPROGRESS)
        );
        let error = wait(
            client.as_raw_fd(),
            libc::POLLOUT,
            Instant::now() + Duration::from_secs(1),
            None,
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ECONNREFUSED));
    }
    #[test]
    fn cleanup_waits_for_target_release() {
        let (mut nl, peer) = control_pair();
        nl.powered = Some(0);
        let worker = std::thread::spawn(move || {
            for status in [-libc::EBUSY, 0] {
                wait(
                    peer.as_raw_fd(),
                    libc::POLLIN,
                    Instant::now() + Duration::from_secs(1),
                    None,
                )
                .unwrap();
                let request = receive(peer.as_raw_fd()).unwrap();
                assert_eq!(request[16], 3);
                let response = packet(2, u32n(&request[8..]), &status.to_ne_bytes());
                assert_eq!(
                    unsafe {
                        libc::send(
                            peer.as_raw_fd(),
                            response.as_ptr() as _,
                            response.len(),
                            libc::MSG_NOSIGNAL,
                        )
                    },
                    response.len() as isize
                );
            }
        });
        nl.finish().unwrap();
        assert!(nl.powered.is_none());
        worker.join().unwrap();
    }
}
