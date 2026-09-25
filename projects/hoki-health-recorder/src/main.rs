mod profile_alarm;
mod sleep_runtime;
mod suspend_runtime;
use hoki_health_recorder::storage_admission::{hal_limit, RESERVE};
use hoki_health_recorder::{
    finalized, healthy, notify_ready, owned_request, recovery_matches, request, select, select_ssc,
    valid_session_id, ControlError, Result,
};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn now() -> Result<f64> {
    let mut t: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(t.tv_sec as f64 + t.tv_nsec as f64 / 1e9)
}
struct Waiter {
    alarm: OwnedFd,
    signal: OwnedFd,
}
impl Waiter {
    fn new() -> Result<Self> {
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigaddset(&mut mask, libc::SIGINT);
        }
        if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let fd = unsafe { libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let signal = unsafe { OwnedFd::from_raw_fd(fd) };
        let fd = unsafe { libc::timerfd_create(libc::CLOCK_BOOTTIME_ALARM, libc::TFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            signal,
            alarm: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }
    fn interrupted(&self) -> Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.signal.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut fd, 1, 0) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err("signal descriptor failed".into());
        }
        Ok(fd.revents & libc::POLLIN != 0)
    }
    fn wait(&self, seconds: f64) -> Result<bool> {
        let nanos = (seconds.max(0.001) * 1e9) as u64;
        let timer = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: (nanos / 1_000_000_000) as _,
                tv_nsec: (nanos % 1_000_000_000) as _,
            },
        };
        if unsafe { libc::timerfd_settime(self.alarm.as_raw_fd(), 0, &timer, std::ptr::null_mut()) }
            != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut fds = [
            libc::pollfd {
                fd: self.signal.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.alarm.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            if fds
                .iter()
                .any(|f| f.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
            {
                return Err("alarm/signal descriptor failed".into());
            }
            if fds[0].revents & libc::POLLIN != 0 {
                return Ok(true);
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let mut count = 0u64;
                if unsafe { libc::read(self.alarm.as_raw_fd(), (&mut count as *mut u64).cast(), 8) }
                    != 8
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                return Ok(false);
            }
        }
    }
}
fn private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or("missing parent")?;
    let m = fs::symlink_metadata(parent)?;
    if !m.is_dir() || m.uid() != 0 || m.mode() & 0o077 != 0 {
        return Err("parent must be a root-owned private directory".into());
    }
    Ok(())
}
fn private_json(path: &Path) -> Result<Value> {
    use std::io::Read;
    private_parent(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o077 != 0 || m.nlink() != 1 || m.len() > 131072 {
        return Err("unsafe JSON metadata file".into());
    }
    Ok(serde_json::from_reader(file.take(131073))?)
}
fn persist_named(directory: &Path, name: &str, value: Value) -> Result<()> {
    // A failed write or killed publisher may leave its temporary file behind.
    // Preserve that evidence without preventing a later recovery publication.
    let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
    let id = id.trim();
    if !valid_session_id(id) {
        return Err("invalid publication UUID".into());
    }
    let temp = directory.join(format!("{name}-{id}.pending"));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer(&mut f, &value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(temp, directory.join(format!("{name}.json")))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
fn persist(directory: &Path, value: Value) -> Result<()> {
    persist_named(directory, "controller", value)
}
fn status_until(socket: &Path, token: &str, stop: bool) -> Result<Value> {
    let deadline = now()? + 12.0;
    loop {
        let s = owned_request(socket, token, json!({"command":"status"}))?;
        healthy(&s)?;
        if if stop {
            finalized(&s)?
        } else {
            s["checkpoint_complete"] == true
        } {
            return Ok(s);
        }
        if now()? >= deadline {
            return Err("checkpoint deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[derive(Debug)]
struct LeaseBusy;
impl std::fmt::Display for LeaseBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "another controller owns the lease")
    }
}
impl std::error::Error for LeaseBusy {}
fn controller_lease(socket: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(socket.with_file_name("controller.lock"))?;
    let meta = lock.metadata()?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err("unsafe controller lock".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(LeaseBusy.into());
        }
        return Err(error.into());
    }
    Ok(lock)
}
fn stop_owned(socket: &Path, token: &str) -> Result<Value> {
    let deadline = now()? + 12.0;
    loop {
        match owned_request(socket, token, json!({"command":"stop"})) {
            Ok(_) => break,
            Err(e)
                if e.downcast_ref::<ControlError>()
                    .is_some_and(|x| x.code == -(libc::EBUSY as i64))
                    && now()? < deadline =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    status_until(socket, token, true)
}
fn recover(socket: &Path, directory: &Path) -> Result<()> {
    if !socket.is_absolute() || !directory.is_absolute() {
        return Err("absolute paths required".into());
    }
    private_parent(socket)?;
    private_parent(directory)?;
    let _lock = controller_lease(socket)?;
    let dir_meta = fs::symlink_metadata(directory)?;
    if !dir_meta.is_dir() || dir_meta.uid() != 0 || dir_meta.mode() & 0o077 != 0 {
        return Err("unsafe capture directory".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(directory.join("controller.json"))?;
    let m = file.metadata()?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o077 != 0 || m.nlink() != 1 || m.len() > 131072 {
        return Err("unsafe controller metadata".into());
    }
    let metadata: Value = serde_json::from_reader(file)?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let mut evidence = json!({"version":1,"result":"failed",
        "original_controller_phase":metadata["phase"],"session_id":metadata["session_id"],
        "observed_boot_id":boot.trim(),"start_boottime_seconds":now()?});
    let outcome = (|| -> Result<()> {
        let status = request(socket, json!({"command":"status"}))?;
        evidence["initial_status"] = status.clone();
        let matches = recovery_matches(&metadata, boot.trim(), &status)?;
        if matches {
            let token = metadata["session_id"].as_str().ok_or("missing session")?;
            evidence["final_status"] = if finalized(&status)? {
                status
            } else {
                stop_owned(socket, token)?
            };
            evidence["result"] = json!("drained");
        } else {
            evidence["result"] = json!("not_current");
        }
        Ok(())
    })();
    if let Err(ref error) = outcome {
        evidence["error"] = json!(error.to_string());
    }
    evidence["boottime_seconds"] = json!(now()?);
    // Preserve both failures if recording the recovery outcome itself fails.
    if let Err(error) = persist_named(directory, "recovery", evidence.clone()) {
        if let Err(ref original) = outcome {
            eprintln!("recovery failed: {original}");
        }
        return Err(error);
    }
    eprintln!("recovery: {}", evidence["result"]);
    outcome?;
    Ok(())
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 {
        return Err(
            "usage: hoki-health-recorder SOCKET NEW_CAPTURE_DIRECTORY SECONDS (0=until signal)"
                .into(),
        );
    }
    if unsafe { libc::geteuid() } != 0 {
        return Err("root required".into());
    }
    if args[1] == "--prepare-sleep" {
        return sleep_runtime::prepare(Path::new(&args[2]), Path::new(&args[3]));
    }
    if args[1] == "--check-recording-space" {
        return hoki_health_recorder::storage_admission::check(
            Path::new(&args[2]),
            Path::new(&args[3]),
        );
    }
    if args[1] == "--reconcile-sleep" {
        return hoki_health_recorder::reboot_reconcile::run(
            Path::new(&args[2]),
            Path::new(&args[3]),
        );
    }
    for mode in [
        "--launch-sleep",
        "--apply-sleep",
        "--restore-sleep",
        "--await-sleep",
    ] {
        if args[1] == mode {
            return sleep_runtime::run(
                mode,
                Path::new(&args[2]),
                args[3].to_str().ok_or("invalid owner")?,
            );
        }
    }
    if args[1] == "--quiesce-sleep" {
        return hoki_health_recorder::ssc_quiesce::quiesce(
            Path::new(&args[2]),
            args[3].to_str().ok_or("invalid owner")?,
        );
    }
    if args[1] == "--snapshot-sleep" {
        let output = Path::new(&args[3]);
        if !output.is_absolute() {
            return Err("absolute snapshot path required".into());
        }
        private_parent(output)?;
        DirBuilder::new().mode(0o700).create(output)?;
        File::open(output.parent().ok_or("missing snapshot parent")?)?.sync_all()?;
        let mut helper =
            hoki_health_recorder::ssc_helper::Helper::new(Path::new(&args[2]), output)?;
        if let Some(unit) = std::env::var_os("HOKI_SSC_SUPERVISOR") {
            helper = helper.with_supervisor(unit.to_str().ok_or("invalid supervisor encoding")?)?;
        }
        return helper.snapshot();
    }
    if args[1] == "--suspend-recording" || args[1] == "--suspend-recording-paced" {
        return suspend_runtime::run(
            Path::new(&args[2]),
            Path::new(&args[3]),
            args[1] == "--suspend-recording-paced",
        );
    }
    if args[1] == "--cleanup" {
        return recover(Path::new(&args[2]), Path::new(&args[3]));
    }
    if args[1] == "--plan-sleep" {
        let baseline = Path::new(&args[2]);
        let output = Path::new(&args[3]);
        if !baseline.is_absolute() || !output.is_absolute() {
            return Err("absolute paths required".into());
        }
        let inventory = private_json(&baseline.join("discovery/inventory.json"))?;
        let discovery_status = private_json(&baseline.join("discovery/status.json"))?;
        let mut snapshots = Vec::new();
        let mut statuses = Vec::new();
        for name in ["user", "tracking", "detect"] {
            snapshots.push(private_json(&baseline.join(name).join("config.json"))?);
            statuses.push(private_json(&baseline.join(name).join("status.json"))?);
        }
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let plan = hoki_health_recorder::sleep_plan::prepare(
            &inventory,
            &discovery_status,
            &snapshots.try_into().unwrap(),
            &statuses.try_into().unwrap(),
            boot.trim(),
        )?;
        private_parent(output)?;
        DirBuilder::new().mode(0o700).create(output)?;
        File::open(output.parent().ok_or("missing plan parent")?)?.sync_all()?;
        persist_named(output, "plan", plan)?;
        return Ok(());
    }
    if args[1] == "--select-processed" {
        let directory = Path::new(&args[2]);
        let output = Path::new(&args[3]);
        if !directory.is_absolute() || !output.is_absolute() {
            return Err("absolute discovery and selection paths required".into());
        }
        let inventory = private_json(&directory.join("inventory.json"))?;
        let status = private_json(&directory.join("status.json"))?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let mut endpoints = serde_json::Map::new();
        for kind in ["fsl_min", "fsl_sleep", "fsl_rhr", "fsl_wk"] {
            endpoints.insert(
                kind.into(),
                json!(select_ssc(&inventory, &status, boot.trim(), kind)?),
            );
        }
        private_parent(output)?;
        DirBuilder::new().mode(0o700).create(output)?;
        File::open(output.parent().ok_or("missing selection parent")?)?.sync_all()?;
        return persist_named(
            output,
            "selection",
            json!({
                "version":1,"boot_id":boot.trim(),"endpoints":endpoints,
                "discovery_session_id":inventory["session_id"],
                "scope":"implemented processed readers; not all SSC endpoints"
            }),
        );
    }
    if args[1] == "--select-ssc" {
        let directory = Path::new(&args[2]);
        if !directory.is_absolute() {
            return Err("absolute discovery directory required".into());
        }
        let inventory = private_json(&directory.join("inventory.json"))?;
        let status = private_json(&directory.join("status.json"))?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let kind = args[3].to_str().ok_or("invalid SSC type")?;
        println!("{}", select_ssc(&inventory, &status, boot.trim(), kind)?);
        return Ok(());
    }
    let socket = PathBuf::from(&args[1]);
    let directory = PathBuf::from(&args[2]);
    let seconds: u64 = args[3].to_str().ok_or("invalid duration")?.parse()?;
    if seconds > 86400 || !socket.is_absolute() || !directory.is_absolute() {
        return Err("invalid duration or relative path".into());
    }
    private_parent(&socket)?;
    private_parent(&directory)?;
    let _lock = controller_lease(&socket)?;
    let waiter = Waiter::new()?; // Require an alarm source before activating anything.
    let inventory = request(&socket, json!({"command":"inventory"}))?;
    if inventory["session_ownership"] != true {
        return Err("backend lacks session ownership support".into());
    }
    let session_id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    if !valid_session_id(&session_id) {
        return Err("invalid kernel session UUID".into());
    }
    let total_limit = hal_limit()?;
    let plan = select(&inventory, 20_000_000_000)?;
    DirBuilder::new().mode(0o700).create(&directory)?;
    File::open(directory.parent().ok_or("missing capture parent")?)?.sync_all()?;
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let mut metadata = json!({"version":1,"phase":"started","boot_id":boot_id.trim(),"session_id":session_id,
        "selected":plan,"seconds_after_activation":seconds,"flush_interval_seconds":20,
        "total_bytes":total_limit,"reserve_bytes":RESERVE,
        "scope":"HAL types only; SSC separate","start_boottime_seconds":now()?});
    persist(&directory, metadata.clone())?;
    let mut opened = false;
    let mut activated = Vec::new();
    let mut periodic_flushes = 0u64;
    let mut max_flush_seconds = 0.0f64;
    let outcome = (|| -> Result<&str> {
        owned_request(
            &socket,
            &session_id,
            json!({"command":"open","directory":directory,"total_bytes":total_limit,"reserve_bytes":RESERVE}),
        )?;
        opened = true;
        let deadline = now()? + 10.0;
        loop {
            if waiter.interrupted()? {
                return Ok("signal_during_startup");
            }
            let s = owned_request(&socket, &session_id, json!({"command":"status"}))?;
            healthy(&s)?;
            if s["storage_status"] == 1 {
                break;
            }
            if now()? >= deadline {
                return Err("storage startup deadline".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        for sensor in &plan {
            if waiter.interrupted()? {
                return Ok("signal_during_startup");
            }
            owned_request(
                &socket,
                &session_id,
                json!({"command":"demand","handle":sensor["sensor"]["handle"],"active":true,
                "period_ns":sensor["period_ns"],"latency_ns":sensor["latency_ns"]}),
            )?;
            activated.push(sensor["sensor"]["handle"].clone());
        }
        if waiter.interrupted()? {
            return Ok("signal_during_startup");
        }
        healthy(&owned_request(
            &socket,
            &session_id,
            json!({"command":"status"}),
        )?)?;
        metadata["activated_handles"] = json!(activated);
        metadata["activation_complete_boottime_seconds"] = json!(now()?);
        persist(&directory, metadata.clone())?;
        notify_ready(std::env::var_os("NOTIFY_SOCKET").as_deref())?;
        eprintln!("recording {} HAL types", plan.len());
        let deadline = if seconds == 0 {
            None
        } else {
            Some(now()? + seconds as f64)
        };
        loop {
            let remaining = match deadline {
                Some(t) => t - now()?,
                None => 20.0,
            };
            if remaining <= 0.0 {
                return Ok("duration");
            }
            if waiter.wait(remaining.min(20.0))? {
                return Ok("signal");
            }
            if let Some(t) = deadline {
                if now()? >= t {
                    return Ok("duration");
                }
            }
            let began = now()?;
            owned_request(&socket, &session_id, json!({"command":"flush"}))?;
            status_until(&socket, &session_id, false)?;
            periodic_flushes += 1;
            max_flush_seconds = max_flush_seconds.max(now()? - began);
        }
    })();
    // Only drain a capture whose open was acknowledged; a rejected open may
    // belong to another controller. An ambiguous transport failure is unfinished.
    let stopped = if opened {
        stop_owned(&socket, &session_id)
    } else {
        Err("capture open not acknowledged; no demands activated".into())
    };
    metadata["phase"] = json!(if outcome.is_ok() && stopped.is_ok() {
        "closed"
    } else {
        "failed"
    });
    metadata["activated_handles"] = json!(activated);
    metadata["periodic_flushes"] = json!(periodic_flushes);
    metadata["max_flush_seconds"] = json!(max_flush_seconds);
    metadata["end_boottime_seconds"] = json!(now()?);
    metadata["end_reason"] = json!(match &outcome {
        Ok(reason) => reason.to_string(),
        Err(e) => e.to_string(),
    });
    metadata["final_status"] = match &stopped {
        Ok(s) => s.clone(),
        Err(e) => json!({"error":e.to_string()}),
    };
    persist(&directory, metadata)?;
    outcome?;
    stopped?;
    eprintln!("capture finalized");
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        if e.is::<suspend_runtime::SuspendCooldown>() {
            std::process::exit(77);
        }
        eprintln!("recorder: {e}");
        std::process::exit(if e.is::<LeaseBusy>() {
            75
        } else if e.is::<suspend_runtime::SuspendRetry>() {
            76
        } else {
            1
        });
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn interrupted_publications_do_not_block_controller_or_recovery_updates() {
        let root = std::env::temp_dir().join(format!(
            "hoki-publication-{}",
            fs::read_to_string("/proc/sys/kernel/random/uuid").unwrap().trim()
        ));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        for name in ["controller", "recovery"] {
            let original = json!({"phase":"started"});
            persist_named(&root, name, original.clone()).unwrap();
            // A killed publisher can leave an incomplete, unpublished file.
            let abandoned = root.join(format!("{name}.pending"));
            fs::write(&abandoned, b"{\"phase\":").unwrap();
            let updated = json!({"phase":"failed","error":"interrupted"});
            let outcome = persist_named(&root, name, updated.clone());
            assert_eq!(fs::read(&abandoned).unwrap(), b"{\"phase\":");
            assert!(outcome.is_ok(), "retry blocked by abandoned {name} publication: {outcome:?}");
            let saved: Value = serde_json::from_slice(
                &fs::read(root.join(format!("{name}.json"))).unwrap()
            ).unwrap();
            assert_eq!(saved, updated);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
