//! Load-driven hotplug demand, separate from leases and suspend eligibility.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU32, Ordering},
        LazyLock, Mutex,
    },
    time::Instant,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub sample_ms: u64,
    pub up_percent: u32,
    pub up_seconds: f64,
    pub sustained_fraction: f64,
    pub down_percent: u32,
    pub down_seconds: f64,
    pub dwell_seconds: f64,
    pub max_cores: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_ms: 500,
            up_percent: 85,
            up_seconds: 2.,
            sustained_fraction: 0.75,
            down_percent: 60,
            down_seconds: 10.,
            dwell_seconds: 2.,
            max_cores: 4,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(100..=5000).contains(&self.sample_ms)
            || !(1..=100).contains(&self.up_percent)
            || self.down_percent == 0
            || self.down_percent >= self.up_percent
            || !(1..=4).contains(&self.max_cores)
            || !self.up_seconds.is_finite()
            || !(1.0..=60.).contains(&self.up_seconds)
            || !self.down_seconds.is_finite()
            || !(2.0..=300.).contains(&self.down_seconds)
            || self.down_seconds < self.up_seconds
            || !self.dwell_seconds.is_finite()
            || !(0.5..=300.).contains(&self.dwell_seconds)
            || !self.sustained_fraction.is_finite()
            || !(0.5..=1.).contains(&self.sustained_fraction)
            || self.up_seconds * 1000. < self.sample_ms as f64 * 2.
        {
            return Err("invalid automatic core configuration".into());
        }
        Ok(())
    }
}
/// Atomic partial updates preserve concurrent settings and reject misspelled keys.
pub fn patched(config: &Config, patch: &Value) -> Result<Config, String> {
    let patch = patch
        .as_object()
        .ok_or("automatic core patch must be an object")?;
    let mut value = serde_json::to_value(config).map_err(|e| e.to_string())?;
    for (key, v) in patch {
        value[key] = v.clone();
    }
    let config: Config = serde_json::from_value(value).map_err(|e| e.to_string())?;
    config.validate()?;
    Ok(config)
}
#[derive(Clone, Debug)]
struct Sample {
    counters: [u64; 8],
    busy: u64,
    total: u64,
    runnable: u32,
    mask: u8,
    boot: f64,
    awake: f64,
}
fn parse(text: &str, mask: u8, boot: f64, awake: f64) -> Result<Sample, String> {
    let mut counters = [0u64; 8];
    let mut busy = 0u64;
    let mut total = 0u64;
    let mut seen = 0u8;
    let mut runnable = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { continue };
        if name == "procs_running" {
            runnable = words
                .next()
                .and_then(|v| v.parse::<u32>().ok())
                .map(|v| v.saturating_sub(1));
            continue;
        }
        let Some(id) = name
            .strip_prefix("cpu")
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|n| *n < 4)
        else {
            continue;
        };
        if mask & (1 << id) == 0 {
            continue;
        }
        let fields = words
            .take(8)
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if fields.len() != 8 {
            return Err("incomplete CPU counters".into());
        }
        seen |= 1 << id;
        for (i, v) in fields.into_iter().enumerate() {
            counters[i] = counters[i].checked_add(v).ok_or("CPU counter overflow")?;
            total = total.checked_add(v).ok_or("CPU counter overflow")?;
            if !matches!(i, 3 | 4 | 7) {
                busy = busy.checked_add(v).ok_or("CPU counter overflow")?;
            }
        }
    }
    if mask & 1 == 0 || mask & !15 != 0 || seen != mask {
        return Err("online CPU counters unavailable".into());
    }
    Ok(Sample {
        counters,
        busy,
        total,
        runnable: runnable.ok_or("runnable count unavailable")?,
        mask,
        boot,
        awake,
    })
}
#[derive(Default)]
struct Engine {
    previous: Option<Sample>,
    window: VecDeque<(f64, f64, bool, bool)>,
    target: u32,
    last_change: f64,
}
impl Engine {
    fn reset(&mut self) {
        *self = Self::default();
        self.target = 1;
    }
    fn fraction(&self, now: f64, seconds: f64, up: bool) -> Option<f64> {
        let start = now - seconds;
        let mut covered = 0.;
        let mut matched = 0.;
        for &(end, dt, high, low) in &self.window {
            let overlap = (end - (end - dt).max(start)).max(0.);
            covered += overlap;
            if if up { high } else { low } {
                matched += overlap;
            }
        }
        (covered + 1e-6 >= seconds).then_some(matched / seconds)
    }
    fn observe(&mut self, c: &Config, s: Sample) -> (u32, String) {
        self.target = self.target.max(1).min(c.max_cores);
        let Some(old) = self.previous.replace(s.clone()) else {
            return (self.target, "warming up".into());
        };
        let dt = s.awake - old.awake;
        let elapsed = s.boot - old.boot;
        if s.mask != old.mask
            || dt <= 0.
            || dt > c.sample_ms as f64 / 1000. * 2.5
            || (elapsed - dt).abs() > 0.05
        {
            self.window.clear();
            self.last_change = s.awake;
            if dt <= 0. || (elapsed - dt).abs() > 0.05 || dt > c.sample_ms as f64 / 1000. * 2.5 {
                self.target = 1;
            }
            return (self.target, "observation reset".into());
        }
        if s.counters
            .iter()
            .zip(old.counters)
            .any(|(new, old)| *new < old)
        {
            self.reset();
            return (1, "CPU counter reset".into());
        }
        let delta = s.total.checked_sub(old.total).filter(|n| *n > 0);
        let busy = s.busy.checked_sub(old.busy);
        let Some((total, busy)) = delta.zip(busy).filter(|(t, b)| b <= t) else {
            self.reset();
            return (1, "CPU counter reset".into());
        };
        let online = s.mask.count_ones();
        let utilization = 100. * busy as f64 / total as f64;
        let high = utilization >= c.up_percent as f64 && s.runnable > online;
        let low = online > 1
            && utilization * online as f64 <= (online - 1) as f64 * c.down_percent as f64
            && s.runnable < online;
        self.window.push_back((s.awake, dt, high, low));
        while self
            .window
            .front()
            .is_some_and(|(end, _, _, _)| *end < s.awake - c.down_seconds.max(c.up_seconds))
        {
            self.window.pop_front();
        }
        let mut reason = "observing";
        if s.awake - self.last_change >= c.dwell_seconds {
            if online < c.max_cores
                && self
                    .fraction(s.awake, c.up_seconds, true)
                    .is_some_and(|n| n >= c.sustained_fraction)
            {
                self.target = online + 1;
                reason = "sustained CPU contention";
            } else if self.target > 1
                && self
                    .fraction(s.awake, c.down_seconds, false)
                    .is_some_and(|n| n >= c.sustained_fraction)
            {
                self.target -= 1;
                reason = "demand fits fewer cores";
            }
            if reason != "observing" {
                self.window.clear();
                self.last_change = s.awake;
            }
        }
        (
            self.target,
            format!(
                "{reason}; utilization={utilization:.1}% runnable={} online={online}",
                s.runnable
            ),
        )
    }
}
struct Runtime {
    config: Config,
    engine: Engine,
    status: Value,
    start: Instant,
}
static DEMAND: AtomicU32 = AtomicU32::new(1);
static RUNTIME: LazyLock<Mutex<Runtime>> = LazyLock::new(|| {
    Mutex::new(Runtime {
        config: Config::default(),
        engine: Engine::default(),
        status: json!({"reason":"disabled","target":1}),
        start: Instant::now(),
    })
});
pub fn configure(config: &Config) {
    let mut r = RUNTIME.lock().unwrap();
    if r.config != *config {
        r.config = config.clone();
        r.engine.reset();
        DEMAND.store(1, Ordering::Relaxed);
        r.status = json!({"reason":if config.enabled {"warming up"}else{"disabled"},"target":1});
    }
}
pub fn sleep_completed() {
    let mut r = RUNTIME.lock().unwrap();
    if r.config.enabled {
        r.engine.reset();
        DEMAND.store(1, Ordering::Relaxed);
        r.status =
            json!({"target":1,"reason":"fresh observations required after sleep transaction"});
    }
}
pub fn failed(error: &str) {
    let mut r = RUNTIME.lock().unwrap();
    r.engine.reset();
    DEMAND.store(1, Ordering::Relaxed);
    r.status = json!({"target":1,"reason":format!("core reconciliation failed: {error}")});
}
pub fn demand() -> u32 {
    DEMAND.load(Ordering::Relaxed)
}
pub fn status() -> Value {
    RUNTIME.lock().unwrap().status.clone()
}
pub fn period() -> std::time::Duration {
    let r = RUNTIME.lock().unwrap();
    std::time::Duration::from_millis(if r.config.enabled {
        r.config.sample_ms
    } else {
        5000
    })
}
pub fn tick() {
    let mut r = RUNTIME.lock().unwrap();
    if !r.config.enabled {
        return;
    }
    if crate::on_charger() {
        r.engine.reset();
        DEMAND.store(1, Ordering::Relaxed);
        r.status = json!({"target":1,"reason":"charging policy owns core count"});
        return;
    }
    let config = r.config.clone();
    let sample = (|| {
        let mut mask = 1;
        for (i, path) in crate::CORE_PATHS.iter().enumerate() {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())?
                .trim()
            {
                "1" => mask |= 1 << (i + 1),
                "0" => {}
                _ => return Err("invalid online CPU state".into()),
            }
        }
        parse(
            &std::fs::read_to_string("/proc/stat").map_err(|e| e.to_string())?,
            mask,
            crate::boottime().map_err(|e| e.to_string())?.as_secs_f64(),
            r.start.elapsed().as_secs_f64(),
        )
    })();
    let (target, reason) = match sample {
        Ok(s) => r.engine.observe(&config, s),
        Err(e) => {
            r.engine.reset();
            (1, e)
        }
    };
    let old = DEMAND.swap(target, Ordering::Relaxed);
    if old != target {
        eprintln!("auto-cores: {old}>{target}: {reason}");
    }
    r.status = json!({"target":target,"reason":reason});
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(t: f64, mask: u8, load: f64, runnable: u32) -> Sample {
        Sample {
            counters: [
                (t * 1000. * load) as u64,
                0,
                0,
                (t * 1000.) as u64 - (t * 1000. * load) as u64,
                0,
                0,
                0,
                0,
            ],
            total: (t * 1000.) as u64,
            busy: (t * 1000. * load) as u64,
            mask,
            runnable,
            boot: t,
            awake: t,
        }
    }
    #[test]
    fn saturation_requires_other_runnable_work_and_a_full_window() {
        let c = Config {
            enabled: true,
            ..Config::default()
        };
        let mut e = Engine::default();
        for i in 0..10 {
            assert_eq!(e.observe(&c, sample(i as f64 / 2., 1, 0.95, 1)).0, 1);
        }
        for i in 10..13 {
            assert_eq!(
                e.observe(&c, sample(i as f64 / 2., 1, 0.95, 2)).0,
                if i == 12 { 2 } else { 1 }
            );
        }
    }
    #[test]
    fn burst_resume_bad_counters_and_topology_reset_evidence() {
        let c = Config::default();
        let mut e = Engine::default();
        e.observe(&c, sample(0., 1, 0.95, 3));
        e.observe(&c, sample(0.5, 1, 0.95, 3));
        assert_eq!(e.observe(&c, sample(1., 3, 0.95, 3)).0, 1);
        let mut s = sample(1.5, 3, 0.95, 3);
        s.boot += 30.;
        assert_eq!(e.observe(&c, s).0, 1);
        assert!(e.window.is_empty());
        e.previous = Some(sample(2., 1, 0.9, 3));
        assert_eq!(e.observe(&c, sample(0.5, 1, 0.9, 3)).0, 1);
    }
    #[test]
    fn low_demand_releases_one_core_after_long_window() {
        let c = Config::default();
        let mut e = Engine {
            target: 4,
            ..Engine::default()
        };
        for i in 0..20 {
            assert_eq!(e.observe(&c, sample(i as f64 / 2., 15, 0.1, 1)).0, 4);
        }
        assert_eq!(e.observe(&c, sample(10., 15, 0.1, 1)).0, 3);
        assert_eq!(e.observe(&c, sample(10.5, 7, 0.1, 1)).0, 3);
    }
    #[test]
    fn proc_counts_exclude_guest_double_count_iowait_and_sampler() {
        let s = parse(
            "cpu0 10 0 10 50 20 5 5 10 999 999\nprocs_running 3\n",
            1,
            0.,
            0.,
        )
        .unwrap();
        assert_eq!((s.busy, s.total, s.runnable), (30, 110, 2));
        assert!(parse("cpu0 1 2\nprocs_running 1\n", 1, 0., 0.).is_err());
    }
    #[test]
    fn ssh_patches_preserve_other_fields_and_reject_typos() {
        let c = Config::default();
        let updated = patched(&c, &json!({"enabled":true,"up_percent":90})).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.up_percent, 90);
        assert_eq!(updated.down_seconds, c.down_seconds);
        assert!(patched(&c, &json!({"up_precent":90})).is_err());
        assert!(patched(&c, &json!({"up_seconds":0})).is_err());
        assert!(patched(&c, &json!(true)).is_err());
        assert!(!c.enabled);
        let mut legacy = serde_json::to_value(crate::sleep_policy::Config::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("auto_cores");
        let decoded: crate::sleep_policy::Config = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.auto_cores, c);
    }
    #[test]
    fn sustained_pressure_adds_one_at_a_time_to_the_configured_limit() {
        let c = Config {
            max_cores: 3,
            ..Config::default()
        };
        let mut e = Engine::default();
        let mut online = 1u32;
        for i in 0..40 {
            let target = e
                .observe(
                    &c,
                    sample(i as f64 / 2., ((1u32 << online) - 1) as u8, 0.95, 8),
                )
                .0;
            assert!(target <= online + 1);
            assert!(target <= 3);
            online = target;
        }
        assert_eq!(online, 3);
    }
    #[test]
    fn runtime_config_bounds_and_legacy_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(!c.enabled);
        assert!(c.validate().is_ok());
        for c in [
            Config {
                sample_ms: 5000,
                ..c.clone()
            },
            Config {
                up_seconds: f64::NAN,
                ..c.clone()
            },
            Config {
                down_percent: 90,
                ..c.clone()
            },
            Config { max_cores: 5, ..c },
        ] {
            assert!(c.validate().is_err());
        }
    }
}
