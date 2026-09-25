use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use zbus::{connection, interface};

const MAX_CORES: u32 = 4;
const CORE_PATHS: [&str; 3] = [
    "/sys/devices/system/cpu/cpu1/online",
    "/sys/devices/system/cpu/cpu2/online",
    "/sys/devices/system/cpu/cpu3/online",
];

fn periodic_interval(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    // These ticks observe current state; missed observations cannot be recovered
    // by repeatedly reading the same present sysfs state after a delay.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}

struct Lease {
    cores: u32,
    /// CLOCK_BOOTTIME deadline: sleeping time counts toward the lease.
    expires: Duration,
    owner: String,
}

impl Lease {
    fn active_at(&self, now: Duration) -> bool {
        self.expires > now
    }

    fn remaining_at(&self, now: Duration) -> Duration {
        self.expires.saturating_sub(now)
    }
}

fn boottime() -> std::io::Result<Duration> {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Unlike CLOCK_MONOTONIC, this advances during system suspend and is not
    // affected by wall-clock corrections. Reading it does not arm a wake alarm.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut stamp) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if stamp.tv_sec < 0 || !(0..1_000_000_000).contains(&stamp.tv_nsec) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid boot clock",
        ));
    }
    Ok(Duration::new(stamp.tv_sec as u64, stamp.tv_nsec as u32))
}

#[derive(Clone)]
struct PowerState {
    leases: Arc<Mutex<HashMap<String, Lease>>>,
    /// Tracked state for change detection.
    prev: Arc<Mutex<TrackedState>>,
}

#[derive(Default)]
struct TrackedState {
    charger: Option<bool>,
    cores: Option<u32>,
    wifi_up: Option<bool>,
}

impl PowerState {
    fn new() -> Self {
        Self {
            leases: Arc::new(Mutex::new(HashMap::new())),
            prev: Arc::new(Mutex::new(TrackedState::default())),
        }
    }

    /// Determine how many cores should be online right now.
    async fn desired_cores(&self) -> std::io::Result<u32> {
        let baseline = if on_charger() { MAX_CORES } else { 1 };
        let mut leases = self.leases.lock().await;
        let now = boottime()?;
        leases.retain(|_, lease| lease.active_at(now));
        let requested: u32 = leases
            .values()
            .map(|l| l.cores)
            .fold(0u32, u32::saturating_add);
        // Leases request additional cores beyond the always-online cpu0.
        Ok(granted_cores(baseline == MAX_CORES, requested))
    }

    /// Remove expired leases and apply the new core count.
    async fn reconcile(&self) -> std::io::Result<()> {
        let desired = self.desired_cores().await?;
        set_cores_online(desired)?;

        // Check for state changes and log immediately
        self.check_state_changes().await;
        Ok(())
    }

    /// Compare current system state against previous, log on any change.
    async fn check_state_changes(&self) {
        let charger = charger_state(std::path::Path::new("/sys/class/power_supply"));
        let cores = cores_online();
        let wifi_up = wifi_up();

        let mut prev = self.prev.lock().await;

        if let Some(charger) = charger {
            if prev.charger != Some(charger) {
                let label = if charger {
                    "charger-connected"
                } else {
                    "charger-disconnected"
                };
                log_battery_event(label);
                prev.charger = Some(charger);
            }
        }
        if prev.cores != Some(cores) {
            if let Some(old) = prev.cores {
                log_battery_event(&format!("cores:{}>{}", old, cores));
            }
            prev.cores = Some(cores);
        }
        if let Some(wifi_up) = wifi_up {
            if prev.wifi_up != Some(wifi_up) {
                let label = if wifi_up { "wifi-up" } else { "wifi-down" };
                log_battery_event(label);
                prev.wifi_up = Some(wifi_up);
            }
        }
    }
}

struct PowerManager {
    state: PowerState,
}

