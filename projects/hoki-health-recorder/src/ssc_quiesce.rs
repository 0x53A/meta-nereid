//! Stop only recorded SSC children of an already stopped session supervisor.
use crate::{
    ssc_helper::{private_json, supervisor_owner},
    valid_session_id, Result,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::Command;
fn state(unit: &str) -> Result<BTreeMap<String, String>> {
    let output = Command::new("/usr/bin/systemctl")
        .args([
            "show",
            unit,
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "-p",
            "SubState",
            "-p",
            "MainPID",
            "-p",
            "ControlPID",
        ])
        .output()?;
    let mut fields = BTreeMap::new();
    for line in std::str::from_utf8(&output.stdout)?.lines() {
        let (k, v) = line.split_once('=').ok_or("invalid systemd state")?;
        if fields.insert(k.into(), v.into()).is_some() {
            return Err("duplicate systemd state field".into());
        }
    }
    if !output.status.success() && fields.get("LoadState").map(String::as_str) != Some("not-found")
    {
        return Err("failed to query helper service state".into());
    }
    Ok(fields)
}
fn stopped(fields: &BTreeMap<String, String>) -> bool {
    matches!(
        fields.get("ActiveState").map(String::as_str),
        Some("inactive" | "failed")
    ) && fields.get("MainPID").map(String::as_str) == Some("0")
        && fields.get("ControlPID").map(String::as_str) == Some("0")
}
pub fn quiesce(root: &Path, owner: &str) -> Result<()> {
    let m = fs::symlink_metadata(root)?;
    if unsafe { libc::geteuid() } != 0
        || !root.is_absolute()
        || !m.is_dir()
        || m.uid() != 0
        || m.mode() & 0o077 != 0
        || !valid_session_id(owner)
    {
        return Err("invalid helper recovery scope".into());
    }
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let mut units = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("launch.json");
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        let launch = private_json(&path)?;
        let unit = launch["unit"].as_str().ok_or("missing child unit")?;
        let id = unit
            .strip_prefix("hoki-ssc-")
            .and_then(|s| s.strip_suffix(".service"))
            .ok_or("invalid child unit scope")?;
        let supervisor = launch["supervisor"]
            .as_str()
            .ok_or("helper lacks session association")?;
        if launch["version"] != 1
            || launch["boot_id"] != boot.trim()
            || !valid_session_id(id)
            || supervisor_owner(supervisor)? != owner
        {
            return Err("helper recovery identity mismatch".into());
        }
        units.push((unit.to_string(), supervisor.to_string()));
    }
    // Validate every parent before stopping anything. An active owner's helpers
    // must not be stolen even if the configuration lease was not acquired yet.
    for (_, parent) in &units {
        if !stopped(&state(parent)?) {
            return Err("helper supervisor has not stopped".into());
        }
    }
    let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    if !valid_session_id(&id) {
        return Err("invalid cleanup UUID".into());
    }
    let mut log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(format!("quiesce-{id}.jsonl")))?;
    File::open(root)?.sync_all()?;
    for (unit, parent) in units {
        let result = Command::new("/usr/bin/systemctl")
            .args(["stop", &unit])
            .output()?;
        let after = state(&unit)?;
        let confirmed = stopped(&after);
        serde_json::to_writer(
            &mut log,
            &json!({"unit":unit,"supervisor":parent,"stop_exit_code":result.status.code(),"state":after,"confirmed_stopped":confirmed}),
        )?;
        log.write_all(b"\n")?;
        log.sync_all()?;
        if !confirmed {
            return Err("SSC helper has not stopped; refusing restoration".into());
        }
    }
    log.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_confirmation_requires_terminal_state_and_no_live_pids() {
        let mut s = BTreeMap::from([
            ("ActiveState".into(), "inactive".into()),
            ("MainPID".into(), "0".into()),
            ("ControlPID".into(), "0".into()),
        ]);
        assert!(stopped(&s));
        s.insert("ActiveState".into(), "failed".into());
        assert!(stopped(&s));
        s.insert("ControlPID".into(), "42".into());
        assert!(!stopped(&s));
        s.insert("ControlPID".into(), "0".into());
        s.insert("ActiveState".into(), "deactivating".into());
        assert!(!stopped(&s));
        s.insert("ActiveState".into(), "inactive".into());
        s.remove("MainPID");
        assert!(!stopped(&s));
    }
}
