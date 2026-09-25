mod nfc;

slint::include_modules!();

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

fn main() {
    std::env::set_var("SLINT_FULLSCREEN", "1");
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    let scanning = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicBool::new(false));
    let worker = std::rc::Rc::new(std::cell::RefCell::new(None::<thread::JoinHandle<()>>));

    // Start scan callback
    {
        let app_weak = app.as_weak();
        let scanning = scanning.clone();
        let busy = busy.clone();
        let worker = worker.clone();
        app.on_start_scan(move || {
            let app_weak = app_weak.clone();
            let scanning = scanning.clone();

            if busy.swap(true, Ordering::SeqCst) {
                return;
            }
            let debug = app_weak.upgrade().is_some_and(|app| app.get_debug());
            scanning.store(true, Ordering::Relaxed);

            app_weak
                .upgrade_in_event_loop(|app| {
                    app.set_scanning(true);
                    app.set_has_tag(false);
                    app.set_status("Polling...".into());
                    app.set_tag_type("".into());
                    app.set_tag_uid("".into());
                    app.set_tag_data("".into());
                })
                .ok();

            let busy = busy.clone();
            *worker.borrow_mut() = Some(thread::spawn(move || {
                nfc_scan_loop(app_weak.clone(), scanning.clone(), debug);
                let cancelled = !scanning.load(Ordering::Relaxed);
                scanning.store(false, Ordering::Relaxed);
                // Re-enable start only on the UI thread after all old callbacks and cleanup.
                app_weak.upgrade_in_event_loop(move |app| {
                    app.set_scanning(false);
                    if cancelled && !app.get_status().starts_with("Cleanup:") { app.set_status("Idle".into()); }
                    busy.store(false, Ordering::SeqCst);
                }).ok();
            }));
        });
    }

    // Stop scan callback
    {
        let app_weak = app.as_weak();
        let scanning = scanning.clone();
        app.on_stop_scan(move || {
            scanning.store(false, Ordering::Relaxed);
            app_weak
                .upgrade_in_event_loop(|app| {
                    app.set_status("Stopping...".into());
                })
                .ok();
        });
    }

    let result = app.run();
    scanning.store(false, Ordering::Relaxed);
    if let Some(worker) = worker.borrow_mut().take() { let _ = worker.join(); };
    result.unwrap();
}

fn nfc_scan_loop(app_weak: slint::Weak<App>, scanning: Arc<AtomicBool>, debug: bool) {
    let log = if debug {
        match nfc::DebugLog::create() {
            Ok(log) => {eprintln!("NFC debug log: {}",log.path().display());Some(log)},
            Err(e) => {let message=format!("Debug log: {e}");app_weak.upgrade_in_event_loop(move |app|app.set_status(message.into())).ok();return;}
        }
    } else {None};
    nfc_scan_session(app_weak.clone(), scanning, log.clone());
    if let Some(log)=log {
        log.text("SESSION END");
        if let Some(e)=log.error() {
            let message=format!("Cleanup: debug log failed: {e}");
            app_weak.upgrade_in_event_loop(move |app|app.set_status(message.into())).ok();
        }
    }
}