#[interface(name = "org.hoki.power.Manager")]
impl PowerManager {
    /// Request `cores` extra cores for `duration_secs` seconds.
    /// Returns a lease ID (use with release_cores for early return).
    /// Max duration: 300s (5 min). Max cores per lease: 3 (cpu1-cpu3).
    /// Time spent suspended counts toward expiry; leases do not wake the watch.
    async fn request_cores(&self, cores: u32, duration_secs: u32, owner: String) -> String {
        if cores == 0 || cores > 3 {
            return "error: cores must be 1-3".to_string();
        }
        let duration_secs = duration_secs.min(300);
        if duration_secs == 0 {
            return "error: duration must be > 0".to_string();
        }

        let now = match boottime() {
            Ok(now) => now,
            Err(error) => return format!("error: lease clock: {error}"),
        };
        let id = uuid::Uuid::new_v4().to_string();
        let lease = Lease {
            cores,
            expires: now + Duration::from_secs(duration_secs as u64),
            owner,
        };

        {
            let mut leases = self.state.leases.lock().await;
            leases.insert(id.clone(), lease);
        }

        if let Err(e) = self.state.reconcile().await {
            self.state.leases.lock().await.remove(&id);
            if let Err(rollback) = self.state.reconcile().await {
                eprintln!("core rollback failed: {rollback}");
            }
            return format!("error: {e}");
        }
        id
    }

    /// Release a lease early, returning its cores to the pool.
    async fn release_cores(&self, lease_id: String) -> String {
        {
            let mut leases = self.state.leases.lock().await;
            if leases.remove(&lease_id).is_none() {
                return "error: unknown lease".to_string();
            }
        }
        match self.state.reconcile().await {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Returns (cores_online, total_leased_cores, on_charger, lease_descriptions).
    /// Each lease description is "owner:cores:remaining_secs".
    async fn status(&self) -> zbus::fdo::Result<(u32, u32, bool, Vec<String>)> {
        let leases = self.state.leases.lock().await;
        let now = boottime()
            .map_err(|error| zbus::fdo::Error::Failed(format!("lease clock: {error}")))?;
        let active: Vec<&Lease> = leases.values().filter(|l| l.active_at(now)).collect();
        let leased: u32 = active
            .iter()
            .map(|l| l.cores)
            .fold(0u32, u32::saturating_add);
        let online = cores_online();
        let descriptions: Vec<String> = active
            .iter()
            .map(|l| {
                let remaining = l.remaining_at(now).as_secs();
                format!("{}:{}:{}s", l.owner, l.cores, remaining)
            })
            .collect();
        Ok((online, leased, on_charger(), descriptions))
    }

    /// External event notification — logs battery stats with the given label.
    /// Called by compositor (display-on/display-off), radiod, etc.
    async fn notify_event(&self, event: String) -> String {
        log_battery_event(&event);
        "ok".to_string()
    }
}

// --- Sysfs helpers ---

fn read_sysfs(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_sysfs_i64(path: &str) -> Option<i64> {
    read_sysfs(path)?.parse().ok()
}

fn on_charger() -> bool {
    // Preserve the established policy/status fallback while keeping failed
    // observations distinct for transition logging.
    charger_state(std::path::Path::new("/sys/class/power_supply")).unwrap_or(false)
}

fn charger_state(root: &std::path::Path) -> Option<bool> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut observed = false;
    let mut unknown = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unknown = true;
                continue;
            }
        };
        match std::fs::read_to_string(entry.path().join("online")) {
            Ok(value) => match value.trim() {
                "1" => return Some(true),
                "0" => observed = true,
                _ => unknown = true,
            },
            // Battery/BMS supplies need not expose an online attribute.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unknown = true,
        }
    }
    (observed && !unknown).then_some(false)
}

fn wifi_up() -> Option<bool> {
    interface_up(std::path::Path::new("/sys/class/net/wlan0"))
}

fn interface_up(interface: &std::path::Path) -> Option<bool> {
    // A registered interface can be administratively down. IFF_UP describes
    // interface enablement, not association or Internet connectivity.
    match std::fs::read_to_string(interface.join("flags")) {
        Ok(flags) => u32::from_str_radix(flags.trim().strip_prefix("0x")?, 16)
            .ok()
            .map(|flags| flags & 1 != 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match interface.try_exists() {
                Ok(false) => Some(false),
                _ => None,
            }
        }
        Err(_) => None,
    }
}

fn cores_online() -> u32 {
    let mut count = 1; // cpu0 is always online
    for path in &CORE_PATHS {
        if let Ok(val) = std::fs::read_to_string(path) {
            if val.trim() == "1" {
                count += 1;
            }
        }
    }
    count
}

// --- Battery logging ---

