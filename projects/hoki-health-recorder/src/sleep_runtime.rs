//! Bounded configuration trial lifecycle; recording/suspend services remain separate.
use super::{persist_named, private_json, private_parent};
use hoki_health_recorder::{
    sleep_backend::LiveBackend, sleep_transaction::Transaction, ssc_helper::Helper,
    valid_session_id, Result,
};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::Command;
fn uuid() -> Result<String> {
    let s = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    if !valid_session_id(&s) {
        return Err("invalid UUID".into());
    }
    Ok(s)
}
fn simple_path(path: &Path) -> Result<&str> {
    let s = path.to_str().ok_or("non-UTF8 service path")?;
    if !path.is_absolute()
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return Err("unsupported service path syntax".into());
    }
    Ok(s)
}
fn save_text(path: &Path, text: &str) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    Ok(())
}
fn systemctl(args: &[&str]) -> Result<()> {
    if !Command::new("/usr/bin/systemctl")
        .args(args)
        .status()?
        .success()
    {
        return Err("sleep lifecycle systemctl command failed".into());
    }
    Ok(())
}
fn property(unit: &str, key: &str) -> Result<String> {
    let output = Command::new("/usr/bin/systemctl")
        .args(["show", unit, "--value", "-p", key])
        .output()?;
    if !output.status.success() {
        return Err("cannot inspect sleep service".into());
    }
    Ok(std::str::from_utf8(&output.stdout)?.trim().to_string())
}
pub fn prepare(plan_dir: &Path, root: &Path) -> Result<()> {
    simple_path(root)?;
    private_parent(root)?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_string();
    let owner = uuid()?;
    let attempt = uuid()?;
    let plan = private_json(&plan_dir.join("plan.json"))?;
    let transaction = Transaction::new(plan, &boot, &owner)?;
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let executable = simple_path(&executable)?;
    let helper = std::env::var("HOKI_SSC_HELPER")?;
    simple_path(Path::new(&helper))?;
    let duration = std::env::var("HOKI_SLEEP_SECONDS")
        .unwrap_or_else(|_| "30".into())
        .parse::<u64>()?;
    let service_seconds = super::profile_alarm::service_seconds(duration)?;
    DirBuilder::new().mode(0o700).create(root)?;
    File::open(root.parent().ok_or("missing trial parent")?)?.sync_all()?;
    Helper::new(Path::new(&helper), root)?;
    let session = fs::canonicalize(root)?;
    let session = simple_path(&session)?;
    let apply = format!("hoki-sleep-{owner}.service");
    let restore = format!("hoki-sleep-restore-{owner}-{attempt}.service");
    persist_named(root, "transaction", transaction.journal().clone())?;
    let runtime = json!({"version":1,"owner":owner,"boot_id":boot,"session":session,"controller":executable,"helper":helper,"duration_seconds":duration,"apply_unit":apply,"restore_unit":restore});
    persist_named(root, "runtime", runtime)?;
    save_text(&root.join(&apply),&format!("[Unit]\nDescription=Bounded Hoki sleep configuration session\n[Service]\nType=exec\nEnvironment=HOKI_SSC_SUPERVISOR={apply}\nExecStart={executable} --apply-sleep {session} {owner}\nExecStopPost=/usr/bin/systemctl --no-block start {restore}\nRuntimeMaxSec={service_seconds}\nTimeoutStartSec=30\nTimeoutStopSec=15\nKillMode=control-group\nSendSIGKILL=yes\n"))?;
    save_text(&root.join(&restore),&format!("[Unit]\nDescription=Restore Hoki sleep configuration\nAfter={apply}\n[Service]\nType=exec\nEnvironment=HOKI_SSC_SUPERVISOR={restore}\nExecStart={executable} --restore-sleep {session} {owner}\nRuntimeMaxSec=360\nTimeoutStartSec=30\nTimeoutStopSec=15\nKillMode=control-group\nSendSIGKILL=yes\n"))?;
    File::open(root)?.sync_all()?;
    println!("{owner}");
    Ok(())
}
pub fn run(mode: &str, root: &Path, owner: &str) -> Result<()> {
    simple_path(root)?;
    let runtime = private_json(&root.join("runtime.json"))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_string();
    let apply = format!("hoki-sleep-{owner}.service");
    let restore = runtime["restore_unit"]
        .as_str()
        .ok_or("missing recovery unit")?;
    let attempt = restore
        .strip_prefix(&format!("hoki-sleep-restore-{owner}-"))
        .and_then(|s| s.strip_suffix(".service"))
        .ok_or("wrong recovery unit scope")?;
    if !valid_session_id(owner)
        || !valid_session_id(attempt)
        || runtime["version"] != 1
        || runtime["owner"] != owner
        || runtime["boot_id"] != boot
        || runtime["apply_unit"] != apply
        || runtime["session"]
            != fs::canonicalize(root)?
                .to_str()
                .ok_or("invalid session path")?
    {
        return Err("sleep runtime identity mismatch".into());
    }
    let helper = runtime["helper"].as_str().ok_or("missing SSC helper")?;
    simple_path(Path::new(helper))?;
    let duration = runtime["duration_seconds"]
        .as_u64()
        .ok_or("invalid trial duration")?;
    super::profile_alarm::service_seconds(duration)?;
    let journal = private_json(&root.join("transaction.json"))?;
    let mut transaction = Transaction::load(journal, &boot, owner)?;
    if mode == "--launch-sleep" {
        if transaction.journal()["phase"] != "prepared" {
            return Err("sleep trial already started".into());
        }
        let apply_path = root.join(&apply);
        let restore_path = root.join(restore);
        systemctl(&[
            "link",
            "--runtime",
            simple_path(&apply_path)?,
            simple_path(&restore_path)?,
        ])?;
        systemctl(&["daemon-reload"])?;
        for unit in [&apply, restore] {
            if fs::canonicalize(property(unit, "FragmentPath")?)?
                != fs::canonicalize(root.join(unit))?
            {
                return Err("sleep service loaded from unexpected path".into());
            }
        }
        return systemctl(&["start", &apply]);
    }
    if mode == "--await-sleep" {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            if property(&apply, "ActiveState")? != "active" {
                return Err("configuration service stopped before recording readiness".into());
            }
            let current =
                Transaction::load(private_json(&root.join("transaction.json"))?, &boot, owner)?;
            if current.journal()["phase"] == "active" {
                match fs::symlink_metadata(root.join("ready.json")) {
                    Ok(_) => {
                        let ready = private_json(&root.join("ready.json"))?;
                        check_ready(
                            &ready,
                            owner,
                            &boot,
                            &property(&apply, "MainPID")?,
                            &property(&apply, "InvocationID")?,
                        )?;
                        return Ok(());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            } else if !["prepared", "enabling"]
                .contains(&current.journal()["phase"].as_str().unwrap_or(""))
            {
                return Err("configuration session is no longer activating".into());
            }
            if std::time::Instant::now() >= deadline {
                return Err("configuration readiness timeout".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let expected = if mode == "--apply-sleep" {
        apply.as_str()
    } else {
        restore
    };
    if std::env::var("HOKI_SSC_SUPERVISOR")? != expected
        || property(expected, "MainPID")? != std::process::id().to_string()
        || property(expected, "ActiveState")? != "active"
    {
        return Err("sleep controller is not running in its expected service".into());
    }
    if mode == "--apply-sleep"
        && (property(restore, "LoadState")? != "loaded"
            || fs::canonicalize(property(restore, "FragmentPath")?)?
                != fs::canonicalize(root.join(restore))?)
    {
        return Err("restoration service is not loaded".into());
    }
    let mut backend = LiveBackend::new(Path::new(helper), root, owner, expected)?;
    if mode == "--apply-sleep" {
        // Verify wake-alarm support before modifying firmware configuration.
        let alarm = super::profile_alarm::ProfileAlarm::new()?;
        transaction.enable(&mut backend)?;
        alarm.arm(duration)?;
        // Created only AFTER the active transaction checkpoint's fsync returned.
        // The gate also checks live PID/invocation to reject stale ready files.
        let invocation = property(expected, "InvocationID")?;
        let ready = json!({"version":1,"boot_id":boot,"owner":owner,"activation_verified":true,
            "pid":std::process::id(),"invocation_id":invocation,"activation_ready_boottime":super::now()?});
        check_ready(
            &ready,
            owner,
            &boot,
            &std::process::id().to_string(),
            &invocation,
        )?;
        persist_named(root, "ready", ready)?;
        // This bounded research lifecycle does not control CPU suspend. Its service
        // timeout and independent ExecStopPost recovery also cover SIGKILL/error.
        alarm.wait()
    } else {
        transaction.restore(&mut backend)
    }
}

fn check_ready(ready: &Value, owner: &str, boot: &str, pid: &str, invocation: &str) -> Result<()> {
    let pid = pid.parse::<u32>()?;
    if pid == 0
        || invocation.len() != 32
        || !invocation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || ready["version"] != 1
        || ready["boot_id"] != boot
        || ready["owner"] != owner
        || ready["activation_verified"] != true
        || ready["pid"].as_u64() != Some(pid as u64)
        || ready["invocation_id"] != invocation
    {
        return Err("configuration readiness identity mismatch".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_rejects_stale_process_invocation_boot_and_unverified_activation() {
        let owner = "12345678-1234-1234-1234-123456789abc";
        let invocation = "0123456789abcdef0123456789abcdef";
        let ready = json!({"version":1,"owner":owner,"boot_id":owner,"pid":42,"invocation_id":invocation,"activation_verified":true});
        assert!(check_ready(&ready, owner, owner, "42", invocation).is_ok());
        for (field, value) in [
            ("pid", json!(43)),
            ("boot_id", json!("old")),
            ("owner", json!("other")),
            ("activation_verified", json!(false)),
            ("invocation_id", json!("fedcba9876543210fedcba9876543210")),
        ] {
            let mut bad = ready.clone();
            bad[field] = value;
            assert!(check_ready(&bad, owner, owner, "42", invocation).is_err());
        }
        assert!(check_ready(&ready, owner, owner, "0", invocation).is_err());
        assert!(check_ready(&ready, owner, owner, "42", "invalid").is_err());
    }
}
