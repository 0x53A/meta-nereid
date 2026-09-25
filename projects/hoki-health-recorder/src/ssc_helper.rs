//! Supervised, durable helper captures. Setters are restricted to transaction backend.
use crate::{select_ssc, valid_session_id, Result};
use serde_json::Value;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn private_json(path: &Path) -> Result<Value> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let m = f.metadata()?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o077 != 0 || m.nlink() != 1 || m.len() > 131072 {
        return Err("unsafe SSC metadata".into());
    }
    Ok(serde_json::from_reader(f.take(131073))?)
}
fn private_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !path.is_absolute() || !m.is_dir() || m.uid() != 0 || m.mode() & 0o077 != 0 {
        return Err("SSC capture parent must be root-owned and private".into());
    }
    Ok(())
}
fn boot_id() -> Result<String> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_string();
    if !valid_session_id(&boot) {
        return Err("invalid boot identity".into());
    }
    Ok(boot)
}
pub(crate) fn supervisor_owner(unit: &str) -> Result<&str> {
    let id = unit
        .strip_prefix("hoki-sleep-")
        .and_then(|s| s.strip_suffix(".service"))
        .ok_or("invalid sleep supervisor unit")?;
    let id = if let Some(rest) = id.strip_prefix("restore-") {
        if valid_session_id(rest) {
            rest
        } else {
            let owner = rest.get(..36).ok_or("invalid recovery owner")?;
            let attempt = rest.get(37..).ok_or("invalid recovery attempt")?;
            if rest.get(36..37) != Some("-") || !valid_session_id(attempt) {
                return Err("invalid recovery attempt".into());
            }
            owner
        }
    } else {
        id
    };
    if !valid_session_id(id) {
        return Err("invalid sleep supervisor identity".into());
    }
    Ok(id)
}
pub struct Helper {
    executable: PathBuf,
    root: PathBuf,
    boot: String,
    supervisor: Option<String>,
}
impl Helper {
    pub fn new(executable: &Path, root: &Path) -> Result<Self> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("root required".into());
        }
        let m = fs::symlink_metadata(executable)?;
        // ExecStopPost is parsed by systemd; restrict path grammar rather than
        // interpolate spaces, specifiers or shell-like syntax into that property.
        let text = executable.to_str().ok_or("non-UTF8 helper path")?;
        if !executable.is_absolute()
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            || !m.is_file()
            || m.uid() != 0
            || m.mode() & 0o022 != 0
            || m.mode() & 0o111 == 0
        {
            return Err("unsafe SSC executable".into());
        }
        private_directory(root)?;
        Ok(Self {
            executable: executable.into(),
            root: root.into(),
            boot: boot_id()?,
            supervisor: None,
        })
    }
    pub fn with_supervisor(mut self, unit: &str) -> Result<Self> {
        supervisor_owner(unit)?;
        self.supervisor = Some(unit.to_string());
        Ok(self)
    }
    pub fn boot(&self) -> &str {
        &self.boot
    }
    pub fn capture(&self, name: &str, mode: Option<(&str, &str)>) -> Result<PathBuf> {
        self.run_capture(name, mode, &[])
    }
    pub(crate) fn control(&self, name: &str, op: &Value, restore: bool) -> Result<PathBuf> {
        if self.supervisor.is_none() {
            return Err("SSC setter requires a session supervisor".into());
        }
        let (mode, env) = control_arguments(op, restore)?;
        self.run_capture(
            name,
            Some((mode, op["source"].as_str().ok_or("missing control source")?)),
            &env,
        )
    }
    fn run_capture(
        &self,
        name: &str,
        mode: Option<(&str, &str)>,
        env: &[String],
    ) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err("invalid capture name".into());
        }
        if let Some((mode, suid)) = mode {
            if !(if env.is_empty() {
                &["--user-config", "--tracking-config", "--detect-config"][..]
            } else {
                &["--set-permissions", "--set-tracking", "--set-detect"][..]
            })
            .contains(&mode)
                || suid.len() != 36
                || !suid
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                || &suid[..2] != "09"
                || &suid[18..20] != "11"
            {
                return Err("invalid read-only SSC request".into());
            }
        }
        if boot_id()? != self.boot {
            return Err("boot changed before SSC capture".into());
        }
        let directory = self.root.join(name);
        DirBuilder::new().mode(0o700).create(&directory)?;
        File::open(&self.root)?.sync_all()?;
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("supervisor.log"))?;
        let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_string();
        if !valid_session_id(&id) {
            return Err("invalid service identity".into());
        }
        let unit = format!("hoki-ssc-{id}.service");
        let launch = serde_json::json!({"version":1,"boot_id":self.boot,"unit":unit,"supervisor":self.supervisor});
        let mut intent = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("launch.json"))?;
        serde_json::to_writer(&mut intent, &launch)?;
        intent.write_all(b"\n")?;
        intent.sync_all()?;
        File::open(&directory)?.sync_all()?;
        let mut command = Command::new("/usr/bin/systemd-run");
        if let Some(supervisor) = &self.supervisor {
            // Requisite refuses late starts after the owner stops, without
            // pulling that owner back up. PartOf propagates stop; After orders it.
            command
                .arg(format!("--property=Requisite={supervisor}"))
                .arg(format!("--property=After={supervisor}"))
                .arg(format!("--property=PartOf={supervisor}"));
        }
        command
            .args([
                "--wait",
                "--collect",
                "--property=Type=exec",
                "--property=RuntimeMaxSec=20",
                "--property=TimeoutStartSec=10",
                "--property=TimeoutStopSec=10",
                "--property=KillMode=control-group",
                "--property=SendSIGKILL=yes",
            ])
            .arg(format!("--unit={unit}"))
            .arg(format!(
                "--property=ExecStopPost={} --cleanup",
                self.executable.display()
            ))
            .args([
                "/usr/bin/env",
                "-i",
                "LD_LIBRARY_PATH=/vendor/lib:/system/lib",
            ])
            .arg(format!("SSC_JOURNAL_DIR={}", directory.display()));
        command.args(env).arg(&self.executable);
        if let Some((mode, suid)) = mode {
            command.args([mode, suid]);
        }
        // Log to a file: a broken helper cannot fill a pipe and deadlock supervision.
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log.try_clone()?));
        let status = command.status();
        log.sync_all()?;
        let mut result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("supervisor-result.json"))?;
        let evidence = serde_json::json!({"version":1,"unit":unit,"boot_id":self.boot,
            "success":status.as_ref().is_ok_and(|s|s.success()),
            "exit_code":status.as_ref().ok().and_then(|s|s.code()),
            "spawn_error":status.as_ref().err().map(|e|e.to_string())});
        serde_json::to_writer(&mut result, &evidence)?;
        result.write_all(b"\n")?;
        result.sync_all()?;
        File::open(&directory)?.sync_all()?;
        if !status?.success() {
            return Err("SSC supervised capture failed; evidence preserved".into());
        }
        if boot_id()? != self.boot {
            return Err("boot changed during SSC capture".into());
        }
        Ok(directory)
    }
    /// Build the exact baseline tree consumed by --plan-sleep. No setter runs.
    pub fn snapshot(&self) -> Result<()> {
        let discovery = self.capture("discovery", None)?;
        let inventory = private_json(&discovery.join("inventory.json"))?;
        let status = private_json(&discovery.join("status.json"))?;
        let cfg = select_ssc(&inventory, &status, &self.boot, "fsl_cfg")?;
        let sleep = select_ssc(&inventory, &status, &self.boot, "fsl_sleep")?;
        for (name, mode, suid, event) in [
            ("user", "--user-config", &cfg, 769),
            ("tracking", "--tracking-config", &sleep, 776),
            ("detect", "--detect-config", &sleep, 876),
        ] {
            let path = self.capture(name, Some((mode, suid)))?;
            validate_snapshot(
                &private_json(&path.join("config.json"))?,
                &private_json(&path.join("status.json"))?,
                &self.boot,
                mode,
                suid,
                event,
            )?;
        }
        Ok(())
    }
}
pub fn validate_snapshot(
    snapshot: &Value,
    status: &Value,
    boot: &str,
    mode: &str,
    source: &str,
    event: u32,
) -> Result<String> {
    let accepted = status["accepted"].as_u64().ok_or("missing SSC count")?;
    if status["version"] != 1
        || status["phase"] != "closed"
        || status["archive_complete"] != true
        || status["archive_error"] != 0
        || status["rejected"] != 0
        || accepted == 0
        || status["durable"].as_u64() != Some(accepted)
        || status["accepted_not_confirmed_durable"] != 0
    {
        return Err("incomplete configuration archive".into());
    }
    if snapshot["version"] != 1
        || snapshot["boot_id"] != boot
        || snapshot["mode"] != mode
        || snapshot["source"] != source
        || snapshot["event_id"] != event
        || !snapshot["session_id"]
            .as_str()
            .is_some_and(valid_session_id)
    {
        return Err("configuration snapshot identity mismatch".into());
    }
    let hex = snapshot["payload_hex"]
        .as_str()
        .ok_or("missing snapshot payload")?;
    if hex.is_empty()
        || hex.len() > 8192
        || hex.len() % 2 != 0
        || !hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalid snapshot payload".into());
    }
    Ok(hex.to_string())
}