fn log_battery_event(reason: &str) {
    let current_ua = read_sysfs_i64("/sys/class/power_supply/battery/current_now");
    let voltage_uv = read_sysfs_i64("/sys/class/power_supply/battery/voltage_now");
    let capacity = read_sysfs_i64("/sys/class/power_supply/battery/capacity");
    let charge_counter = read_sysfs_i64("/sys/class/power_supply/bms/charge_counter");
    let status = read_sysfs("/sys/class/power_supply/battery/status").unwrap_or_else(|| "?".into());
    let cores = cores_online();

    let current_ma = current_ua.map(|v| v as f64 / 1000.0);
    let voltage_mv = voltage_uv.map(|v| v as f64 / 1000.0);

    eprintln!(
        "battery: [{}] {}mA {}mV {}% cc={}uAh {} cores={}",
        reason,
        current_ma.map_or("?".into(), |v| format!("{:.1}", v)),
        voltage_mv.map_or("?".into(), |v| format!("{:.0}", v)),
        capacity.unwrap_or(-1),
        charge_counter.unwrap_or(-1),
        status,
        cores,
    );
}

// --- Core management ---

fn granted_cores(charging: bool, extra: u32) -> u32 {
    if charging {
        MAX_CORES
    } else {
        extra.saturating_add(1).min(MAX_CORES)
    }
}

fn write_sysfs_if_changed(path: &str, value: &str) -> std::io::Result<()> {
    if read_sysfs(path).as_deref() == Some(value) {
        return Ok(());
    }
    std::fs::write(path, value).map_err(|e| std::io::Error::new(e.kind(), format!("{path}: {e}")))
}

