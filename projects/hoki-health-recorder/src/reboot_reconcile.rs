//! Reconcile a prior-boot reservation only when fresh reads equal its baseline.
//! This path sends getters only; a remaining configuration change stays reserved.
use crate::{
    profile_owner::Reservation,
    sleep_plan,
    sleep_transaction::Transaction,
    ssc_helper::{private_json, supervisor_owner, Helper},
    Result,
};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::Command;

fn baseline_matches(journal: Value, current_plan: Value, current_boot: &str) -> Result<bool> {
    let old_boot = journal["boot_id"]
        .as_str()
        .ok_or("missing prior boot")?
        .to_owned();
    let owner = journal["owner"]
        .as_str()
        .ok_or("missing prior owner")?
        .to_owned();
    if old_boot == current_boot {
        return Err("same-boot recovery must use the normal transaction path".into());
    }
    let old = Transaction::load(journal, &old_boot, &owner)?;
    let current = Transaction::new(current_plan, current_boot, &owner)?;
    let a = old.journal()["plan"]["baseline_snapshots"]
        .as_array()
        .unwrap();
    let b = current.journal()["plan"]["baseline_snapshots"]
        .as_array()
        .unwrap();
    // Each plan independently binds getters to discovery on its own boot.
    // SUIDs may change across boots; compare semantic getter and full payload.
    Ok(a.iter().zip(b).all(|(a, b)| {
        ["mode", "event_id", "payload_hex"]
            .iter()
            .all(|key| a[key] == b[key])
    }))
}

fn property(unit: &str, key: &str) -> Result<String> {
    let output = Command::new("/usr/bin/systemctl")
        .args(["show", unit, "-p", key, "--value"])
        .output()?;
    if !output.status.success() {
        return Err("cannot inspect reconciliation service".into());
    }
    Ok(std::str::from_utf8(&output.stdout)?.trim().to_owned())
}

