// Quick CLI test for the NFC netlink interface
// Build: cargo build --release --target armv7-unknown-linux-gnueabihf --bin nfc_test

#[path = "../nfc.rs"]
mod nfc;

fn main() {
    eprintln!("Opening NFC netlink...");
    let mut nl = nfc::NfcNetlink::open().expect("Failed to open NFC netlink");

    eprintln!("Resetting...");
    nl.reset(0).expect("reset failed");

    eprintln!("Starting poll...");
    nl.start_poll(0).expect("Failed to start poll");

    eprintln!("Waiting for tag (30s)...");
    for _ in 0..30 {
        match nl.wait_target(1000) {
            Ok(Some(target)) => {
                println!("TAG FOUND!");
                println!("  Type: {}", target.tag_type_str());
                println!("  UID:  {}", target.uid_hex());
                println!("  idx:  {}", target.idx);
                println!("  sel_res: 0x{:02X}", target.sel_res);

                // Connect
                match nfc::NfcRawSock::connect(0, target.idx, target.protocol) {
                    Ok(sock) => {
                        println!("  Connected with proto {}!", target.protocol);
                        if target.protocol == nfc::NFC_PROTO_ISO14443 {
                            emv_full_read(&sock);
                        } else {
                            // T2T: read block 0
                            match sock.transceive(&[0x30, 0x00], 2000) {
                                Ok(data) => println!("  Block 0 ({} bytes): {:02x?}", data.len(), data),
                                Err(e) => println!("  Read: {}", e),
                            }
                        }
                    }
                    Err(e) => println!("  Connect failed: {}", e),
                }
                break;
            }
            Ok(None) => {
                eprint!(".");
            }
            Err(e) => {
                eprintln!("\nwait error: {}", e);
            }
        }
    }

    eprintln!("\nStopping poll...");
    let _ = nl.stop_poll(0);
    eprintln!("Done.");
}

fn hexdump(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
}

fn bcd_to_str(bcd: &[u8]) -> String {
    let mut s = String::new();
    for b in bcd {
        let hi = (b >> 4) & 0x0f;
        let lo = b & 0x0f;
        if hi <= 9 { s.push((b'0' + hi) as char); }
        if lo <= 9 { s.push((b'0' + lo) as char); }
    }
    s
}

fn tlv_find<'a>(data: &'a [u8], tag: u16) -> Option<&'a [u8]> {
    let mut pos = 0;
    while pos < data.len() {
        let mut t = data[pos] as u16;
        pos += 1;
        if (t & 0x1f) == 0x1f && pos < data.len() {
            t = (t << 8) | data[pos] as u16;
            pos += 1;
        }
        if pos >= data.len() { break; }
        let mut l = data[pos] as usize;
        pos += 1;
        if l == 0x81 && pos < data.len() { l = data[pos] as usize; pos += 1; }
        else if l == 0x82 && pos + 1 < data.len() { l = ((data[pos] as usize) << 8) | data[pos+1] as usize; pos += 2; }
        if pos + l > data.len() { break; }
        if t == tag { return Some(&data[pos..pos+l]); }
        if (t & 0x20) != 0 || ((t >> 8) & 0x20) != 0 {
            if let Some(f) = tlv_find(&data[pos..pos+l], tag) { return Some(f); }
        }
        pos += l;
    }
    None
}

fn check_sw(resp: &[u8]) -> bool {
    resp.len() >= 2 && resp[resp.len()-2] == 0x90 && resp[resp.len()-1] == 0x00
}

