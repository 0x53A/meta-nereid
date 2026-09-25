//! Default: one detection-only check. --transport-test: bounded read-only exchanges.
#[path = "../nfc.rs"]
mod nfc;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};
fn main() -> io::Result<()> {
    let mut debug = false;
    let mut transport = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--debug" => debug = true,
            "--transport-test" => transport = true,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "use --debug and/or --transport-test",
                ))
            }
        }
    }
    let debug = if debug {
        Some(nfc::DebugLog::create()?)
    } else {
        None
    };
    let cycles = if transport { 10 } else { 1 };
    for cycle in 0..cycles {
        let mut nl = nfc::NfcNetlink::open_with_debug(debug.clone())?;
        let initial = nl.is_powered(0)?;
        nl.dev_up(0)?;
        nl.start_poll(0)?;
        let target = nl.wait_target(2000)?;
        println!("Cycle {}: target present: {}", cycle + 1, target.is_some());
        if transport {
            let target = target
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no NFC target detected"))?;
            if target.protocol != nfc::NFC_PROTO_ISO14443
                && target.protocol != nfc::NFC_PROTO_ISO14443_B
            {
                return Err(io::Error::other("transport test requires ISO-DEP"));
            }
            let mut sock =
                nfc::NfcRawSock::connect_with_debug(0, target.idx, target.protocol, debug.clone())?;
            for _ in 0..32 {
                // Select the standard NDEF application. Unsupported applications
                // still return a valid status response; no writes or payment reads.
                let response = sock.transceive(
                    b"\x00\xa4\x04\x00\x07\xd2\x76\x00\x00\x85\x01\x01\x00",
                    2000,
                )?;
                if response.len() < 2 {
                    return Err(io::Error::other("missing ISO-DEP status response"));
                }
            }
            if cycle == cycles - 1 {
                match sock.transceive(b"\x00\xa4\x04\x00\x07\xd2\x76\x00\x00\x85\x01\x01\x00", 1) {
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        println!("Forced receive timeout exercised")
                    }
                    Err(e) => return Err(e),
                    Ok(_) => println!("Response completed within 1 ms; timeout not exercised"),
                }
            } else {
                // Cancellation closes the socket and must release its target.
                sock.set_running(Arc::new(AtomicBool::new(false)));
                let err = sock
                    .transceive(&[0], 2000)
                    .expect_err("cancelled exchange must fail");
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
                println!("Cancellation passed");
            }
            drop(sock);
            println!("32 exchanges passed");
        }
        nl.stop_poll(0)?;
        nl.finish()?;
        let final_power = nl.is_powered(0)?;
        if initial != final_power {
            return Err(io::Error::other("adapter power was not restored"));
        }
        println!("Power restored: {final_power}");
    }
    if let Some(log) = debug {
        log.text("PROBE END");
        if let Some(e) = log.error() {
            return Err(io::Error::other(e));
        }
        println!("Debug log: {}", log.path().display());
    }
    Ok(())
}
