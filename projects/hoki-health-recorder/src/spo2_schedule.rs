//! Bounded recorder-owned SpO2 demand; other clients retain their demands.
use crate::Result;
use serde_json::Value;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Policy { Continuous, Periodic, Off }
impl Policy {
    pub fn parse(value: Option<&str>, trial: bool) -> Result<Self> {
        let policy = match value {
            None if trial => Self::Continuous,
            None | Some("periodic") => Self::Periodic,
            Some("continuous") => Self::Continuous,
            Some("off") => Self::Off,
            _ => return Err("HOKI_SPO2_POLICY must be periodic, continuous, or off".into()),
        };
        if trial && policy != Self::Continuous {
            return Err("buffered isolation trials require continuous SpO2 policy".into());
        }
        Ok(policy)
    }
    pub fn name(self) -> &'static str {
        match self { Self::Continuous => "continuous", Self::Periodic => "periodic", Self::Off => "off" }
    }
}
pub const INTERVAL_SECONDS: f64 = 900.0;
pub const WINDOW_SECONDS: f64 = 180.0;

pub fn cooldown_remaining(saved: &Value, boot: &str, now: f64, unix: f64) -> Result<f64> {
    let previous_boot = saved["boot_id"].as_str().ok_or("missing cooldown boot")?;
    if !matches!(saved["version"].as_u64(), Some(1 | 2)) || !crate::valid_session_id(previous_boot) {
        return Err("invalid SpO2 cooldown identity".into());
    }
    let legacy = saved["version"] == 1;
    let previous = if previous_boot == boot {
        saved[if legacy {"success_boottime_seconds"} else {"attempt_boottime_seconds"}].as_f64()
    } else { saved[if legacy {"success_unix_seconds"} else {"attempt_unix_seconds"}].as_f64() }.ok_or("invalid cooldown time")?;
    if !previous.is_finite() || previous < 0.0 { return Err("invalid cooldown time".into()); }
    let current = if previous_boot == boot { now } else { unix };
    // A clock rollback conservatively starts a fresh fifteen-minute wait.
    Ok((INTERVAL_SECONDS - (current-previous)).clamp(0.0,INTERVAL_SECONDS))
}

pub fn persist_attempt(path: &Path, boot: &str, now: f64) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temp=path.with_extension("tmp");
    let mut f=fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&temp)?;
    writeln!(f,"{}",serde_json::json!({"version":2,"boot_id":boot,
        "attempt_boottime_seconds":now,
        "attempt_unix_seconds":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64()}))?;
    f.sync_all()?; fs::rename(temp,path)?;
    Ok(())
}

pub struct Schedule { pub active: bool, pub deadline: f64, pub started: f64 }
impl Schedule {
    pub fn started(now: f64) -> Self {
        Self { active: true, deadline: now + WINDOW_SECONDS, started: now }
    }
    pub fn due(&self, now: f64) -> bool { now >= self.deadline }
    pub fn released(&mut self, now: f64, _success: bool) {
        self.active = false;
        // Skip missed slots instead of immediately retrying after a delayed release.
        let elapsed = (now - self.started).max(0.0);
        self.deadline = self.started + (elapsed / INTERVAL_SECONDS).floor().mul_add(INTERVAL_SECONDS, INTERVAL_SECONDS);
    }
    pub fn activated(&mut self, now: f64) { *self = Self::started(now); }
}

/// Stock consumer acceptance plus finite/range and freshness checks. Confidence
/// is an algorithm field, not a calibrated probability or clinical validation.
fn accepted(record: &[u8; 88], handle: u32, start: f64, end: f64) -> bool {
    let arrival = i64::from_le_bytes(record[0..8].try_into().unwrap());
    let source = i64::from_le_bytes(record[8..16].try_into().unwrap());
    let h = u32::from_le_bytes(record[16..20].try_into().unwrap());
    let typ = u32::from_le_bytes(record[20..24].try_into().unwrap());
    let values: Vec<f32> = record[24..40].chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    h == handle && typ == 65561 && source >= (start * 1e9) as i64
        && source <= (end * 1e9) as i64 && arrival >= source
        && values.iter().all(|v| v.is_finite())
        && values[0] >= 81.0 && values[0] <= 100.0
        && values[1] >= 80.0 && values[1] <= 100.0
        && values[2] == 5.0 && values[3] == 0.0
}