fn emv_full_read(sock: &nfc::NfcRawSock) {
    println!("\n=== EMV CARD READ ===\n");

    // Step 1: SELECT PPSE
    let ppse = b"\x00\xA4\x04\x00\x0E2PAY.SYS.DDF01\x00";
    let resp = match sock.transceive(ppse, 3000) {
        Ok(r) => r,
        Err(e) => { println!("  SELECT PPSE failed: {}", e); return; }
    };
    println!("  PPSE ({} bytes): {}", resp.len(), hexdump(&resp));
    if !check_sw(&resp) {
        println!("  PPSE rejected: {:02x?}", &resp[resp.len().saturating_sub(2)..]);
        return;
    }
    let body = &resp[..resp.len()-2];

    // Find AID
    let aid = match tlv_find(body, 0x4F) {
        Some(v) => v.to_vec(),
        None => { println!("  No AID in PPSE body: {}", hexdump(body)); return; }
    };
    println!("  AID: {}", hexdump(&aid));

    if let Some(label) = tlv_find(body, 0x50) {
        println!("  App: {}", String::from_utf8_lossy(label));
    }

    // Step 2: SELECT payment application
    let mut sel = vec![0x00, 0xA4, 0x04, 0x00, aid.len() as u8];
    sel.extend_from_slice(&aid);
    sel.push(0x00);
    let resp = match sock.transceive(&sel, 3000) {
        Ok(r) => r,
        Err(e) => { println!("  SELECT AID failed: {}", e); return; }
    };
    if !check_sw(&resp) {
        println!("  AID rejected: {:02x?}", &resp[resp.len().saturating_sub(2)..]);
        return;
    }
    let body = &resp[..resp.len()-2];
    println!("  FCI ({} bytes): {}", body.len(), hexdump(body));

    if let Some(label) = tlv_find(body, 0x50) {
        println!("  Label: {}", String::from_utf8_lossy(label));
    }

    // Check for PDOL (tag 9F38)
    let pdol = tlv_find(body, 0x9F38);
    if let Some(p) = pdol {
        println!("  PDOL: {}", hexdump(p));
    }

    // Step 3: GET PROCESSING OPTIONS
    // Build PDOL data if present, otherwise send empty
    let gpo_data = if let Some(pdol_data) = pdol {
        // Build a DOL response with zeros for all requested tags
        let mut dol_len = 0usize;
        let mut i = 0;
        while i < pdol_data.len() {
            // Skip tag (1 or 2 bytes)
            if (pdol_data[i] & 0x1f) == 0x1f { i += 2; } else { i += 1; }
            if i >= pdol_data.len() { break; }
            dol_len += pdol_data[i] as usize;
            i += 1;
        }
        let mut gpo = vec![0x80, 0xA8, 0x00, 0x00];
        gpo.push((dol_len + 2) as u8); // Lc
        gpo.push(0x83); // tag
        gpo.push(dol_len as u8); // len
        gpo.extend(vec![0x00; dol_len]); // zero-filled PDOL data
        gpo.push(0x00); // Le
        gpo
    } else {
        vec![0x80, 0xA8, 0x00, 0x00, 0x02, 0x83, 0x00, 0x00]
    };

    let resp = match sock.transceive(&gpo_data, 3000) {
        Ok(r) => r,
        Err(e) => { println!("  GPO failed: {}", e); return; }
    };

    let gpo_ok = check_sw(&resp);
    if gpo_ok {
        let body = &resp[..resp.len()-2];
        println!("  GPO: {}", hexdump(body));

        // Parse AFL (Application File Locator) from GPO response
        // Format 1: tag 80 (data = AIP[2] + AFL[n*4])
        // Format 2: tag 77 with nested tags
        if let Some(data80) = tlv_find(body, 0x80) {
            if data80.len() >= 6 {
                let afl = &data80[2..]; // skip AIP (2 bytes)
                println!("  AIP: {}", hexdump(&data80[..2]));
                println!("  AFL: {}", hexdump(afl));
                read_afl_records(sock, afl);
            }
        } else if let Some(afl) = tlv_find(body, 0x94) {
            println!("  AFL: {}", hexdump(afl));
            read_afl_records(sock, afl);
        }
    } else {
        println!("  GPO status: {:02x?}", &resp[resp.len().saturating_sub(2)..]);
        println!("  (Trying READ RECORD brute force...)");
        brute_force_records(sock);
    }

    println!("\n=== END ===");
}