fn nfc_scan_session(app_weak: slint::Weak<App>, scanning: Arc<AtomicBool>, log: Option<nfc::DebugLog>) {
    let adapter_idx = 0u32;

    // Open NFC netlink and start polling
    let mut nl = match nfc::NfcNetlink::open_with_debug(log.clone()) {
        Ok(nl) => nl,
        Err(e) => {
            let msg = format!("Netlink: {}", e);
            app_weak
                .upgrade_in_event_loop(move |app| {
                    app.set_status(msg.into());
                    app.set_scanning(false);
                })
                .ok();
            return;
        }
    };

    // Bring up only if needed; preserve an already-powered adapter.
    if let Err(e) = nl.dev_up(adapter_idx) {
        let message = format!("Power: {}", e);
        app_weak.upgrade_in_event_loop(move |app| app.set_status(message.into())).ok();
        return;
    }
    if !scanning.load(Ordering::Relaxed) { return; }

    if let Err(e) = nl.start_poll(adapter_idx) {
        let msg = format!("Poll: {}", e);
        app_weak
            .upgrade_in_event_loop(move |app| {
                app.set_status(msg.into());
                app.set_scanning(false);
            })
            .ok();
        return;
    }

    // Wait for targets
    while scanning.load(Ordering::Relaxed) {
        match nl.wait_target(100) {
            Ok(Some(target)) => {
                if let Some(log)=&log {log.text(&format!("TARGET index={} protocol={} sens_res={:04x} sel_res={:02x} uid={}",target.idx,target.protocol,target.sens_res,target.sel_res,target.uid_hex()));}
                let tag_type = target.tag_type_str();
                let uid_str = target.uid_hex();

                let app_weak2 = app_weak.clone();
                let tag_type2 = tag_type.clone();
                let uid_str2 = uid_str.clone();
                app_weak
                    .upgrade_in_event_loop(move |app| {
                        app.set_has_tag(true);
                        app.set_status("Tag found".into());
                        app.set_tag_type(tag_type2.into());
                        app.set_tag_uid(uid_str2.into());
                    })
                    .ok();

                // Try to connect and read
                let transport_failed;
                match nfc::NfcRawSock::connect_with_debug(adapter_idx, target.idx, target.protocol, log.clone()) {
                    Ok(mut sock) => {
                        sock.set_running(scanning.clone());
                        let data = match target.protocol {
                            nfc::NFC_PROTO_ISO14443 => read_emv(&sock),
                            nfc::NFC_PROTO_MIFARE => read_mifare(&sock),
                            _ => "Read not supported".to_string(),
                        };
                        transport_failed = !sock.is_connected();
                        app_weak2
                            .upgrade_in_event_loop(move |app| {
                                app.set_tag_data(data.into());
                            })
                            .ok();
                    }
                    Err(e) => {
                        transport_failed = true;
                        if let Some(log)=&log {log.text(&format!("CONNECT ERROR {e}"));}
                        let msg = format!("Connect: {}", e);
                        app_weak2
                            .upgrade_in_event_loop(move |app| {
                                app.set_tag_data(msg.into());
                            })
                            .ok();
                    }
                }

                if transport_failed {
                    app_weak.upgrade_in_event_loop(|app|app.set_status("Read failed".into())).ok();
                    break;
                }

                // Keep tag info visible for 5 seconds
                for _ in 0..50 {
                    if !scanning.load(Ordering::Relaxed) { break; }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }

                // Restart polling if still scanning (keep tag info visible)
                if scanning.load(Ordering::Relaxed) {
                    if let Err(e) = nl.start_poll(adapter_idx) {
                        let message = format!("Poll: {}", e);
                        app_weak.upgrade_in_event_loop(move |app| app.set_status(message.into())).ok();
                        break;
                    }
                    app_weak
                        .upgrade_in_event_loop(|app| {
                            app.set_status("Scanning...".into());
                            // Don't clear has_tag — keep previous result visible
                        })
                        .ok();
                }
            }
            Ok(None) => continue, // timeout, loop
            Err(e) => {
                let message = format!("NFC: {}", e);
                app_weak.upgrade_in_event_loop(move |app| app.set_status(message.into())).ok();
                break;
            }
        }
    }

    if let Err(e) = nl.finish() {
        let message = format!("Cleanup: {}", e);
        app_weak.upgrade_in_event_loop(move |app| app.set_status(message.into())).ok();
    }
}

