//! Versioned local power coordinator protocol. A connection owns its requests;
//! EOF releases them. Requests are acknowledged before protected work starts.
#![allow(dead_code)]
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub const SOCKET: &str = "/run/hoki-powerd/control.sock";
pub const MAX_LINE: u64 = 16384;

pub fn read_line(reader: &mut impl BufRead) -> io::Result<Value> {
    let mut line = String::new();
    reader.take(MAX_LINE + 1).read_line(&mut line)?;
    if line.len() as u64 > MAX_LINE || !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete or oversized message",
        ));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

pub struct Client(BufReader<UnixStream>);
impl Client {
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(SOCKET)
    }
    pub fn connect_to(path: &str) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self(BufReader::new(stream)))
    }
    pub fn request(&mut self, mut request: Value) -> io::Result<Value> {
        request["version"] = json!(1);
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_LINE {
            return Err(io::Error::other("message too large"));
        }
        self.0.get_mut().write_all(&bytes)?;
        let reply = read_line(&mut self.0)?;
        if reply["ok"] != true {
            return Err(io::Error::other(
                reply["error"].as_str().unwrap_or("coordinator failed"),
            ));
        }
        Ok(reply)
    }
    pub fn inhibit(&mut self, cpu: bool, display: bool, reason: &str) -> io::Result<()> {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match self.request(
                json!({"command":"inhibit", "cpu":cpu, "display":display, "reason":reason}),
            ) {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.to_string().contains("sleep transition in progress")
                        && std::time::Instant::now() < until =>
                {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(error),
            }
        }
    }
}

pub fn boottime() -> io::Result<f64> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(time.tv_sec as f64 + time.tv_nsec as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_partial_and_oversized_messages() {
        assert!(read_line(&mut &b"{}"[..]).is_err());
        assert!(read_line(&mut vec![b' '; MAX_LINE as usize + 1].as_slice()).is_err());
        let mut input = &b"{}\n{\"ok\":true}\n"[..];
        assert_eq!(read_line(&mut input).unwrap(), json!({}));
        assert_eq!(read_line(&mut input).unwrap(), json!({"ok":true}));
    }
}