fn read_afl_records(sock: &nfc::NfcRawSock, afl: &[u8]) {
    // AFL entries: [SFI<<3 | P2_extra, first_record, last_record, num_offline_auth]
    let mut i = 0;
    while i + 3 < afl.len() {
        let sfi = afl[i] >> 3;
        let first = afl[i + 1];
        let last = afl[i + 2];
        // afl[i+3] = offline auth records (ignored)
        i += 4;

        for rec in first..=last {
            let cmd = [0x00, 0xB2, rec, (sfi << 3) | 0x04, 0x00];
            match sock.transceive(&cmd, 2000) {
                Ok(resp) if check_sw(&resp) => {
                    let body = &resp[..resp.len()-2];
                    println!("\n  [SFI {} Rec {}] ({} bytes)", sfi, rec, body.len());
                    parse_emv_record(body);
                }
                Ok(resp) => {
                    let sw = if resp.len() >= 2 { format!("{:02x}{:02x}", resp[resp.len()-2], resp[resp.len()-1]) } else { "??".into() };
                    println!("  [SFI {} Rec {}] SW: {}", sfi, rec, sw);
                }
                Err(_) => {}
            }
        }
    }
}

fn brute_force_records(sock: &nfc::NfcRawSock) {
    for sfi in 1..=10u8 {
        for rec in 1..=10u8 {
            let cmd = [0x00, 0xB2, rec, (sfi << 3) | 0x04, 0x00];
            match sock.transceive(&cmd, 1000) {
                Ok(resp) if check_sw(&resp) => {
                    let body = &resp[..resp.len()-2];
                    println!("\n  [SFI {} Rec {}] ({} bytes)", sfi, rec, body.len());
                    parse_emv_record(body);
                }
                _ => {}
            }
        }
    }
}

fn parse_emv_record(data: &[u8]) {
    // PAN (tag 5A)
    if let Some(pan) = tlv_find(data, 0x5A) {
        println!("    PAN: {}", bcd_to_str(pan));
    }
    // Cardholder name (tag 5F20)
    if let Some(name) = tlv_find(data, 0x5F20) {
        let s = String::from_utf8_lossy(name).trim().to_string();
        if !s.is_empty() && s != "/" {
            println!("    Name: {}", s);
        }
    }
    // Expiry date (tag 5F24, YYMMDD BCD)
    if let Some(exp) = tlv_find(data, 0x5F24) {
        if exp.len() >= 3 {
            println!("    Expires: {:02X}/{:02X}/20{:02X}", exp[2], exp[1], exp[0]);
        }
    }
    // Issuer country code (tag 5F28)
    if let Some(cc) = tlv_find(data, 0x5F28) {
        if cc.len() >= 2 {
            println!("    Country: {:02X}{:02X}", cc[0], cc[1]);
        }
    }
    // Application preferred name (tag 9F12)
    if let Some(name) = tlv_find(data, 0x9F12) {
        println!("    Preferred name: {}", String::from_utf8_lossy(name));
    }
    // Track 2 equivalent (tag 57) — contains PAN + expiry
    if let Some(t2) = tlv_find(data, 0x57) {
        println!("    Track2: {}", bcd_to_str(t2));
    }
    // Application label (tag 50)
    if let Some(label) = tlv_find(data, 0x50) {
        println!("    App label: {}", String::from_utf8_lossy(label));
    }
    // Application effective date (tag 5F25)
    if let Some(eff) = tlv_find(data, 0x5F25) {
        if eff.len() >= 3 {
            println!("    Effective: {:02X}/{:02X}/20{:02X}", eff[2], eff[1], eff[0]);
        }
    }
    // Application usage control (tag 9F07)
    if let Some(auc) = tlv_find(data, 0x9F07) {
        println!("    Usage ctrl: {}", hexdump(auc));
    }
    // Card risk management data (tag 9F6C)
    if let Some(crm) = tlv_find(data, 0x9F6C) {
        println!("    CRM: {}", hexdump(crm));
    }
    // Raw hex if nothing interesting found
    let has_interesting = tlv_find(data, 0x5A).is_some()
        || tlv_find(data, 0x5F20).is_some()
        || tlv_find(data, 0x57).is_some();
    if !has_interesting {
        println!("    Raw: {}", hexdump(&data[..std::cmp::min(data.len(), 64)]));
    }
}
