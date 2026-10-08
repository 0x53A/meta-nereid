//! Single owner of everyday suspend. All grants and final suspend entry serialize
//! on State; new work cancels preparation and waits for the logind transaction.
use crate::{
    sleep_client,
    sleep_policy::{self, Config, Owner, Sensor, Ui},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const CONFIG: &str = "/var/lib/hoki-powerd/sleep.json";
struct State {
    config: Config,
    owners: BTreeMap<u64, Owner>,
    generation: u64,
    startup_until: f64,
    retry_after: f64,
    failures: u32,
    last_result: String,
    sensor_fault: bool,
    transition: Option<f64>,
    cancelled: bool,
    external_blockers: Vec<String>,
}
fn now() -> f64 {
    sleep_client::boottime().unwrap_or(f64::INFINITY)
}
fn power_safe() -> bool {
    crate::charger_state(Path::new("/sys/class/power_supply")) == Some(false)
        && fs::read_to_string("/sys/class/android_usb/android0/state")
            .map(|s| s.trim() == "DISCONNECTED")
            .unwrap_or(false)
}
impl State {
    fn decision(&self) -> sleep_policy::Decision {
        let mut d = sleep_policy::decide(
            &self.config,
            &self.owners,
            now(),
            self.startup_until,
            self.retry_after,
            power_safe(),
        );
        if !self.config.enabled {
            return d;
        }
        // Connected-radio suspend preparation is hardware-specific and not yet
        // verified in the integrated path. Preserve networking instead of letting
        // an unconfigured wiphy suspend silently disconnect the station.
        if crate::wifi_up() != Some(false) && d.sleep_until.is_some() {
            d.sleep_until = None;
            d.reason = "Wi-Fi is active; connected suspend needs validation".into();
        }
        if !self.external_blockers.is_empty() {
            d.sleep_until = None;
            d.reason = format!("logind: {}", self.external_blockers.join("; "));
        }
        if !self.owners.values().any(|o| o.sensor.is_some())
            && Path::new("/run/systemd/system/sensorfwd.service.d/80-health-recording.conf")
                .exists()
        {
            d.sleep_until = None;
            d.reason = "recording setup or recovery pending".into();
        }
        if self.cancelled {
            d.sleep_until = None;
            d.reason = "sleep preparation cancelled by new work".into();
        }
        if self.sensor_fault {
            d.sleep_until = None;
            d.reason = "sensor owner disconnected; cleanup required".into();
        }
        d
    }
    fn status(&self) -> Value {
        let d = self.decision();
        json!({"ok":true,"config":self.config,"generation":self.generation,
            "ui":self.owners.values().find_map(|o| o.ui.as_ref()).map(|ui| json!({"display":ui.display,"ready":ui.ready,"revision":ui.revision})),"reason":d.reason,"sleep_until":d.sleep_until,
            "auto_cores":crate::auto_cores::status(),"last_result":self.last_result,"sensor_fault":self.sensor_fault,
            "inhibitors":self.owners.iter().filter(|(_,o)|o.cpu||o.display)
                .map(|(id,o)|json!({"id":id,"cpu":o.cpu,"display":o.display,"reason":o.reason})).collect::<Vec<_>>()})
    }
}
fn persist(config: &Config) -> io::Result<()> {
    fs::create_dir_all("/var/lib/hoki-powerd")?;
    let temp = format!("{CONFIG}.pending");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    serde_json::to_writer(&mut f, config)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(temp, CONFIG)?;
    File::open("/var/lib/hoki-powerd")?.sync_all()
}
fn finite(v: &Value, key: &str) -> Result<f64, String> {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .ok_or_else(|| format!("invalid {key}"))
}
fn boolean(v: &Value, key: &str) -> Result<bool, String> {
    v[key].as_bool().ok_or_else(|| format!("invalid {key}"))
}
fn command(state: &mut State, id: u64, uid: u32, v: Value) -> Result<Value, String> {
    if v["version"] != 1 {
        return Err("unsupported protocol version".into());
    }
    match v["command"].as_str().unwrap_or("") {
        "status" => {}
        "commit-sleep" => {
            if uid != 0 {
                return Err("sleep gate requires root".into());
            }
            let deadline = state
                .transition
                .ok_or("sleep was not coordinated by powerd")?;
            if state.decision().sleep_until.is_none() || deadline <= now() + 2. {
                return Err("sleep readiness changed".into());
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            if !runtime.block_on(crate::logind::blockers())?.is_empty() {
                return Err("new logind inhibitor".into());
            }
            if deadline <= now() + 2. {
                return Err("wake deadline expired during preparation".into());
            }
            let mut count = String::new();
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/sys/power/wakeup_count")
                .and_then(|mut f| f.read_to_string(&mut count))
                .map_err(|e| e.to_string())?;
            let count: u32 = count.trim().parse().map_err(|_| "invalid wakeup count")?;
            fs::write("/sys/power/wakeup_count", count.to_string()).map_err(|e| e.to_string())?;
        }
        "configure-brightness" => {
            let mut config = state.config.clone();
            config.brightness = config.brightness.patched(&v["patch"])?;
            persist(&config).map_err(|e| e.to_string())?;
            state.config = config;
        }
        "configure-auto-cores" => {
            let mut config = state.config.clone();
            config.auto_cores = crate::auto_cores::patched(&config.auto_cores, &v["patch"])?;
            persist(&config).map_err(|e| e.to_string())?;
            crate::auto_cores::configure(&config.auto_cores);
            state.config = config;
        }
        "configure" | "configure-patch" => {
            let config: Config = if v["command"] == "configure-patch" {
                state.config.patched(&v["patch"])?
            } else {
                serde_json::from_value(v["config"].clone()).map_err(|e| e.to_string())?
            };
            config.validate()?;
            persist(&config).map_err(|e| e.to_string())?;
            crate::auto_cores::configure(&config.auto_cores);
            state.config = config;
            state.generation = state.generation.wrapping_add(1);
        }
        "inhibit" => {
            let cpu = boolean(&v, "cpu")?;
            let display = boolean(&v, "display")?;
            if (cpu || display) && state.transition.is_some() {
                state.cancelled = true;
                return Err("sleep transition in progress; retry request".into());
            }
            let reason = v["reason"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 160)
                .ok_or("invalid inhibitor reason")?
                .to_string();
            let o = state.owners.get_mut(&id).ok_or("unknown owner")?;
            o.cpu = cpu;
            o.display = display;
            o.reason = reason;
        }
        "ui" => {
            if state
                .owners
                .iter()
                .any(|(other, o)| *other != id && o.ui.is_some())
            {
                return Err("compositor already registered".into());
            }
            let display = v["display"]
                .as_str()
                .filter(|s| matches!(*s, "interactive" | "ambient" | "off"))
                .ok_or("invalid display")?;
            let ready = boolean(&v, "ready")?;
            let revision = v["revision"].as_u64().ok_or("invalid revision")?;
            let owner = state.owners.get_mut(&id).ok_or("unknown owner")?;
            if owner.ui.as_ref().is_some_and(|ui| revision < ui.revision
                || (revision == ui.revision && (ui.ready != ready || ui.display != display))) {
                return Err("stale UI readiness revision".into());
            }
            if ready && display == "interactive" {
                return Err("interactive display cannot be suspend-ready".into());
            }
            if !ready && state.transition.is_some() { state.cancelled = true; }
            owner.ui = Some(Ui { at: now(), display:display.into(), ready, revision });
        }
        "sensor" => {
            if uid != 0 {
                return Err("sensor registration requires root".into());
            }
            let deadline = finite(&v, "deadline")?;
            if deadline > now() + 65. {
                return Err("sensor deadline exceeds safety bound".into());
            }
            let profile = v["profile"]
                .as_str()
                .filter(|s| matches!(*s, "daily" | "sleep" | "activity" | "full"))
                .ok_or("invalid sensor profile")?;
            state.owners.get_mut(&id).ok_or("unknown owner")?.sensor = Some(Sensor {
                ready: boolean(&v, "ready")?,
                deadline,
                profile: profile.into(),
            });
        }
        "sensor-closed" => {
            if uid != 0 {
                return Err("sensor cleanup requires root".into());
            }
            state.owners.get_mut(&id).ok_or("unknown owner")?.sensor = None;
        }
        "sensor-recovered" => {
            if uid != 0 {
                return Err("sensor recovery requires root".into());
            }
            if state.owners.values().any(|o| o.sensor.is_some()) {
                return Err("sensor owner still active".into());
            }
            state.sensor_fault = false;
        }
        _ => return Err("unknown command".into()),
    }
    Ok(state.status())
}
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}
fn client(mut stream: UnixStream, id: u64, state: Arc<Mutex<State>>) -> io::Result<()> {
    let uid = peer_uid(&stream)?;
    if uid != 0 && uid != 1000 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "root/ceres only",
        ));
    }
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    state.lock().unwrap().owners.insert(id, Owner::default());
    let outcome = (|| {
        let mut reader = BufReader::new(stream.try_clone()?);
        loop {
            let request = sleep_client::read_line(&mut reader)?;
            let reply = {
                let mut s = state.lock().unwrap();
                match command(&mut s, id, uid, request) {
                    Ok(reply) => reply,
                    Err(error) => json!({"ok":false,"error":error}),
                }
            };
            serde_json::to_writer(&mut stream, &reply)?;
            stream.write_all(b"\n")?;
        }
    })();
    let mut s = state.lock().unwrap();
    if s.owners.remove(&id).is_some_and(|o| o.sensor.is_some()) {
        s.sensor_fault = true;
    }
    outcome
}

