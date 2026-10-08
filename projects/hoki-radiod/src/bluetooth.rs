//! Linux Bluetooth management channel. Wire layout follows the watch kernel's
//! include/net/bluetooth/mgmt.h. ConnMan remains the owner of power changes.
use std::{os::fd::{AsRawFd, FromRawFd, OwnedFd}, time::{Duration, Instant}};

pub const POWERED: u32 = 1;
const BREDR: u32 = 0x80;
const LE: u32 = 0x200;

#[repr(C)]
struct Address { family: libc::sa_family_t, device: u16, channel: u16 }
struct Management(OwnedFd);
impl Management {
    fn open() -> Result<Self, String> {
        // SAFETY: valid Linux Bluetooth constants; successful fd is owned once.
        let fd = unsafe { libc::socket(libc::AF_BLUETOOTH, libc::SOCK_RAW | libc::SOCK_CLOEXEC, 1) };
        if fd < 0 { return Err(std::io::Error::last_os_error().to_string()); }
        let socket = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        let address = Address { family: libc::AF_BLUETOOTH as _, device: 0xffff, channel: 3 };
        let result = unsafe { libc::bind(socket.0.as_raw_fd(), (&address as *const Address).cast(), std::mem::size_of::<Address>() as _) };
        if result < 0 { return Err(std::io::Error::last_os_error().to_string()); }
        Ok(socket)
    }
    fn call(&self, command: u16, index: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut packet = Vec::from(command.to_le_bytes());
        packet.extend(index.to_le_bytes());
        packet.extend((payload.len() as u16).to_le_bytes());
        packet.extend(payload);
        let fd = self.0.as_raw_fd();
        if unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), 0) } != packet.len() as isize {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis();
            if remaining == 0 { return Err("Bluetooth management timed out".into()); }
            let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut poll, 1, remaining as i32) };
            if ready < 0 { return Err(std::io::Error::last_os_error().to_string()); }
            if ready == 0 { continue; }
            let mut buffer = [0u8; 1024];
            let len = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            if len < 0 { return Err(std::io::Error::last_os_error().to_string()); }
            if let Some(result) = reply(&buffer[..len as usize], command, index) { return result; }
        }
    }
    fn index(&self) -> Result<u16, String> {
        let list = self.call(3, 0xffff, &[])?;
        if list.len() != 4 || u16::from_le_bytes([list[0], list[1]]) != 1 {
            return Err("Expected one Bluetooth controller".into());
        }
        Ok(u16::from_le_bytes([list[2], list[3]]))
    }
    fn settings(&self, index: u16) -> Result<u32, String> {
        let info = self.call(4, index, &[])?;
        let bytes: [u8; 4] = info.get(13..17).ok_or("Truncated Bluetooth information")?.try_into().unwrap();
        Ok(u32::from_le_bytes(bytes))
    }
}
fn reply(packet: &[u8], command: u16, index: u16) -> Option<Result<Vec<u8>, String>> {
    if packet.len() < 9 { return None; }
    let word = |offset| u16::from_le_bytes([packet[offset], packet[offset + 1]]);
    if word(2) != index || !matches!(word(0), 1 | 2) || word(6) != command { return None; }
    if usize::from(word(4)) + 6 != packet.len() { return Some(Err("Malformed Bluetooth reply".into())); }
    Some(if packet[8] != 0 { Err(format!("Bluetooth command {command:#06x} failed ({:#04x})", packet[8])) }
        else if word(0) == 2 { Err("Bluetooth command returned status without completion".into()) }
        else { Ok(packet[9..].to_vec()) })
}
pub fn settings() -> Result<u32, String> {
    let socket = Management::open()?;
    socket.settings(socket.index()?)
}
pub fn mode(settings: u32) -> &'static str {
    if settings & POWERED == 0 { "off" }
    else if settings & LE == 0 { "unknown" }
    else if settings & BREDR == 0 { "le" } else { "dual" }
}
pub fn configure(dual: bool) -> Result<(), String> {
    let socket = Management::open()?;
    let index = socket.index()?;
    if socket.settings(index)? & POWERED != 0 { return Err("Bluetooth must be powered off to change mode".into()); }
    socket.call(0x000d, index, &[1])?; // Set Low Energy
    socket.call(0x002a, index, &[u8::from(dual)])?; // Set BR/EDR
    let observed = socket.settings(index)?;
    if observed & LE == 0 || (observed & BREDR != 0) != dual { return Err("Bluetooth mode was not confirmed".into()); }
    Ok(())
}

/// Preserve other BlueZ settings and sections. BlueZ reapplies ControllerMode
/// on controller registration/boot, while ConnMan persists Powered separately.
fn config_with_mode(original: &str, mode: &str) -> String {
    let mut output = Vec::new();
    let mut general = false;
    let mut inserted = false;
    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if general && !inserted { output.push(format!("ControllerMode = {mode}")); inserted = true; }
            general = trimmed == "[General]";
        }
        if general && trimmed.split_once('=').is_some_and(|(key, _)| key.trim() == "ControllerMode") {
            if !inserted { output.push(format!("ControllerMode = {mode}")); inserted = true; }
        } else { output.push(line.to_owned()); }
    }
    if !inserted {
        if !general { output.push("[General]".into()); }
        output.push(format!("ControllerMode = {mode}"));
    }
    output.join("\n") + "\n"
}
pub fn persist(mode: &str) -> Result<(), String> {
    let path = "/etc/bluetooth/main.conf";
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let updated = config_with_mode(&original, mode);
    if updated == original { return Ok(()); }
    let temporary = "/etc/bluetooth/main.conf.hoki-new";
    std::fs::write(temporary, updated).map_err(|e|e.to_string())?;
    if let Ok(metadata) = std::fs::metadata(path) {
        std::fs::set_permissions(temporary, metadata.permissions()).map_err(|e|e.to_string())?;
    }
    std::fs::rename(temporary, path).map_err(|e|e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_modes_and_reply_errors() {
        assert_eq!(mode(LE | BREDR), "off");
        assert_eq!(mode(POWERED | LE), "le");
        assert_eq!(mode(POWERED | LE | BREDR), "dual");
        assert_eq!(mode(POWERED), "unknown");
        assert!(reply(&[1,0,0,0,3,0,42,0,12],42,0).unwrap().is_err());
        assert!(reply(&[1,0,1,0,3,0,42,0,0],42,0).is_none());
        assert_eq!(reply(&[1,0,0,0,4,0,42,0,0,7],42,0).unwrap().unwrap(),vec![7]);
    }
    #[test]
    fn persistence_preserves_sections_and_replaces_only_controller_mode() {
        let original = "[General]\nName = hoki\nControllerMode=dual\n[Policy]\nAutoEnable=true\n";
        assert_eq!(config_with_mode(original,"le"), "[General]\nName = hoki\nControllerMode = le\n[Policy]\nAutoEnable=true\n");
        assert_eq!(config_with_mode("", "dual"), "[General]\nControllerMode = dual\n");
        assert_eq!(config_with_mode("[General]\n[Policy]\n", "le"), "[General]\nControllerMode = le\n[Policy]\n");
    }
}
