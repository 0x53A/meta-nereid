//! Bounded raw health recording. Requires exclusive HAL ownership; external service restores sensorfw.
#[allow(dead_code)]
#[path = "../decode.rs"]
mod decode;
#[allow(dead_code)]
#[path = "../ffi.rs"]
mod ffi;
use ffi::{Api, Handle, RawReader};
use std::{ffi::c_void, ptr};
#[path = "../health_poll.rs"]
mod health_poll;
#[path = "../health_quality.rs"]
mod health_quality;
#[path = "../health_session.rs"]
mod health_session;
#[repr(C)]
struct Buffer {
    data: *const u8,
    size: usize,
}
fn run() -> Result<(), String> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if !["record", "off", "poll-worker"].contains(&mode.as_str()) {
        return Err("usage: hoki-health-record record|off".into());
    }
    let number = |name: &str, default: u64, max: u64| -> Result<u64, String> {
        let v = match std::env::var(name) {
            Ok(v) => v.parse::<u64>().map_err(|_| format!("invalid {name}"))?,
            Err(_) => default,
        };
        if v > max {
            return Err(format!("{name} exceeds {max}"));
        }
        Ok(v)
    };
    let duration = number("HOKI_RECORD_SECONDS", 1190, 86400)?;
    if duration == 0 {
        return Err("duration must be positive".into());
    }
    let latency = number("HOKI_BATCH_MS", 0, 60000)? as i64 * 1_000_000;
    let wakeup = number("HOKI_WAKEUP", 0, 1)? as u32;
    let selection = std::env::var("HOKI_SENSOR_TYPES").ok();
    let all = selection.as_deref() == Some("all");
    let requested = selection
        .filter(|_| !all)
        .map(|v| {
            v.split(',')
                .map(|s| {
                    s.parse::<u32>()
                        .map_err(|_| "invalid sensor type".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let flush_dir = std::env::var_os("HOKI_FLUSH_DIR").map(std::path::PathBuf::from);
    let binary = match std::env::var("HOKI_RECORD_FORMAT").as_deref() {
        Ok("binary") => true,
        Ok("text") | Err(_) => false,
        _ => return Err("HOKI_RECORD_FORMAT must be text or binary".into()),
    };
    if mode == "record" {
        if let Some(dir) = &flush_dir {
            let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .map_err(|e| e.to_string())?;
            let identity = format!("boot={} pid={}\n", boot.trim(), std::process::id());
            health_session::claim(dir, &identity).map_err(|e| format!("capture session: {e}"))?;
        }
    }
    let api = Api::load()?;
    let lib = unsafe { libloading::Library::new("libgbinder.so.1") }.map_err(|e| e.to_string())?;
    let vec: unsafe extern "C" fn(*mut RawReader, *mut usize, *mut usize) -> *const u8 = unsafe {
        *lib.get(b"gbinder_reader_read_hidl_vec\0")
            .map_err(|e| e.to_string())?
    };
    let remote_unref: unsafe extern "C" fn(*mut c_void) = unsafe {
        *lib.get(b"gbinder_remote_object_unref\0")
            .map_err(|e| e.to_string())?
    };
    let remote_ref: unsafe extern "C" fn(*mut c_void) -> *mut c_void = unsafe {
        *lib.get(b"gbinder_remote_object_ref\0")
            .map_err(|e| e.to_string())?
    };
    let sm = Handle::new(
        &api,
        unsafe { (api.gbinder_servicemanager_new)(c"/dev/hwbinder".as_ptr()) },
        api.gbinder_servicemanager_unref,
        "manager",
    )?;
    let mut status = 0;
    let remote = Handle::new(
        &api,
        unsafe {
            remote_ref((api.gbinder_servicemanager_get_service_sync)(
                sm.ptr,
                c"android.hardware.sensors@1.0::ISensors/default".as_ptr(),
                &mut status,
            ))
        },
        remote_unref,
        "sensor service",
    )?;
    if status != 0 {
        return Err(format!("service status {status}"));
    }
    let client = Handle::new(
        &api,
        unsafe {
            (api.gbinder_client_new)(
                remote.ptr,
                c"android.hardware.sensors@1.0::ISensors".as_ptr(),
            )
        },
        api.gbinder_client_unref,
        "client",
    )?;
    let reply = Handle::new(
        &api,
        unsafe {
            (api.gbinder_client_transact_sync_reply)(client.ptr, 1, ptr::null_mut(), &mut status)
        },
        api.gbinder_remote_reply_unref,
        "reply",
    )?;
    if status != 0 {
        return Err(format!("transaction status {status}"));
    }
    let mut reader: RawReader = unsafe { std::mem::zeroed() };
    unsafe { (api.gbinder_remote_reply_init_reader)(reply.ptr, &mut reader) };
    let mut result = 0;
    if unsafe { (api.gbinder_reader_read_uint32)(&mut reader, &mut result) } == 0 || result != 0 {
        return Err(format!("reply status {result}"));
    }
    let (mut count, mut size) = (0, 0);
    let data = unsafe { vec(&mut reader, &mut count, &mut size) };
    if data.is_null() || size != 112 || count > 256 {
        return Err(format!("invalid sensor vector count={count} size={size}"));
    }

    // Select the requested wakeup variant. Retain reporting mode for one-shot
    // rearming and flush eligibility; one-shot sensors do not support flush.
    let wanted = [
        1u32, 2, 4, 5, 6, 19, 21, 31, 34, 65561, 65572, 65573, 65574, 65575, 65581, 65582,
    ];
    let mut selected = Vec::new();
    for i in 0..count {
        let b = unsafe { std::slice::from_raw_parts(data.add(i * size), size) };
        let u = |o: usize| u32::from_ne_bytes(b[o..o + 4].try_into().unwrap());
        if (all || requested.as_deref().unwrap_or(&wanted).contains(&u(44))) && u(108) & 1 == wakeup
        {
            if mode != "poll-worker" {
                eprintln!(
                    "DESCRIPTOR handle={} type={} min_delay_us={} max_delay_us={} fifo_reserved={} fifo_max={} flags={}",
                    u(0),
                    u(44),
                    u(76) as i32,
                    u(104) as i32,
                    u(80),
                    u(84),
                    u(108)
                );
            }
            selected.push((u(0), u(44), u(76) as i32, u(104) as i32, (u(108) >> 1) & 7));
        }
    }
    if selected.is_empty() {
        return Err("no sensor streams".into());
    }
    if let Some(types) = &requested {
        for typ in types {
            if !selected.iter().any(|s| s.1 == *typ) {
                return Err(format!(
                    "requested type {typ} has no wakeup={wakeup} descriptor"
                ));
            }
        }
    }
    let write64: unsafe extern "C" fn(*mut ffi::RawWriter, i64) = unsafe {
        *lib.get(b"gbinder_writer_append_int64\0")
            .map_err(|e| e.to_string())?
    };
    let call = |code: u32, fill: &dyn Fn(&mut ffi::RawWriter)| -> Result<Handle<'_>, String> {
        let req = Handle::new(
            &api,
            unsafe { (api.gbinder_client_new_request)(client.ptr) },
            api.gbinder_local_request_unref,
            "request",
        )?;
        let mut w: ffi::RawWriter = unsafe { std::mem::zeroed() };
        unsafe { (api.gbinder_local_request_init_writer)(req.ptr, &mut w) };
        fill(&mut w);
        let mut st = 0;
        let rep = Handle::new(
            &api,
            unsafe { (api.gbinder_client_transact_sync_reply)(client.ptr, code, req.ptr, &mut st) },
            api.gbinder_remote_reply_unref,
            "control reply",
        )?;
        if st != 0 {
            return Err(format!("transport {st}"));
        }
        Ok(rep)
    };
    let check = |r: &Handle<'_>| -> Result<(), String> {
        let mut reader: RawReader = unsafe { std::mem::zeroed() };
        unsafe { (api.gbinder_remote_reply_init_reader)(r.ptr, &mut reader) };
        for _ in 0..2 {
            let mut v = 0;
            if unsafe { (api.gbinder_reader_read_uint32)(&mut reader, &mut v) } == 0 || v != 0 {
                return Err(format!("HAL result {}", v as i32));
            }
        }
        Ok(())
    };

    // A separate process owns only the blocking poll. It never activates sensors
    // or writes the persistent output, and the parent kills/reaps it on every exit.
    if mode == "poll-worker" {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        loop {
            let rep = call(4, &|w| unsafe { (api.gbinder_writer_append_int32)(w, 128) })?;
            let mut r: RawReader = unsafe { std::mem::zeroed() };
            unsafe { (api.gbinder_remote_reply_init_reader)(rep.ptr, &mut r) };
            for _ in 0..2 {
                let mut v = 0;
                if unsafe { (api.gbinder_reader_read_uint32)(&mut r, &mut v) } == 0 || v != 0 {
                    return Err(format!("poll status {v}"));
                }
            }
            let (mut n, mut size) = (0, 0);
            let p = unsafe { vec(&mut r, &mut n, &mut size) };
            if size != 80 || n > 128 || (p.is_null() && n != 0) {
                return Err(format!("bad events {n}x{size}"));
            }
            let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
            if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
                return Err("clock read failed".into());
            }
            let arrival = ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64;
            for i in 0..n {
                let b = unsafe { std::slice::from_raw_parts(p.add(i * size), size) };
                out.write_all(&arrival.to_le_bytes())
                    .map_err(|e| e.to_string())?;
                out.write_all(b).map_err(|e| e.to_string())?;
            }
            out.flush().map_err(|e| e.to_string())?;
        }
    }

    let activate = |sensor: u32, on: u32| -> Result<(), String> {
        let r = call(3, &|w| unsafe {
            (api.gbinder_writer_append_int32)(w, sensor);
            (api.gbinder_writer_append_int32)(w, on)
        })?;
        check(&r)
    };
    if mode == "off" {
        let mut errors = Vec::new();
        for &(sensor, typ, _, _, _) in &selected {
            if let Err(e) = activate(sensor, 0) {
                errors.push(format!("{sensor}:{typ}:{e}"))
            }
        }
        return if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join(";"))
        };
    }
    use std::io::Write;
    if flush_dir.is_some() {
        // fsync(file) alone does not persist a newly created directory entry.
        let output = std::fs::read_link("/proc/self/fd/1").map_err(|e| e.to_string())?;
        if !std::fs::metadata(&output)
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err("flush protocol requires regular-file stdout".into());
        }
        health_session::sync_ancestors(output.parent().ok_or("output has no parent")?)
            .map_err(|e| e.to_string())?;
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    if binary {
        // Versioned little-endian framing: magic, record size, version. Each record
        // contains arrival BOOTTIME followed by the exact 80-byte HIDL event.
        out.write_all(b"HOKISEN1\x58\0\0\0\x01\0\0\0")
            .map_err(|e| e.to_string())?;
    } else {
        writeln!(out,"# raw HIDL Sensors1.0 events: arrival_boottime_ns timestamp_ns handle type payload64_hex").map_err(|e|e.to_string())?;
    }
    let mut active = Vec::new();
    let mut continuous_periods = Vec::new();
    let mut one_shot = std::collections::HashSet::new();
    for &(sensor, typ, min, max, reporting_mode) in &selected {
        let mut period = if matches!(typ, 1 | 4 | 16 | 35 | 65572) {
            40_000_000i64
        } else {
            200_000_000i64
        };
        if min > 0 {
            period = period.max(min as i64 * 1000)
        }
        if max > 0 {
            period = period.min(max as i64 * 1000)
        }
        let setup = (|| {
            if reporting_mode != 2 {
                let r = call(5, &|w| unsafe {
                    (api.gbinder_writer_append_int32)(w, sensor);
                    write64(w, period);
                    write64(w, latency)
                })?;
                check(&r)?;
            }
            activate(sensor, 1)
        })();
        match setup {
            Ok(()) => {
                active.push(sensor);
                if reporting_mode == 0 {
                    continuous_periods.push((sensor, period));
                }
                if reporting_mode == 2 {
                    one_shot.insert(sensor);
                }
                eprintln!(
                    "ACTIVE handle={sensor} type={typ} period_ns={period} latency_ns={latency} wakeup={wakeup} reporting_mode={reporting_mode}"
                );
            }
            Err(e) => eprintln!("UNAVAILABLE handle={sensor} type={typ}: {e}"),
        }
    }
    if active.is_empty() {
        return Err("no activated streams".into());
    }
    let boottime = || -> Result<i64, String> {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
            return Err("clock read failed".into());
        }
        Ok(ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64)
    };
    let start = boottime()?;
    let mut events = 0u64;
    let mut timing = health_quality::TimingMonitor::new(start, continuous_periods);
    let mut flush_generation = String::new();
    let mut pending_flush = std::collections::HashSet::new();
    let experiment = (|| -> Result<(), String> {
        let mut poll = health_poll::PollReader::start(flush_dir.as_deref())?;
        let mut stopping = false;
        let mut final_flush = false;
        let mut flush_deadline = 0i64;
        eprintln!("READY boottime_ns={start} duration_s={duration} control=independent");
        loop {
            let now = boottime()?;
            stopping |= now - start >= duration as i64 * 1_000_000_000
                || flush_dir
                    .as_ref()
                    .is_some_and(|dir| dir.join("stop").exists());
            if stopping && pending_flush.is_empty() && !final_flush {
                flush_generation.clear();
                final_flush = true;
                flush_deadline = now + 10_000_000_000;
                for &sensor in &active {
                    if one_shot.contains(&sensor) {
                        continue;
                    }
                    let rep = call(6, &|w| unsafe {
                        (api.gbinder_writer_append_int32)(w, sensor)
                    })?;
                    check(&rep)?;
                    pending_flush.insert(sensor);
                }
                eprintln!(
                    "FINAL_FLUSH requested_ns={now} handles={}",
                    pending_flush.len()
                );
            }
            if final_flush && pending_flush.is_empty() {
                break;
            }
            if !pending_flush.is_empty() && now > flush_deadline {
                return Err(format!("flush timeout: {pending_flush:?}"));
            }
            if let Some(dir) = &flush_dir {
                match std::fs::read_to_string(dir.join("flush-request")) {
                    Ok(generation)
                        if !stopping
                            && generation != flush_generation
                            && pending_flush.is_empty() =>
                    {
                        if generation.is_empty()
                            || generation.len() > 32
                            || !generation.bytes().all(|c| c.is_ascii_digit())
                        {
                            return Err("invalid flush generation".into());
                        }
                        for &sensor in &active {
                            if one_shot.contains(&sensor) {
                                continue;
                            }
                            let rep = call(6, &|w| unsafe {
                                (api.gbinder_writer_append_int32)(w, sensor)
                            })?;
                            check(&rep)?;
                            pending_flush.insert(sensor);
                        }
                        flush_generation = generation;
                        flush_deadline = boottime()? + 10_000_000_000;
                        eprintln!(
                            "FLUSH_REQUEST generation={flush_generation} boottime_ns={}",
                            boottime()?
                        );
                    }
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
                    _ => {}
                }
            }
            let remaining = if stopping || !pending_flush.is_empty() {
                flush_deadline - boottime()?
            } else {
                start + duration as i64 * 1_000_000_000 - boottime()?
            };
            let timeout_ms = ((remaining.max(0) + 999_999) / 1_000_000).min(i32::MAX as i64) as i32;
            let batch = poll.receive(timeout_ms)?;
            let arrival = boottime()?;
            for record in &batch {
                let event_arrival = i64::from_le_bytes(record[..8].try_into().unwrap());
                let b = &record[8..];
                let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                if u(12) == 0 && u(16) == 1 {
                    pending_flush.remove(&u(8));
                }
                let timestamp = i64::from_ne_bytes(b[..8].try_into().unwrap());
                if u(12) != 0 {
                    if let Some(issue) = timing.observe(u(8), timestamp) {
                        eprintln!("TIMING handle={} type={} {:?}", u(8), u(12), issue);
                    }
                }
                if binary {
                    out.write_all(&event_arrival.to_le_bytes())
                        .map_err(|e| e.to_string())?;
                    out.write_all(b).map_err(|e| e.to_string())?;
                } else {
                    write!(out, "{event_arrival} {timestamp} {} {} ", u(8), u(12))
                        .map_err(|e| e.to_string())?;
                    for v in &b[16..80] {
                        write!(out, "{v:02x}").map_err(|e| e.to_string())?;
                    }
                    writeln!(out).map_err(|e| e.to_string())?;
                }
                events += 1;
                if !stopping && u(12) != 0 && one_shot.contains(&u(8)) {
                    activate(u(8), 1)
                        .map_err(|e| format!("one-shot rearm handle={}: {e}", u(8)))?;
                    eprintln!("REARM handle={} timestamp_ns={timestamp}", u(8));
                }
            }
            out.flush().map_err(|e| e.to_string())?;
            if !flush_generation.is_empty() && pending_flush.is_empty() {
                if let Some(dir) = &flush_dir {
                    let ack = dir.join("flush-done");
                    if std::fs::read_to_string(&ack).ok().as_deref() != Some(&flush_generation) {
                        // Acknowledge only after every HAL completion and durable file output.
                        if unsafe { libc::fsync(libc::STDOUT_FILENO) } != 0 {
                            return Err(std::io::Error::last_os_error().to_string());
                        }
                        let mut ack_file = std::fs::File::create(dir.join("flush-done.tmp"))
                            .map_err(|e| e.to_string())?;
                        ack_file
                            .write_all(flush_generation.as_bytes())
                            .map_err(|e| e.to_string())?;
                        ack_file.sync_all().map_err(|e| e.to_string())?;
                        std::fs::rename(dir.join("flush-done.tmp"), ack)
                            .map_err(|e| e.to_string())?;
                        std::fs::File::open(dir)
                            .and_then(|f| f.sync_all())
                            .map_err(|e| e.to_string())?;
                        eprintln!(
                            "DURABLE_FLUSH generation={flush_generation} arrival_ns={arrival}"
                        );
                    }
                }
            }
        }
        if !pending_flush.is_empty() {
            return Err(format!(
                "recording ended with {} incomplete flushes",
                pending_flush.len()
            ));
        }
        Ok(())
    })();
    let mut cleanup_errors = Vec::new();
    for sensor in active {
        if let Err(e) = activate(sensor, 0) {
            cleanup_errors.push(format!("handle={sensor}: {e}"));
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    // With the file-backed flush protocol, a normal END also guarantees that the
    // final records (including any tail after the last checkpoint) reached disk.
    if flush_dir.is_some() && unsafe { libc::fsync(libc::STDOUT_FILENO) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if let Err(e) = experiment {
        eprintln!("ABORT events={events} cleanup_errors={cleanup_errors:?}");
        return Err(e);
    }
    if !cleanup_errors.is_empty() {
        return Err(format!("deactivation failed: {cleanup_errors:?}"));
    }
    eprintln!("END events={events}");
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("HEALTH RECORD: {e}");
        std::process::exit(1)
    }
}