fn save(root: &Path, name: &str, value: &Value) -> Result<()> {
    let pending = root.join(format!("{name}.pending"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    // Names are fixed and the output directory is freshly created under the lock.
    fs::rename(pending, root.join(format!("{name}.json")))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

pub fn run(profile: &Path, output: &Path) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 || !profile.is_absolute() || !output.is_absolute() {
        return Err("root and absolute reconciliation paths required".into());
    }
    let _lock = crate::sleep_backend::lease(Path::new("/run/hoki-sleep-profile.lock"))?;
    let runtime = private_json(&profile.join("runtime.json"))?;
    let journal = private_json(&profile.join("transaction.json"))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned();
    let old_boot = runtime["boot_id"].as_str().ok_or("missing profile boot")?;
    let owner = runtime["owner"].as_str().ok_or("missing profile owner")?;
    if runtime["version"] != 1
        || old_boot == boot
        || runtime["session"]
            != fs::canonicalize(profile)?
                .to_str()
                .ok_or("invalid profile path")?
    {
        return Err("invalid or same-boot reconciliation identity".into());
    }
    Transaction::load(journal.clone(), old_boot, owner)?;
    let unit = std::env::var("HOKI_SSC_SUPERVISOR")?;
    if !unit.starts_with("hoki-sleep-restore-")
        || supervisor_owner(&unit)? != owner
        || runtime["restore_unit"] == unit
        || property(&unit, "MainPID")? != std::process::id().to_string()
        || property(&unit, "ActiveState")? != "active"
        || property(&unit, "Type")? != "exec"
        || property(&unit, "RuntimeMaxUSec")? != "2min"
        || property(&unit, "TimeoutStopUSec")? != "10s"
        || property(&unit, "KillMode")? != "control-group"
    {
        return Err("reconciliation requires a fresh bounded owner-qualified service".into());
    }
    let reservation = Reservation::existing(
        Path::new("/var/lib/hoki-sleep-profile"),
        profile,
        old_boot,
        owner,
    )?;
    let parent = output.parent().ok_or("missing reconciliation parent")?;
    let m = fs::symlink_metadata(parent)?;
    if !m.is_dir() || m.uid() != 0 || m.mode() & 0o077 != 0 {
        return Err("reconciliation parent must be root-owned and private".into());
    }
    DirBuilder::new().mode(0o700).create(output)?;
    File::open(parent)?.sync_all()?;
    save(
        output,
        "intent",
        &json!({"version":1,"current_boot":boot,"prior_runtime":runtime,
        "prior_transaction":journal,"result":"pending_fresh_reads","owner_released":false}),
    )?;
    let helper = Helper::new(
        Path::new(runtime["helper"].as_str().ok_or("missing helper")?),
        output,
    )?
    .with_supervisor(&unit)?;
    helper.snapshot()?;
    let inventory = private_json(&output.join("discovery/inventory.json"))?;
    let discovery_status = private_json(&output.join("discovery/status.json"))?;
    let mut snapshots = Vec::new();
    let mut statuses = Vec::new();
    for name in ["user", "tracking", "detect"] {
        snapshots.push(private_json(&output.join(name).join("config.json"))?);
        statuses.push(private_json(&output.join(name).join("status.json"))?);
    }
    let current = sleep_plan::prepare(
        &inventory,
        &discovery_status,
        &snapshots.try_into().unwrap(),
        &statuses.try_into().unwrap(),
        &boot,
    )?;
    let matches = baseline_matches(journal.clone(), current.clone(), &boot)?;
    reservation.verify()?;
    save(
        output,
        "assessment",
        &json!({"version":1,"current_boot":boot,"prior_boot":old_boot,
        "owner":owner,"current_plan":current,"baseline_matches":matches,
        "release_authorized":matches,"owner_released":false,"firmware_writes":0}),
    )?;
    if !matches {
        return Err(
            "current configuration differs from prior baseline; reservation retained".into(),
        );
    }
    // The assessment is durable before removal. A crash between removal and this
    // final result leaves explicit intent, never an invented success record.
    reservation.release()?;
    save(
        output,
        "result",
        &json!({"version":1,"current_boot":boot,"prior_boot":old_boot,
        "owner":owner,"owner_released":true,"firmware_writes":0,
        "result":"baseline_observed_after_reboot"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const NEW_BOOT: &str = "12345678-1234-1234-1234-123456789abb";
    #[test]
    fn cross_boot_requires_complete_matching_baselines() {
        let (i, d, s, a) = sleep_plan::tests::fixture();
        let old_boot = i["boot_id"].as_str().unwrap();
        let old_plan = sleep_plan::prepare(&i, &d, &s, &a, old_boot).unwrap();
        let journal = Transaction::new(old_plan.clone(), old_boot, old_boot)
            .unwrap()
            .journal()
            .clone();
        assert!(baseline_matches(journal.clone(), old_plan, old_boot).is_err());
        let mut ni = i.clone();
        ni["boot_id"] = json!(NEW_BOOT);
        let mut ns = s.clone();
        for item in &mut ns {
            item["boot_id"] = json!(NEW_BOOT);
        }
        let plan = sleep_plan::prepare(&ni, &d, &ns, &a, NEW_BOOT).unwrap();
        assert!(baseline_matches(journal.clone(), plan.clone(), NEW_BOOT).unwrap());
        let mut interrupted = journal.clone();
        interrupted["phase"] = json!("active");
        interrupted["states"] = json!(vec![
            "applied";
            interrupted["states"].as_array().unwrap().len()
        ]);
        assert!(baseline_matches(interrupted, plan.clone(), NEW_BOOT).unwrap());
        // A different current-boot SUID is acceptable only through a newly
        // validated inventory and matching getter source, never stale rebinding.
        let changed_suid = "09010203040506070811090a0b0c0d0e0f10";
        let mut rebound_inventory = ni.clone();
        for entry in rebound_inventory["streams"].as_array_mut().unwrap() {
            if entry["data_type"] == "fsl_cfg" {
                entry["suids"] = json!([changed_suid]);
            }
        }
        let mut rebound_snapshots = ns.clone();
        rebound_snapshots[0]["source"] = json!(changed_suid);
        let rebound =
            sleep_plan::prepare(&rebound_inventory, &d, &rebound_snapshots, &a, NEW_BOOT).unwrap();
        assert!(baseline_matches(journal.clone(), rebound, NEW_BOOT).unwrap());
        ns[0]["payload_hex"] = json!("0a0808001001180120011208080010011800200a");
        let changed = sleep_plan::prepare(&ni, &d, &ns, &a, NEW_BOOT).unwrap();
        assert!(!baseline_matches(journal.clone(), changed, NEW_BOOT).unwrap());
        let mut bad = plan;
        bad["baseline_snapshots"][0]["boot_id"] = json!(old_boot);
        assert!(baseline_matches(journal, bad, NEW_BOOT).is_err());
    }
}