/// Incremental scan of durable prefixes only. Idle checkpoints advance the
/// cursor without reading payload; memory remains bounded across segment rolls.
#[derive(Default)]
pub struct Results { segment: u64, offset: u64 }
impl Results {
    pub fn scan(&mut self, root: &Path, attempt: Option<(u32, f64, f64)>) -> Result<bool> {
        let cp: Value = serde_json::from_slice(&fs::read(root.join("checkpoint.json"))?)?;
        let segment = cp["segment"].as_u64().ok_or("invalid checkpoint segment")?;
        let bytes = cp["segment_bytes"].as_u64().ok_or("invalid checkpoint length")?;
        if cp["version"] != 1 || cp["complete"] != true || segment > 999999
            || bytes < 16 || (bytes-16)%88 != 0 || segment < self.segment
            || (segment == self.segment && bytes < self.offset) {
            return Err("invalid SpO2 durable cursor".into());
        }
        let mut success = false;
        if let Some((handle, start, end)) = attempt {
            for i in self.segment..=segment {
                let mut f = BufReader::new(crate::recording_io::segment(root,&cp,i)?);
                let size = f.get_ref().get_ref().len() as u64;
                let limit = if i == segment { bytes } else { size };
                if limit > size || limit < 16 || (limit-16)%88 != 0 {
                    return Err("invalid durable segment extent".into());
                }
                let mut header = [0;16]; f.read_exact(&mut header)?;
                if &header != b"HOKISEN1\x58\0\0\0\x01\0\0\0" {
                    return Err("invalid HAL record header".into());
                }
                let mut offset = if i == self.segment { self.offset.max(16) } else { 16 };
                f.seek(SeekFrom::Start(offset))?;
                let mut record = [0;88];
                while offset < limit {
                    f.read_exact(&mut record)?;
                    success |= accepted(&record, handle, start, end);
                    offset += 88;
                }
            }
        }
        self.segment = segment; self.offset = bytes;
        Ok(success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooldown_survives_restart_and_handles_clock_rollback() {
        let boot="12345678-1234-1234-1234-123456789abc";
        let saved=serde_json::json!({"version":1,"boot_id":boot,
            "success_boottime_seconds":100.,"success_unix_seconds":10000.});
        assert_eq!(cooldown_remaining(&saved,boot,200.,20000.).unwrap(),800.);
        assert_eq!(cooldown_remaining(&saved,boot,1000.,20000.).unwrap(),0.);
        assert_eq!(cooldown_remaining(&saved,"other",0.,10200.).unwrap(),700.);
        assert_eq!(cooldown_remaining(&saved,"other",0.,9000.).unwrap(),900.);
        let attempt=serde_json::json!({"version":2,"boot_id":boot,
            "attempt_boottime_seconds":100.,"attempt_unix_seconds":10000.});
        assert_eq!(cooldown_remaining(&attempt,boot,280.,0.).unwrap(),720.);
        assert_eq!(cooldown_remaining(&attempt,"other",0.,10200.).unwrap(),700.);
        assert!(cooldown_remaining(&serde_json::json!({}),boot,0.,0.).is_err());
    }
    fn record(value: f32, confidence: f32, state: f32, signal: f32) -> [u8;88] {
        let mut r=[0;88];
        r[0..8].copy_from_slice(&2_100_000_000i64.to_le_bytes());
        r[8..16].copy_from_slice(&2_000_000_000i64.to_le_bytes());
        r[16..20].copy_from_slice(&8u32.to_le_bytes());
        r[20..24].copy_from_slice(&65561u32.to_le_bytes());
        for (i,v) in [value,confidence,state,signal].iter().enumerate() {
            r[24+i*4..28+i*4].copy_from_slice(&v.to_le_bytes());
        }
        r
    }
    #[test]
    fn rejects_progress_movement_low_confidence_and_stale_values() {
        let good=record(97.0,90.0,5.0,0.0);
        assert!(accepted(&good,8,1.0,3.0));
        for r in [record(97.,90.,4.,0.),record(97.,90.,5.,8.),record(97.,79.,5.,0.),
                  record(0.,90.,5.,0.),record(f32::NAN,90.,5.,0.),record(101.,90.,5.,0.)] {
            assert!(!accepted(&r,8,1.,3.));
        }
        assert!(!accepted(&good,8,2.1,3.));
        assert!(!accepted(&good,9,1.,3.));
        assert!(!accepted(&good,8,1.,1.9));
    }
    #[test]
    fn attempt_state_survives_interruption_before_a_result() {
        let root=std::env::temp_dir().join(format!("hoki-spo2-state-{}",std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path=root.join("spo2-cooldown.json");
        let boot="12345678-1234-1234-1234-123456789abc";
        persist_attempt(&path,boot,100.).unwrap();
        let saved:Value=serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["version"],2);
        assert_eq!(cooldown_remaining(&saved,boot,160.,0.).unwrap(),840.);
        assert_eq!(cooldown_remaining(&saved,boot,1000.,0.).unwrap(),0.);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fixed_start_cadence_is_independent_of_outcome() {
        for success in [false, true] {
            let mut s=Schedule::started(100.);
            assert!(!s.due(279.)); assert!(s.due(280.));
            s.released(if success {160.} else {280.},success);
            assert!(!s.active); assert_eq!(s.deadline,1000.);
            s.activated(1000.); s.released(4000.,success);
            assert_eq!(s.deadline,4600.);
        }
    }
    #[test]
    fn trials_require_explicit_continuous_behavior() {
        assert_eq!(Policy::parse(None,true).unwrap(),Policy::Continuous);
        assert!(Policy::parse(Some("periodic"),true).is_err());
        assert!(Policy::parse(Some("off"),true).is_err());
        assert!(Policy::parse(Some("typo"),false).is_err());
        assert_eq!(Policy::parse(None,false).unwrap(),Policy::Periodic);
    }
    #[test]
    fn durable_cursor_skips_uncommitted_tail_and_handles_rollover() {
        let root=std::env::temp_dir().join(format!("hoki-spo2-{}",std::process::id()));
        fs::create_dir(&root).unwrap();
        let header=b"HOKISEN1\x58\0\0\0\x01\0\0\0";
        let mut data=header.to_vec();data.extend_from_slice(&record(97.,90.,5.,8.));
        data.extend_from_slice(&record(97.,90.,5.,0.));
        fs::write(root.join("events-000000.bin"),&data).unwrap();
        let checkpoint=|segment,bytes|fs::write(root.join("checkpoint.json"),
            serde_json::json!({"version":1,"complete":true,"segment":segment,"segment_bytes":bytes}).to_string()).unwrap();
        checkpoint(0,104);
        let mut reader=Results::default();
        assert!(!reader.scan(&root,Some((8,1.,3.))).unwrap());
        checkpoint(0,192);
        assert!(reader.scan(&root,Some((8,1.,3.))).unwrap());
        assert!(!reader.scan(&root,Some((8,1.,3.))).unwrap());
        fs::write(root.join("events-000001.bin"),&data[..104]).unwrap();checkpoint(1,104);
        assert!(!reader.scan(&root,Some((8,1.,3.))).unwrap());
        checkpoint(0,192);assert!(reader.scan(&root,None).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
