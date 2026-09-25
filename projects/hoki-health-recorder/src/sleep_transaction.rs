//! Configuration transaction engine. The adapter owns the exclusive profile lease,
//! independent restoration supervisor, bounded helper execution and fsync policy.
use crate::{sleep_plan, valid_session_id, Result};
use serde_json::{json, Value};

/// Successful persist must mean file AND containing directory are durable.
/// Successful read must mean a fresh, clean, same-boot source-qualified getter.
/// Set is transport-only; this engine always reads back independently.
pub trait Backend {
    fn finish_restoration(&mut self) -> Result<()> {
        Ok(())
    }
    fn persist(&mut self, journal: &Value) -> Result<()>;
    fn read(&mut self, baseline: &Value) -> Result<String>;
    fn set(&mut self, operation: &Value, restore: bool) -> Result<()>;
}

pub struct Transaction {
    journal: Value,
}
fn array3(value: &Value) -> Result<[Value; 3]> {
    value
        .as_array()
        .ok_or("missing snapshot array")?
        .clone()
        .try_into()
        .map_err(|_| "expected three snapshots".into())
}
fn validate_plan(plan: &Value, boot: &str) -> Result<()> {
    let regenerated = sleep_plan::prepare(
        &plan["discovery_inventory"],
        &plan["discovery_archive_status"],
        &array3(&plan["baseline_snapshots"])?,
        &array3(&plan["baseline_archive_statuses"])?,
        boot,
    )?;
    if regenerated != *plan {
        return Err("sleep plan differs from validated baseline".into());
    }
    Ok(())
}
impl Transaction {
    pub fn new(plan: Value, boot: &str, owner: &str) -> Result<Self> {
        validate_plan(&plan, boot)?;
        if !valid_session_id(owner) {
            return Err("invalid profile owner".into());
        }
        let count = plan["operations"].as_array().unwrap().len();
        Ok(Self {
            journal: json!({"version":1,"owner":owner,"boot_id":boot,
            "phase":"prepared","plan":plan,"states":vec!["untouched";count]}),
        })
    }
    pub fn load(journal: Value, boot: &str, owner: &str) -> Result<Self> {
        if journal["version"] != 1
            || journal["boot_id"] != boot
            || !valid_session_id(owner)
            || journal["owner"] != owner
        {
            return Err("sleep recovery identity mismatch".into());
        }
        validate_plan(&journal["plan"], boot)?;
        let phase = journal["phase"]
            .as_str()
            .ok_or("missing transaction phase")?;
        if !["prepared", "enabling", "active", "restoring", "restored"].contains(&phase) {
            return Err("invalid transaction phase".into());
        }
        let states = journal["states"]
            .as_array()
            .ok_or("missing operation states")?;
        if states.len() != journal["plan"]["operations"].as_array().unwrap().len() {
            return Err("operation count mismatch".into());
        }
        let mut untouched = false;
        for state in states {
            let s = state.as_str().ok_or("invalid operation state")?;
            if ![
                "untouched",
                "enable_intent",
                "applied",
                "restore_intent",
                "restored",
            ]
            .contains(&s)
            {
                return Err("unknown operation state".into());
            }
            if untouched && s != "untouched" {
                return Err("non-prefix configuration ownership".into());
            }
            untouched |= s == "untouched";
            if (phase == "prepared" && s != "untouched")
                || (phase == "active" && s != "applied")
                || (phase == "restored" && !["untouched", "restored"].contains(&s))
                || (phase == "enabling" && !["untouched", "enable_intent", "applied"].contains(&s))
            {
                return Err("inconsistent transaction phase/state".into());
            }
        }
        Ok(Self { journal })
    }
    pub fn journal(&self) -> &Value {
        &self.journal
    }
    fn checkpoint(
        &mut self,
        backend: &mut impl Backend,
        phase: &str,
        state: Option<(usize, &str)>,
    ) -> Result<()> {
        // Keep the conservative new state even on persistence error: rename may
        // already have happened. No subsequent mutation follows a failed save.
        self.journal["phase"] = json!(phase);
        if let Some((index, s)) = state {
            self.journal["states"][index] = json!(s);
        }
        backend.persist(&self.journal)
    }
    fn baseline(&self, op: &Value) -> Value {
        self.journal["plan"]["baseline_snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| {
                s["session_id"] == op["snapshot_session_id"]
                    && s["event_id"] == op["readback_id"]
                    && s["source"] == op["source"]
            })
            .expect("validated plan operation")
            .clone()
    }
    /// Only a new prepared transaction may enable. Interrupted transactions must
    /// restore; replaying enable would obscure uncertain application/ownership.
    pub fn enable(&mut self, backend: &mut impl Backend) -> Result<()> {
        if self.journal["phase"] != "prepared" {
            return Err("transaction cannot resume enable".into());
        }
        backend.persist(&self.journal)?;
        let baselines = self.journal["plan"]["baseline_snapshots"]
            .as_array()
            .unwrap()
            .clone();
        // Include already-enabled settings: they are prerequisites, not owned writes.
        for baseline in &baselines {
            if backend.read(baseline)? != baseline["payload_hex"].as_str().unwrap() {
                return Err("configuration baseline changed before activation".into());
            }
        }
        let operations = self.journal["plan"]["operations"]
            .as_array()
            .unwrap()
            .clone();
        for (i, op) in operations.iter().enumerate() {
            let baseline = self.baseline(op);
            if backend.read(&baseline)? != op["baseline_reply_hex"].as_str().unwrap() {
                return Err("configuration changed before owned write".into());
            }
            self.checkpoint(backend, "enabling", Some((i, "enable_intent")))?;
            backend.set(op, false)?;
            if backend.read(&baseline)? != op["enabled_reply_hex"].as_str().unwrap() {
                return Err("configuration enable readback mismatch".into());
            }
            self.checkpoint(backend, "enabling", Some((i, "applied")))?;
        }
        self.checkpoint(backend, "active", None)
    }
    /// Restore in reverse order, including uncertain sends. Never write settings
    /// for untouched operations. Conflicting replies stop recovery for inspection.
    pub fn restore(&mut self, backend: &mut impl Backend) -> Result<()> {
        if self.journal["phase"] == "restored" {
            return backend.finish_restoration();
        }
        self.checkpoint(backend, "restoring", None)?;
        let operations = self.journal["plan"]["operations"]
            .as_array()
            .unwrap()
            .clone();
        for (i, op) in operations.iter().enumerate().rev() {
            let state = self.journal["states"][i].as_str().unwrap();
            if state == "untouched" || state == "restored" {
                continue;
            }
            let baseline = self.baseline(op);
            let current = backend.read(&baseline)?;
            if current == op["baseline_reply_hex"].as_str().unwrap() {
                self.checkpoint(backend, "restoring", Some((i, "restored")))?;
                continue;
            }
            if current != op["enabled_reply_hex"].as_str().unwrap() {
                return Err("configuration conflict: refusing restoration write".into());
            }
            self.checkpoint(backend, "restoring", Some((i, "restore_intent")))?;
            backend.set(op, true)?;
            if backend.read(&baseline)? != op["baseline_reply_hex"].as_str().unwrap() {
                return Err("configuration restoration readback mismatch".into());
            }
            self.checkpoint(backend, "restoring", Some((i, "restored")))?;
        }
        self.checkpoint(backend, "restored", None)?;
        backend.finish_restoration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BOOT: &str = "12345678-1234-1234-1234-123456789abc";
    struct Mock {
        current: [String; 3],
        saved: Option<Value>,
        calls: usize,
        fail_at: Option<usize>,
        fail_after: bool,
        writes: Vec<(String, bool)>,
    }
    impl Mock {
        fn index(event: &Value) -> usize {
            match event.as_u64().unwrap() {
                769 => 0,
                776 => 1,
                876 => 2,
                _ => panic!(),
            }
        }
        fn fault(&mut self) -> bool {
            self.calls += 1;
            self.fail_at == Some(self.calls)
        }
    }
    impl Backend for Mock {
        fn finish_restoration(&mut self) -> Result<()> {
            assert_eq!(self.saved.as_ref().unwrap()["phase"], "restored");
            if self.fault() {
                Err("injected owner release failure".into())
            } else {
                Ok(())
            }
        }
        fn persist(&mut self, journal: &Value) -> Result<()> {
            let fail = self.fault();
            if !fail || self.fail_after {
                self.saved = Some(journal.clone());
            }
            if fail {
                Err("injected persistence failure".into())
            } else {
                Ok(())
            }
        }
        fn read(&mut self, baseline: &Value) -> Result<String> {
            if self.fault() {
                return Err("injected read failure".into());
            }
            Ok(self.current[Self::index(&baseline["event_id"])].clone())
        }
        fn set(&mut self, op: &Value, restore: bool) -> Result<()> {
            let i = Self::index(&op["readback_id"]);
            let saved = self
                .saved
                .as_ref()
                .expect("write must follow persisted intent");
            let op_index = saved["plan"]["operations"]
                .as_array()
                .unwrap()
                .iter()
                .position(|o| o == op)
                .unwrap();
            assert_eq!(
                saved["states"][op_index],
                if restore {
                    "restore_intent"
                } else {
                    "enable_intent"
                }
            );
            let fail = self.fault();
            if !fail || self.fail_after {
                self.current[i] = op[if restore {
                    "baseline_reply_hex"
                } else {
                    "enabled_reply_hex"
                }]
                .as_str()
                .unwrap()
                .into();
                self.writes
                    .push((op["kind"].as_str().unwrap().into(), restore));
            }
            if fail {
                Err("injected uncertain send".into())
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> (Transaction, Mock) {
        let (i, d, s, a) = sleep_plan::tests::fixture();
        let plan = sleep_plan::prepare(&i, &d, &s, &a, BOOT).unwrap();
        let current = s.map(|v| v["payload_hex"].as_str().unwrap().into());
        (
            Transaction::new(plan, BOOT, BOOT).unwrap(),
            Mock {
                current,
                saved: None,
                calls: 0,
                fail_at: None,
                fail_after: false,
                writes: vec![],
            },
        )
    }
    #[test]
    fn successful_transaction_and_reverse_restoration() {
        let (mut t, mut b) = fixture();
        let original = b.current.clone();
        t.enable(&mut b).unwrap();
        assert_eq!(t.journal()["phase"], "active");
        let mut t = Transaction::load(b.saved.clone().unwrap(), BOOT, BOOT).unwrap();
        assert!(t.enable(&mut b).is_err());
        t.restore(&mut b).unwrap();
        assert_eq!(b.current, original);
        assert_eq!(
            b.writes
                .iter()
                .filter(|v| v.1)
                .map(|v| v.0.as_str())
                .collect::<Vec<_>>(),
            vec!["detect", "tracking", "user"]
        );
        let calls = b.calls;
        t.restore(&mut b).unwrap();
        assert_eq!(b.calls, calls + 1);
    }
    #[test]
    fn failure_at_every_activation_boundary_recovers_uncertain_application() {
        let (mut t, mut b) = fixture();
        t.enable(&mut b).unwrap();
        let count = b.calls;
        for at in 1..=count {
            for after in [false, true] {
                let (mut t, mut b) = fixture();
                let original = b.current.clone();
                b.fail_at = Some(at);
                b.fail_after = after;
                assert!(t.enable(&mut b).is_err(), "boundary {at}");
                b.fail_at = None;
                if let Some(saved) = b.saved.clone() {
                    Transaction::load(saved, BOOT, BOOT)
                        .unwrap()
                        .restore(&mut b)
                        .unwrap();
                }
                assert_eq!(b.current, original, "boundary {at} after {after}");
            }
        }
    }
    #[test]
    fn failure_at_every_restore_boundary_can_retry_from_disk() {
        let (mut t, mut b) = fixture();
        t.enable(&mut b).unwrap();
        b.calls = 0;
        t.restore(&mut b).unwrap();
        let count = b.calls;
        for at in 1..=count {
            for after in [false, true] {
                let (mut t, mut b) = fixture();
                let original = b.current.clone();
                t.enable(&mut b).unwrap();
                b.calls = 0;
                b.fail_at = Some(at);
                b.fail_after = after;
                assert!(t.restore(&mut b).is_err(), "boundary {at}");
                b.fail_at = None;
                Transaction::load(b.saved.clone().unwrap(), BOOT, BOOT)
                    .unwrap()
                    .restore(&mut b)
                    .unwrap();
                assert_eq!(b.current, original, "boundary {at} after {after}");
            }
        }
    }
    #[test]
    fn borrowed_flags_are_checked_but_never_owned_or_restored() {
        let (i, d, mut s, a) = sleep_plan::tests::fixture();
        s[0]["payload_hex"] = json!("0a0808001001180120001208080010011800200a");
        s[1]["payload_hex"] =
            json!("080112060800103c18001a1508001211080015000000001d000000002500000000");
        let plan = sleep_plan::prepare(&i, &d, &s, &a, BOOT).unwrap();
        assert_eq!(plan["operations"].as_array().unwrap().len(), 2);
        assert_eq!(plan["operations"][0]["restore_payload_hex"], "1a022000");
        let mut t = Transaction::new(plan, BOOT, BOOT).unwrap();
        let (_, mut b) = fixture();
        b.current = s.map(|v| v["payload_hex"].as_str().unwrap().into());
        let original = b.current.clone();
        t.enable(&mut b).unwrap();
        t.restore(&mut b).unwrap();
        assert_eq!(b.current, original);
        assert!(b.writes.iter().all(|(kind, _)| kind != "tracking"));
    }
    #[test]
    fn stale_plan_conflict_and_wrong_identity_never_write() {
        let (mut t, mut b) = fixture();
        b.current[2].push_str("a00101");
        assert!(t.enable(&mut b).is_err());
        assert!(b.writes.is_empty());
        let (mut t, mut b) = fixture();
        t.enable(&mut b).unwrap();
        b.current[2].push_str("a00101");
        let writes = b.writes.len();
        assert!(t.restore(&mut b).is_err());
        assert_eq!(b.writes.len(), writes);
        let saved = b.saved.unwrap();
        assert!(
            Transaction::load(saved.clone(), "00000000-0000-0000-0000-000000000000", BOOT).is_err()
        );
        assert!(
            Transaction::load(saved.clone(), BOOT, "00000000-0000-0000-0000-000000000000").is_err()
        );
        let mut corrupt = saved;
        corrupt["plan"]["operations"][0]["restore_payload_hex"] = json!("0801");
        assert!(Transaction::load(corrupt, BOOT, BOOT).is_err());
    }
}