/// Convert validated plan payloads to the helper's narrow environment interface.
/// Parse the payload itself so changed_fields cannot silently alter what is sent.
fn control_arguments(op: &Value, restore: bool) -> Result<(&'static str, Vec<String>)> {
    let payload = op[if restore {
        "restore_payload_hex"
    } else {
        "enable_payload_hex"
    }]
    .as_str()
    .ok_or("missing control payload")?;
    if !payload
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalid control payload encoding".into());
    }
    let kind = op["kind"].as_str().ok_or("missing control kind")?;
    let mut env = vec!["SSC_CONFIG_RESTORE_ARMED=1".to_string()];
    let mode = match kind {
        "tracking" | "detect" => {
            if !["0800", "0801"].contains(&payload) {
                return Err("invalid boolean control payload".into());
            }
            let id = if kind == "tracking" { 776 } else { 876 };
            if op["request_id"] != id {
                return Err("control message mismatch".into());
            }
            env.push(format!("SSC_CONFIG_VALUE={}", &payload[3..]));
            if kind == "tracking" {
                "--set-tracking"
            } else {
                "--set-detect"
            }
        }
        "user" => {
            if op["request_id"] != 768
                || !matches!(payload.len(), 8 | 12)
                || !payload.starts_with(if payload.len() == 8 { "1a02" } else { "1a04" })
            {
                return Err("invalid permission control payload".into());
            }
            let mut previous = "";
            for at in (4..payload.len()).step_by(4) {
                let pair = payload
                    .get(at..at + 4)
                    .ok_or("invalid permission encoding")?;
                let name = match &pair[..2] {
                    "18" => "SSC_RHR_PERMISSION",
                    "20" => "SSC_SLEEP_PERMISSION",
                    _ => return Err("unknown permission".into()),
                };
                if !["00", "01"].contains(&&pair[2..])
                    || (!previous.is_empty() && &pair[..2] <= previous)
                {
                    return Err("duplicate, unordered or invalid permission".into());
                }
                previous = &pair[..2];
                env.push(format!("{name}={}", &pair[3..]));
            }
            "--set-permissions"
        }
        _ => return Err("unknown control kind".into()),
    };
    Ok((mode, env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn supervisor_names_bind_to_one_owner_without_freeform_unit_injection() {
        let owner = "12345678-1234-1234-1234-123456789abc";
        for role in ["", "restore-"] {
            assert_eq!(
                supervisor_owner(&format!("hoki-sleep-{role}{owner}.service")).unwrap(),
                owner
            );
        }
        assert_eq!(
            supervisor_owner(&format!("hoki-sleep-restore-{owner}-{owner}.service")).unwrap(),
            owner
        );
        assert!(supervisor_owner(&format!("hoki-sleep-restore-{owner}-bad.service")).is_err());
        for bad in [
            "sensorfwd.service",
            "hoki-sleep-.service",
            "hoki-sleep-%i.service",
            "hoki-sleep-a.service other.service",
            "hoki-sleep-restore-restore-12345678-1234-1234-1234-123456789abc.service",
        ] {
            assert!(supervisor_owner(bad).is_err());
        }
    }
    #[test]
    fn control_arguments_preserve_partial_permissions_and_reject_other_writes() {
        let (i, d, s, a) = crate::sleep_plan::tests::fixture();
        let boot = s[0]["boot_id"].as_str().unwrap();
        let plan = crate::sleep_plan::prepare(&i, &d, &s, &a, boot).unwrap();
        for op in plan["operations"].as_array().unwrap() {
            for restore in [false, true] {
                let (mode, env) = control_arguments(op, restore).unwrap();
                let v = if restore { 0 } else { 1 };
                if op["kind"] == "user" {
                    assert_eq!(mode, "--set-permissions");
                    assert_eq!(
                        env,
                        vec![
                            "SSC_CONFIG_RESTORE_ARMED=1".to_string(),
                            format!("SSC_RHR_PERMISSION={v}"),
                            format!("SSC_SLEEP_PERMISSION={v}")
                        ]
                    );
                } else {
                    assert_eq!(
                        env,
                        vec![
                            "SSC_CONFIG_RESTORE_ARMED=1".to_string(),
                            format!("SSC_CONFIG_VALUE={v}")
                        ]
                    );
                }
            }
        }
        let mut op = plan["operations"][0].clone();
        op["enable_payload_hex"] = json!("1a022001");
        assert_eq!(
            control_arguments(&op, false).unwrap().1,
            vec!["SSC_CONFIG_RESTORE_ARMED=1", "SSC_SLEEP_PERMISSION=1"]
        );
        for bad in [
            "1a0418011801",
            "1a0420011801",
            "1a021802",
            "1a022801",
            "1a041801",
            "1a02éé",
            "1A021801",
            "0801",
        ] {
            op["enable_payload_hex"] = json!(bad);
            assert!(control_arguments(&op, false).is_err(), "{bad}");
        }
        let mut op = plan["operations"][1].clone();
        op["request_id"] = json!(876);
        assert!(control_arguments(&op, false).is_err());
    }
    #[test]
    fn snapshot_rejects_stale_identity_incomplete_archive_and_bad_payload() {
        let (_, _, snapshots, statuses) = crate::sleep_plan::tests::fixture();
        let s = &snapshots[1];
        let a = &statuses[1];
        let boot = s["boot_id"].as_str().unwrap();
        let source = s["source"].as_str().unwrap();
        let check =
            |s: &Value, a: &Value| validate_snapshot(s, a, boot, "--tracking-config", source, 776);
        assert!(check(s, a).is_ok());
        for (key, value) in [
            ("boot_id", json!("stale")),
            ("mode", json!("--detect-config")),
            ("source", snapshots[0]["source"].clone()),
            ("event_id", json!(876)),
            ("session_id", json!("invalid")),
            ("payload_hex", json!("ABC")),
            ("payload_hex", json!("")),
        ] {
            let mut bad = s.clone();
            bad[key] = value;
            assert!(check(&bad, a).is_err(), "{key}");
        }
        for (key, value) in [
            ("phase", json!("started")),
            ("archive_error", json!(5)),
            ("rejected", json!(1)),
            ("durable", json!(4)),
            ("accepted_not_confirmed_durable", json!(1)),
        ] {
            let mut bad = a.clone();
            bad[key] = value;
            assert!(check(s, &bad).is_err(), "{key}");
        }
    }
}