fn read_emv(sock: &nfc::NfcRawSock) -> String {
    let mut result = String::new();

    // SELECT PPSE
    let ppse = b"\x00\xA4\x04\x00\x0E2PAY.SYS.DDF01\x00";
    let resp = match sock.transceive(ppse, 3000) {
        Ok(r) => r,
        Err(e) => return format!("SELECT PPSE failed:\n{e}"),
    };
    if !check_sw(&resp) {
        return "PPSE rejected".into();
    }

    // Find AID (tag 4F)
    let aid = match tlv_find(&resp[..resp.len() - 2], 0x4F) {
        Some(v) => v.to_vec(),
        None => return "No payment AID".into(),
    };

    // App label (tag 50)
    if let Some(label) = tlv_find(&resp[..resp.len() - 2], 0x50) {
        result.push_str(&format!(
            "{}\n",
            String::from_utf8_lossy(label)
        ));
    }

    // SELECT payment app
    let mut sel = vec![0x00, 0xA4, 0x04, 0x00, aid.len() as u8];
    sel.extend_from_slice(&aid);
    sel.push(0x00);
    let resp = match sock.transceive(&sel, 3000) {
        Ok(r) => r,
        Err(e) => return result + &format!("SELECT AID failed: {e}"),
    };
    if !check_sw(&resp) {
        return result + "AID rejected";
    }

    // GET PROCESSING OPTIONS
    let gpo = b"\x80\xA8\x00\x00\x02\x83\x00\x00";
    if let Err(e) = sock.transceive(gpo, 3000) { return result + &format!("Read stopped: {e}"); }

    // READ RECORDs
    for sfi in 1..=4u8 {
        for rec in 1..=5u8 {
            let rr = [0x00, 0xB2, rec, (sfi << 3) | 0x04, 0x00];
            let resp = match sock.transceive(&rr, 2000) {
                Ok(r) => r,
                Err(e) => return result + &format!("\nRead stopped: {e}"),
            };
            if resp.len() < 2 || resp[resp.len() - 2] != 0x90 {
                continue;
            }

            let data = &resp[..resp.len() - 2];

            // PAN (tag 5A)
            if let Some(pan) = tlv_find(data, 0x5A) {
                result.push_str(&format!("PAN: {}\n", bcd_to_str(pan)));
            }
            // Cardholder name (tag 5F20)
            if let Some(name) = tlv_find(data, 0x5F20) {
                let s = String::from_utf8_lossy(name).trim().to_string();
                if !s.is_empty() {
                    result.push_str(&format!("Name: {}\n", s));
                }
            }
            // Expiry (tag 5F24)
            if let Some(exp) = tlv_find(data, 0x5F24) {
                if exp.len() >= 2 {
                    result.push_str(&format!("Exp: {:02X}/{:02X}\n", exp[1], exp[0]));
                }
            }
        }
    }

    if result.is_empty() {
        "No readable data".into()
    } else {
        result
    }
}

fn read_mifare(sock: &nfc::NfcRawSock) -> String {
    // Try reading block 0 (manufacturer block / UID)
    match sock.transceive(&[0x30, 0x00], 2000) {
        Ok(data) if data.len() >= 8 => {
            // T2T block 0: bytes 0-2 = UID part 1, byte 3 = BCC,
            // bytes 4-7 = UID part 2
            let uid = format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                data[0], data[1], data[2], data[4], data[5], data[6], data[7]);
            format!("UID: {}", uid)
        }
        Ok(data) => format!("Block 0: {} bytes", data.len()),
        Err(_) => "MIFARE Classic\n(auth keys required)".into(),
    }
}

fn check_sw(resp: &[u8]) -> bool {
    resp.len() >= 2 && resp[resp.len() - 2] == 0x90 && resp[resp.len() - 1] == 0x00
}

fn bcd_to_str(bcd: &[u8]) -> String {
    let mut s = String::new();
    for (i, b) in bcd.iter().enumerate() {
        let hi = (b >> 4) & 0x0f;
        let lo = b & 0x0f;
        if hi <= 9 {
            s.push((b'0' + hi) as char);
        }
        if lo <= 9 {
            s.push((b'0' + lo) as char);
        }
        if i % 2 == 1 && i < bcd.len() - 1 {
            s.push(' ');
        }
    }
    s
}

fn tlv_find(data: &[u8], tag: u16) -> Option<&[u8]> {
    let mut pos = 0;
    while pos < data.len() {
        // Parse tag
        let mut t = data[pos] as u16;
        pos += 1;
        if (t & 0x1f) == 0x1f && pos < data.len() {
            t = (t << 8) | data[pos] as u16;
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }

        // Parse length
        let mut l = data[pos] as usize;
        pos += 1;
        if l == 0x81 && pos < data.len() {
            l = data[pos] as usize;
            pos += 1;
        } else if l == 0x82 && pos + 1 < data.len() {
            l = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
            pos += 2;
        }

        if pos + l > data.len() {
            break;
        }

        if t == tag {
            return Some(&data[pos..pos + l]);
        }

        // Recurse into constructed tags
        if (t & 0x20) != 0 || ((t >> 8) & 0x20) != 0 {
            if let Some(found) = tlv_find(&data[pos..pos + l], tag) {
                return Some(found);
            }
        }

        pos += l;
    }
    None
}