fn set_cores_online(desired: u32) -> std::io::Result<()> {
    for (i, path) in CORE_PATHS.iter().enumerate() {
        let value = if (i as u32 + 2) <= desired { "1" } else { "0" };
        write_sysfs_if_changed(path, value)?;
    }
    write_sysfs_if_changed(
        "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
        "ondemand",
    )?;
    // Kernel bool module parameters read back as Y/N, not 1/0.
    write_sysfs_if_changed("/sys/module/lpm_levels/parameters/sleep_disabled", "N")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn delayed_periodic_work_does_not_replay_missed_observations() {
        for period in [Duration::from_secs(5), Duration::from_secs(60)] {
            let mut interval = periodic_interval(period);
            // Preserve the immediate startup observation.
            let started = tokio::time::Instant::now();
            interval.tick().await;
            assert_eq!(tokio::time::Instant::now(), started);
            tokio::time::advance(period * 4 + Duration::from_secs(1)).await;
            interval.tick().await;
            // One current observation is enough after the delay. The default
            // Burst behavior would make this next tick immediately ready.
            assert!(tokio::time::timeout(Duration::from_secs(1), interval.tick())
                .await
                .is_err());
            interval.tick().await;
            assert_eq!(tokio::time::Instant::now(), started + period * 5);
        }
    }

    #[test]
    fn lease_expires_at_deadline_and_after_sleep_sized_boot_jump() {
        let lease = Lease {
            cores: 2,
            expires: Duration::from_secs(100 + 300),
            owner: "test".into(),
        };
        let before = Duration::new(399, 999_999_999);
        assert!(lease.active_at(before));
        assert_eq!(lease.remaining_at(before), Duration::from_nanos(1));
        for now in [Duration::from_secs(400), Duration::from_secs(3700)] {
            assert!(!lease.active_at(now));
            assert_eq!(lease.remaining_at(now), Duration::ZERO);
        }
    }

    #[tokio::test]
    async fn status_and_reconciliation_agree_on_expired_boot_deadlines() {
        let state = PowerState::new();
        let now = boottime().unwrap();
        assert!(now > Duration::ZERO);
        {
            let mut leases = state.leases.lock().await;
            leases.insert(
                "expired".into(),
                Lease {
                    cores: 3,
                    expires: Duration::ZERO,
                    owner: "expired".into(),
                },
            );
            leases.insert(
                "active".into(),
                Lease {
                    cores: 1,
                    expires: now + Duration::from_secs(300),
                    owner: "active".into(),
                },
            );
        }
        let manager = PowerManager {
            state: state.clone(),
        };
        let (_, leased, _, descriptions) = manager.status().await.unwrap();
        assert_eq!(leased, 1);
        assert_eq!(descriptions.len(), 1);
        assert!(descriptions[0].starts_with("active:1:"));
        // Compute the policy without writing host CPU or power sysfs files.
        state.desired_cores().await.unwrap();
        let leases = state.leases.lock().await;
        assert_eq!(leases.len(), 1);
        assert!(leases.contains_key("active"));
    }

    #[test]
    fn status_keeps_its_dbus_success_signature() {
        use zbus::object_server::Interface;
        let manager = PowerManager {
            state: PowerState::new(),
        };
        let mut xml = String::new();
        manager.introspect_to_writer(&mut xml, 0);
        let status = xml
            .split("<method name=\"Status\">")
            .nth(1)
            .unwrap()
            .split("</method>")
            .next()
            .unwrap();
        assert_eq!(status.matches("direction=\"out\"").count(), 4);
        assert_eq!(status.matches("type=\"u\"").count(), 2);
        assert_eq!(status.matches("type=\"b\"").count(), 1);
        assert_eq!(status.matches("type=\"as\"").count(), 1);
    }

    #[test]
    fn leases_add_to_always_online_core_and_cap_at_four() {
        assert_eq!(
            (0..=4).map(|n| granted_cores(false, n)).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 4]
        );
        assert_eq!(granted_cores(false, u32::MAX), 4);
        assert_eq!(granted_cores(true, 0), 4);
    }

    #[test]
    fn charger_observation_distinguishes_unknown_from_disconnected() {
        let root = std::env::temp_dir().join(format!("powerd-supplies-{}", uuid::Uuid::new_v4()));
        assert_eq!(charger_state(&root), None);
        std::fs::create_dir_all(root.join("battery")).unwrap();
        assert_eq!(charger_state(&root), None);
        std::fs::create_dir(root.join("usb")).unwrap();
        let online = root.join("usb/online");
        std::fs::write(&online, "0\n").unwrap();
        assert_eq!(charger_state(&root), Some(false));
        for value in [b"invalid".as_slice(), b"", b"2", b"\xff"] {
            std::fs::write(&online, value).unwrap();
            assert_eq!(charger_state(&root), None);
        }
        std::fs::create_dir(root.join("wireless")).unwrap();
        std::fs::write(root.join("wireless/online"), "1\n").unwrap();
        // A known external source is sufficient even if another is unreadable.
        assert_eq!(charger_state(&root), Some(true));
        std::fs::write(root.join("wireless/online"), "0\n").unwrap();
        assert_eq!(charger_state(&root), None);
        std::fs::write(&online, "0\n").unwrap();
        assert_eq!(charger_state(&root), Some(false));
        std::fs::remove_file(&online).unwrap();
        std::fs::create_dir(&online).unwrap();
        assert_eq!(charger_state(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registered_interface_requires_up_flag_and_unknown_is_not_down() {
        let interface = std::env::temp_dir().join(format!("powerd-net-{}", uuid::Uuid::new_v4()));
        assert_eq!(interface_up(&interface), Some(false));
        std::fs::create_dir(&interface).unwrap();
        assert_eq!(interface_up(&interface), None);
        for (flags, expected) in [
            ("0x1002\n", Some(false)),
            ("0x1003\n", Some(true)),
            ("0x1042\n", Some(false)),
            ("0x11003\n", Some(true)),
            ("invalid\n", None),
            ("0x100000000\n", None),
        ] {
            std::fs::write(interface.join("flags"), flags).unwrap();
            assert_eq!(interface_up(&interface), expected, "{flags}");
        }
        std::fs::remove_file(interface.join("flags")).unwrap();
        std::fs::remove_dir(interface).unwrap();
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = PowerState::new();

    // Initial reconcile — set cores based on charger state
    state.reconcile().await?;

    // Log initial battery state
    log_battery_event("startup");

    // Spawn a timer to periodically reconcile (expire leases, react to charger changes)
    let sweep_state = state.clone();
    tokio::spawn(async move {
        let mut interval = periodic_interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(e) = sweep_state.reconcile().await {
                eprintln!("core reconciliation failed: {e}");
            }
        }
    });

    // Spawn a timer to log battery stats every 60s
    tokio::spawn(async move {
        let mut interval = periodic_interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            log_battery_event("periodic");
        }
    });

    let manager = PowerManager {
        state: state.clone(),
    };

    let _conn = connection::Builder::system()?
        .name("org.hoki.power")?
        .serve_at("/org/hoki/power", manager)?
        .build()
        .await?;

    eprintln!("hoki-powerd: running on system bus");

    // Block forever
    std::future::pending::<()>().await;

    Ok(())
}