/// Keep a bounded wake alarm alive throughout the logind transaction.
/// The final systemd gate performs the wakeup_count handshake separately.
fn arm_alarm(until: f64) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::timerfd_create(
            libc::CLOCK_BOOTTIME_ALARM,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let alarm = unsafe { OwnedFd::from_raw_fd(fd) };
    let timer = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 3,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: until.floor() as _,
            tv_nsec: ((until.fract()) * 1e9) as _,
        },
    };
    if unsafe {
        libc::timerfd_settime(
            alarm.as_raw_fd(),
            libc::TFD_TIMER_ABSTIME,
            &timer,
            std::ptr::null_mut(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // Recurrence prevents a slow systemd preparation from consuming the only
    // wake before actual entry. The final gate still rejects expired deadlines.
    Ok(alarm)
}

pub fn start() -> io::Result<()> {
    let config = match fs::read(CONFIG) {
        Ok(bytes) => serde_json::from_slice::<Config>(&bytes).map_err(io::Error::other)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e),
    };
    config.validate().map_err(io::Error::other)?;
    crate::auto_cores::configure(&config.auto_cores);
    fs::create_dir_all("/run/hoki-powerd")?;
    fs::set_permissions("/run/hoki-powerd", fs::Permissions::from_mode(0o755))?;
    match fs::remove_file(sleep_client::SOCKET) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(sleep_client::SOCKET)?;
    fs::set_permissions(sleep_client::SOCKET, fs::Permissions::from_mode(0o666))?;
    let state = Arc::new(Mutex::new(State {
        config,
        owners: BTreeMap::new(),
        generation: 1,
        startup_until: now() + 30.,
        retry_after: 0.,
        failures: 0,
        last_result: "not attempted".into(),
        sensor_fault: false,
        transition: None,
        cancelled: false,
        external_blockers: vec!["not checked".into()],
    }));
    let shared = state.clone();
    std::thread::spawn(move || {
        for (id, stream) in listener.incoming().enumerate() {
            if let Ok(stream) = stream {
                let state = shared.clone();
                // Bound connection/thread resources. Local peer credentials are
                // still checked in the worker before any command is accepted.
                if state.lock().unwrap().owners.len() >= 64 {
                    continue;
                }
                std::thread::spawn(move || {
                    let _ = client(stream, id as u64, state);
                });
            }
        }
    });
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sleep runtime");
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if !state.lock().unwrap().config.enabled {
                continue;
            }
            let blockers = runtime
                .block_on(crate::logind::blockers())
                .unwrap_or_else(|e| vec![format!("unavailable: {e}")]);
            let until = {
                let mut s = state.lock().unwrap();
                s.external_blockers = blockers;
                let until = s.decision().sleep_until;
                if until.is_some() {
                    s.transition = until;
                }
                until
            };
            let Some(until) = until else {
                continue;
            };
            let boot = now();
            let awake = std::time::Instant::now();
            let result =
                arm_alarm(until).and_then(|_alarm| runtime.block_on(crate::logind::suspend()));
            crate::auto_cores::sleep_completed();
            let residency = (now() - boot - awake.elapsed().as_secs_f64()).max(0.);
            let mut s = state.lock().unwrap();
            s.transition = None;
            s.cancelled = false;
            s.last_result = match result {
                Ok(()) => format!("residency {residency:.3}s"),
                Err(e) => format!("suspend failed: {e}"),
            };
            eprintln!("sleep: {}", s.last_result);
            if residency < 0.5 {
                s.failures = s.failures.saturating_add(1);
            } else {
                s.failures = 0;
            }
            s.retry_after = now()
                + if s.failures == 0 {
                    1.
                } else {
                    (2u64.saturating_pow(s.failures.min(8))).min(300) as f64
                };
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State {
            config: Config::default(),
            owners: BTreeMap::from([(1, Owner::default()), (2, Owner::default())]),
            generation: 1,
            startup_until: 0.,
            retry_after: 0.,
            failures: 0,
            last_result: String::new(),
            sensor_fault: false,
            transition: None,
            cancelled: false,
            external_blockers: vec![],
        }
    }
    #[test]
    fn readiness_is_connection_owned_revision_checked_and_cancels_sleep() {
        let mut s=state();
        let report=json!({"version":1,"command":"ui","display":"off","ready":true,"revision":7});
        assert!(command(&mut s,1,1000,report.clone()).is_ok());
        assert!(command(&mut s,2,1000,report.clone()).is_err());
        let mut stale=report.clone(); stale["revision"]=json!(6);
        assert!(command(&mut s,1,1000,stale).is_err());
        let mut interactive=report.clone(); interactive["display"]=json!("interactive");
        assert!(command(&mut s,1,1000,interactive.clone()).is_err());
        s.transition=Some(now()+10.);
        interactive["ready"]=json!(false); interactive["revision"]=json!(8);
        assert!(command(&mut s,1,1000,interactive).is_ok());
        assert!(s.cancelled);
        s.owners.remove(&1);
        assert!(command(&mut s,2,1000,report).is_ok());
    }
    #[test]
    fn new_work_cancels_preparation_without_acknowledging_an_inhibitor() {
        let mut s = state();
        s.transition = Some(now() + 10.);
        let request =
            json!({"version":1,"command":"inhibit","cpu":true,"display":false,"reason":"audio"});
        assert!(command(&mut s, 1, 1000, request.clone()).is_err());
        assert!(s.cancelled);
        assert!(!s.owners[&1].cpu);
        s.transition = None;
        s.cancelled = false;
        assert!(command(&mut s, 1, 1000, request).is_ok());
        assert!(s.owners[&1].cpu);
        assert!(!s.owners[&2].cpu);
    }
    #[test]
    fn sensor_and_final_gate_require_root_and_deadlines_are_bounded() {
        let mut s = state();
        let request = json!({"version":1,"command":"sensor","ready":true,"profile":"sleep","deadline":now()+10.});
        assert!(command(&mut s, 1, 1000, request.clone()).is_err());
        assert!(command(&mut s, 1, 0, request).is_ok());
        assert!(command(&mut s,1,0,json!({"version":1,"command":"sensor","ready":true,"profile":"sleep","deadline":now()+1000.})).is_err());
        assert!(command(
            &mut s,
            1,
            1000,
            json!({"version":1,"command":"commit-sleep"})
        )
        .is_err());
        assert!(command(&mut s, 1, 0, json!({"version":1,"command":"commit-sleep"})).is_err());
    }
    #[test]
    fn owner_disconnect_releases_only_its_own_requests() {
        use std::io::{BufRead, Write};
        let shared = Arc::new(Mutex::new(state()));
        let (worker, mut peer) = UnixStream::pair().unwrap();
        let state = shared.clone();
        let server = std::thread::spawn(move || client(worker, 3, state));
        peer.write_all(b"{\"version\":1,\"command\":\"inhibit\",\"cpu\":true,\"display\":false,\"reason\":\"test\"}\n").unwrap();
        let mut reply = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut reply)
            .unwrap();
        assert!(reply.contains("\"ok\":true"));
        assert!(shared.lock().unwrap().owners[&3].cpu);
        drop(peer);
        let _ = server.join().unwrap();
        let s = shared.lock().unwrap();
        assert!(!s.owners.contains_key(&3));
        assert_eq!(s.owners.len(), 2);
    }
}
