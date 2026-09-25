//! One bounded suspend attempt. Caller owns UI/radio policy and session lifetime.
use super::{persist_named, private_json, private_parent};
use hoki_health_recorder::{
    owned_request,
    suspend_policy::{power_blocker, recording_readiness, RecordingReadiness, SuspendWrite},
    valid_session_id, Result,
};
#[derive(Debug)]
pub struct SuspendCooldown;
impl std::fmt::Display for SuspendCooldown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "suspend cooldown")
    }
}
impl std::error::Error for SuspendCooldown {}
fn cooldown(paced: bool) -> Result<()> {
    if paced {
        Err(SuspendCooldown.into())
    } else {
        Ok(())
    }
}
#[derive(Debug)]
pub struct SuspendRetry(io::Error);
impl std::fmt::Display for SuspendRetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "suspend retry: {}", self.0)
    }
}
impl std::error::Error for SuspendRetry {}

fn failed_write(
    capture: &Path,
    attempt: &str,
    boot: &str,
    session: &str,
    stage: SuspendWrite,
    error: io::Error,
) -> Result<()> {
    let retryable = stage.retryable(error.raw_os_error());
    // Publication errors stay hard failures; errno alone is not a cause diagnosis.
    persist_named(
        capture,
        &format!("suspend-{attempt}-result"),
        json!({
            "version":1,"boot_id":boot.trim(),"session_id":session,"result":"failed",
            "stage":stage.name(),"errno":error.raw_os_error(),"retryable":retryable,
            "suspend_requested":matches!(stage, SuspendWrite::Mem)
        }),
    )?;
    if retryable {
        Err(SuspendRetry(error).into())
    } else {
        Err(error.into())
    }
}
use serde_json::json;
use std::{
    fs, io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    process::Command,
};
fn property(unit: &str, key: &str) -> Result<String> {
    let p = Command::new("/usr/bin/systemctl")
        .args(["show", unit, "--value", "-p", key])
        .output()?;
    if !p.status.success() {
        return Err("cannot inspect suspend supervisor".into());
    }
    Ok(std::str::from_utf8(&p.stdout)?.trim().to_owned())
}
fn power() -> Result<(String, String)> {
    Ok((
        fs::read_to_string("/sys/class/power_supply/battery/status")?,
        fs::read_to_string("/sys/class/android_usb/android0/state")?,
    ))
}
fn clock(id: libc::clockid_t) -> Result<f64> {
    let mut t: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(id, &mut t) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(t.tv_sec as f64 + t.tv_nsec as f64 / 1e9)
}
fn remaining(fd: i32) -> Result<f64> {
    let mut t: libc::itimerspec = unsafe { std::mem::zeroed() };
    if unsafe { libc::timerfd_gettime(fd, &mut t) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(t.it_value.tv_sec as f64 + t.it_value.tv_nsec as f64 / 1e9)
}
pub fn run(socket: &Path, capture: &Path, paced: bool) -> Result<()> {
    let unit = std::env::var("HOKI_SUSPEND_SUPERVISOR")?;
    let id = unit
        .strip_prefix("hoki-recording-suspend-")
        .and_then(|s| s.strip_suffix(".service"))
        .ok_or("wrong suspend unit scope")?;
    if unsafe { libc::geteuid() } != 0
        || !valid_session_id(id)
        || property(&unit, "MainPID")? != std::process::id().to_string()
        || property(&unit, "ActiveState")? != "active"
        || property(&unit, "Type")? != "exec"
        || property(&unit, "RuntimeMaxUSec")? != "30s"
        || property(&unit, "TimeoutStopUSec")? != "5s"
        || property(&unit, "KillMode")? != "control-group"
    {
        return Err("suspend requires its bounded Type=exec supervisor".into());
    }
    let seconds = std::env::var("HOKI_SUSPEND_SECONDS")
        .unwrap_or_else(|_| "10".into())
        .parse::<u64>()?;
    if !(3..=20).contains(&seconds) {
        return Err("suspend interval must be3..20 seconds".into());
    }
    let (battery, usb) = power()?;
    if let Some(reason) = power_blocker(&battery, &usb) {
        println!(
            "{}",
            json!({"result":"skipped","reason":reason,"battery":battery.trim(),"usb":usb.trim(),"recording_verified":false,"suspend_requested":false})
        );
        return cooldown(paced);
    }
    if !socket.is_absolute() || !capture.is_absolute() {
        return Err("absolute recording paths required".into());
    }
    private_parent(socket)?;
    private_parent(capture)?;
    let meta = private_json(&capture.join("controller.json"))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let session = meta["session_id"]
        .as_str()
        .ok_or("missing recording owner")?;
    let check = || -> Result<RecordingReadiness> {
        let current = private_json(&capture.join("controller.json"))?;
        if current["session_id"] != session {
            return Err("recording identity changed".into());
        }
        let status = owned_request(socket, session, json!({"command":"status"}))?;
        recording_readiness(&current, boot.trim(), &status)
    };
    if check()? == RecordingReadiness::PendingDurability {
        println!(
            "{}",
            json!({"result":"skipped","reason":"recording_pending_durability",
                             "suspend_requested":false})
        );
        return cooldown(paced);
    }
    let raw = unsafe {
        libc::timerfd_create(
            libc::CLOCK_BOOTTIME_ALARM,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let alarm = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut spec: libc::itimerspec = unsafe { std::mem::zeroed() };
    spec.it_value.tv_sec = seconds as _;
    if unsafe { libc::timerfd_settime(raw, 0, &spec, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if remaining(raw)? < 2.0 {
        return Err("alarm not armed".into());
    }
    let attempt = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    if !valid_session_id(&attempt) {
        return Err("invalid suspend attempt identity".into());
    }
    persist_named(
        capture,
        &format!("suspend-{attempt}-intent"),
        json!({
            "version":1,"boot_id":boot.trim(),"session_id":session,"supervisor":unit,
            "alarm_seconds":seconds,"boottime_seconds":clock(libc::CLOCK_BOOTTIME)?,
            "meaning":"intent only; suspend entry not yet attempted"
        }),
    )?;
    // This read can block behind wake holds; mandatory service bounds contain it.
    let count = fs::read_to_string("/sys/power/wakeup_count")?;
    if check()? == RecordingReadiness::PendingDurability {
        persist_named(
            capture,
            &format!("suspend-{attempt}-result"),
            json!({"version":1,"boot_id":boot.trim(),"session_id":session,
                   "result":"skipped","reason":"recording_pending_durability",
                   "suspend_requested":false}),
        )?;
        return cooldown(paced);
    }
    let (battery, usb) = power()?;
    if power_blocker(&battery, &usb).is_some() {
        return Err("power connection changed before suspend".into());
    }
    // Only a valid kernel counter can be classified as an EINVAL handshake race.
    let count = count.trim().parse::<u32>()?;
    if let Err(error) = fs::write("/sys/power/wakeup_count", count.to_string()) {
        return failed_write(
            capture,
            &attempt,
            &boot,
            session,
            SuspendWrite::WakeupCount,
            error,
        );
    }
    if remaining(alarm.as_raw_fd())? < 2.0 {
        return Err("alarm expired or too close before suspend".into());
    }
    let b = clock(libc::CLOCK_BOOTTIME)?;
    let m = clock(libc::CLOCK_MONOTONIC)?;
    println!(
        "{}",
        json!({"result":"attempt","boot_id":boot.trim(),"session_id":session,"alarm_seconds":seconds,"boottime_seconds":b})
    );
    if let Err(error) = fs::write("/sys/power/state", b"mem") {
        return failed_write(capture, &attempt, &boot, session, SuspendWrite::Mem, error);
    }
    let end_boottime = clock(libc::CLOCK_BOOTTIME)?;
    let elapsed = end_boottime - b;
    let awake = clock(libc::CLOCK_MONOTONIC)? - m;
    let mut ticks = 0u64;
    let n = unsafe { libc::read(raw, (&mut ticks as *mut u64).cast(), 8) };
    println!(
        "{}",
        json!({"result":"returned","start_boottime_seconds":b,"end_boottime_seconds":end_boottime,
               "elapsed_seconds":elapsed,"awake_seconds":awake,"suspended_estimate_seconds":elapsed-awake,"alarm_expired":n==8 && ticks>0})
    );
    persist_named(
        capture,
        &format!("suspend-{attempt}-result"),
        json!({
            "version":1,"boot_id":boot.trim(),"session_id":session,"result":"returned",
            "start_boottime_seconds":b,"end_boottime_seconds":end_boottime,
            "elapsed_seconds":elapsed,"awake_seconds":awake,"suspended_estimate_seconds":elapsed-awake,
            "alarm_expired":n==8 && ticks>0
        }),
    )?;
    if hoki_health_recorder::suspend_policy::return_needs_cooldown(elapsed, awake) {
        return cooldown(paced);
    }
    Ok(())
}
