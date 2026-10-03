//! Apply the union of logical consumers inside the one durable HAL capture.
use crate::{collection_profile, owned_request, Result};
use crate::spo2_schedule::{self, Schedule, Results};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

type Plan = BTreeMap<i64, Value>;
pub fn union(inventory: &Value, profiles: &[Value], optical: bool) -> Result<Plan> {
    let mut result = Plan::new();
    for profile in profiles {
        let name = profile.as_str().or_else(||profile["profile"].as_str()).ok_or("invalid consumer profile")?;
        if name == "off" { continue; }
        let mut selected = collection_profile::plan(inventory,
            if matches!(name,"running"|"spo2") {"full"} else {name})?;
        if name == "running" {
            selected.retain(|s| matches!(s["sensor"]["type"].as_i64(),Some(1|4|18|19|21|31|65574|65561)));
            for required in [1,4,21,31,65574,65561] {
                if !selected.iter().any(|s| s["sensor"]["type"] == required) {
                    return Err(format!("running sensor {required} unavailable").into());
                }
            }
        } else if name == "spo2" {
            selected.retain(|s| s["sensor"]["type"] == 65561);
            if selected.is_empty() { return Err("SpO2 unavailable".into()); }
        }
        let rates=profile.get("rates_hz").map(|r|r.as_object().ok_or("invalid rates_hz")).transpose()?;
        if let Some(rates)=rates {
            for (key,rate) in rates {
                let typ=key.parse::<i64>()?;
                if key!=&typ.to_string() || !matches!(typ,1|2|4|6|9|10|11|14|15|16|20|35|65572) ||
                    !selected.iter().any(|s|s["sensor"]["type"]==typ) {
                    return Err("rate override requires a selected adjustable sensor".into());
                }
                let hz=rate.as_f64().ok_or("invalid rate")?;
                if !hz.is_finite() || !(0.1..=1000.0).contains(&hz) {return Err("rate out of range".into());}
            }
        }
        for mut s in selected {
            let typ = s["sensor"]["type"].as_i64().ok_or("missing type")?;
            let hz=rates.and_then(|r|r.get(&typ.to_string())).and_then(Value::as_f64)
                .or_else(|| if name=="running" && matches!(typ,1|4) {Some(50.0)} else {None});
            if let Some(hz)=hz {
                let min=s["sensor"]["min_delay_us"].as_u64().unwrap_or(0).saturating_mul(1000);
                let max=s["sensor"]["max_delay_us"].as_u64().filter(|n|*n>0).unwrap_or(60_000_000).saturating_mul(1000);
                if min>max || min>60_000_000_000 {return Err("invalid sensor rate limits".into());}
                s["period_ns"]=json!(((1e9/hz).floor() as u64).clamp(min.max(1),max.min(60_000_000_000)));
            }
            if (typ == 65561 && !optical) || (matches!(typ,31|65574) && optical) { continue; }
            let handle = s["sensor"]["handle"].as_i64().ok_or("missing handle")?;
            if let Some(old) = result.get_mut(&handle) {
                old["period_ns"] = json!(old["period_ns"].as_u64().unwrap().min(s["period_ns"].as_u64().unwrap()));
            } else { result.insert(handle,s); }
        }
    }
    Ok(result)
}

pub struct Broker {
    path: PathBuf, current: Plan, schedule: Schedule, results: Results,
    epoch: String,
    buffered_full: bool,
    applied_since: f64,
    pub revision: u64, pub optical: bool, success: bool, cooldown: Option<PathBuf>, boot: String,
}
impl Broker {
    pub fn new(path: PathBuf, now: f64) -> Result<Self> {
        let boot=fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().to_string();
        let cooldown=std::env::var_os("HOKI_SPO2_COOLDOWN_FILE").map(PathBuf::from);
        let wait=if let Some(path)=cooldown.as_ref().filter(|p|p.exists()) {
            spo2_schedule::cooldown_remaining(&serde_json::from_slice(&fs::read(path)?)?,&boot,now,
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64())?
        } else {0.0};
        let buffered_full=match std::env::var("HOKI_SHARED_FULL_BUFFERED").as_deref() {
            Ok("1")=>true, Ok("0")|Err(std::env::VarError::NotPresent)=>false,
            _=>return Err("HOKI_SHARED_FULL_BUFFERED must be 0 or 1".into()),
        };
        Ok(Self {buffered_full,path,current:Plan::new(),schedule:Schedule{active:false,deadline:now+wait,started:0.0},
            results:Results::default(), epoch:String::new(), applied_since:now, revision:0,optical:false,success:false,cooldown,boot})
    }
    pub fn update(&mut self, inventory: &Value, socket: &Path, token: &str, directory: &Path, now: f64) -> Result<()> {
        let bytes=fs::read(&self.path)?;
        if bytes.len()>16384 {return Err("consumer plan too large".into());}
        let request: Value=serde_json::from_slice(&bytes)?;
        if request["version"]!=1 {return Err("unknown consumer protocol".into());}
        let epoch=request["epoch"].as_str().filter(|s|crate::valid_session_id(s)).ok_or("invalid broker epoch")?;
        let revision=request["revision"].as_u64().ok_or("missing revision")?;
        let profiles=request["profiles"].as_array().ok_or("missing consumers")?;
        let wants=profiles.iter().any(|p| matches!(p.as_str(),Some("full"|"running"|"spo2")));
        let immediate=profiles.iter().any(|p|p=="spo2");
        if immediate && profiles.iter().any(|p|p=="running") {return Err("conflicting optical requests".into());}
        if self.schedule.active && (self.schedule.due(now)||self.success||!wants) {
            self.schedule.released(now,self.success);
            self.success=false;
        }
        // Manual clients own a bounded 180 s lease; periodic demand shares the same window.
        if wants && !self.schedule.active && (self.schedule.due(now) || (immediate && revision!=self.revision)) {
            if let Some(path)=&self.cooldown {
                spo2_schedule::persist_attempt(path,&self.boot,now)?;
            }
            self.schedule.activated(now);
        }
        self.optical=wants && self.schedule.active;
        let subscriptions=request.get("subscriptions").map(|v|v.as_array().ok_or("invalid subscriptions")).transpose()?;
        if let Some(subscriptions)=subscriptions {
            let names:Vec<_>=subscriptions.iter().map(|s|s["profile"].clone()).collect();
            if names!=*profiles {return Err("inconsistent subscription profiles".into());}
        }
        let mut wanted=union(inventory,subscriptions.unwrap_or(profiles),self.optical)?;
        if self.buffered_full && profiles.iter().all(|p|p=="off" || p=="full") {
            for item in wanted.values_mut() {
                if item["sensor"]["flags"].as_u64().is_some_and(|f| ((f>>1)&7)==0) {
                    let period=item["period_ns"].as_u64().ok_or("missing period")?;
                    let latency=(7_000_000_000u64/period)*period;
                    item["latency_ns"]=json!(latency);
                    item["batching_mode"]=json!(match item["sensor"]["fifo_reserved"].as_u64() {
                        None=>"trial_fifo_metadata_unknown",Some(0)=>"trial_unreserved_fifo",
                        Some(n) if n.saturating_mul(period)>=latency=>"advertised_reserved_fifo_window",
                        _=>"trial_over_reserved_fifo",
                    });
                } else {
                    item["batching_mode"]=json!("non_continuous_immediate");
                }
            }
        }
        let changed=wanted!=self.current || revision!=self.revision || epoch!=self.epoch;
        if changed {
            let mut f=OpenOptions::new().append(true).create(true).mode(0o600).open(directory.join("consumer-transitions.jsonl"))?;
            writeln!(f,"{}",json!({"phase":"requested","boottime_seconds":now,"epoch":epoch,"revision":revision,
                "profiles":profiles,"optical_window":self.optical,"requested_demands":wanted.values().collect::<Vec<_>>()}))?;
            f.sync_all()?;
        }
        // Release conflicting old demands BEFORE enabling the new optical mode.
        for (&handle, old) in &self.current {
            if !wanted.contains_key(&handle) {
                owned_request(socket,token,json!({"command":"demand","handle":handle,"active":false,
                    "period_ns":old["period_ns"],"latency_ns":old["latency_ns"]}))?;
            }
        }
        for (&handle, item) in &wanted {
            if self.current.get(&handle)!=Some(item) {
                owned_request(socket,token,json!({"command":"demand","handle":handle,"active":true,
                    "period_ns":item["period_ns"],"latency_ns":item["latency_ns"]}))?;
            }
        }
        self.current=wanted; self.revision=revision; self.epoch=epoch.to_string();
        if changed {self.applied_since=now;}
        if changed {
            let mut f=OpenOptions::new().append(true).create(true).mode(0o600).open(directory.join("consumer-transitions.jsonl"))?;
            writeln!(f,"{}",json!({"phase":"applied","boottime_seconds":now,"revision":revision,"epoch":self.epoch,"profiles":profiles,
                "optical_window":self.optical,"acknowledged_demands":self.current.values().collect::<Vec<_>>()}))?;
            f.sync_all()?;
        }
        Ok(())
    }
    pub fn suspend_capable(&self) -> bool {
        self.buffered_full && collection_profile::buffered_full_trial_safe(
            &self.current.values().cloned().collect::<Vec<_>>(),7_000_000_000,17_000_000_000)
    }
    pub fn maintenance_interval(&self) -> f64 {
        if self.suspend_capable() {8.0} else {1.0}
    }
    pub fn selected(&self) -> Vec<Value> { self.current.values().cloned().collect() }
    pub fn next_interval(&self, now:f64) -> f64 {
        if self.suspend_capable() {8.0f64.min((self.schedule.deadline-now).max(0.001))} else {1.0}
    }
    pub fn checkpoint(&mut self,directory:&Path,now:f64)->Result<()> {
        let attempt=self.current.values().find(|s|s["sensor"]["type"]==65561)
            .map(|s|(s["sensor"]["handle"].as_u64().unwrap() as u32,self.schedule.started,now));
        self.success |= self.results.scan(directory,attempt)?;
        let cp:Value=serde_json::from_slice(&fs::read(directory.join("checkpoint.json"))?)?;
        let mut hr=Value::Null;
        let mut spo2=Value::Null;
        let mut timing:BTreeMap<u32,(i64,i64,u64)>=BTreeMap::new();
        let segment=cp["segment"].as_u64().ok_or("invalid segment")?;
        let end=cp["segment_bytes"].as_u64().ok_or("invalid checkpoint")?;
        if end>=104 {
            let mut f=crate::recording_io::segment(directory,&cp,segment)?;
            let start=16+((end-16)/88).saturating_sub(4096)*88;
            f.seek(SeekFrom::Start(start))?;
            let mut record=[0u8;88];
            for _ in 0..(end-start)/88 {
                f.read_exact(&mut record)?;
                let typ=u32::from_le_bytes(record[20..24].try_into().unwrap());
                let handle=u32::from_le_bytes(record[16..20].try_into().unwrap());
                let stamp=i64::from_le_bytes(record[8..16].try_into().unwrap());
                let seconds=stamp as f64/1e9;
                if matches!(typ,1|2|4|6|9|10|11|14|15|16|20|35|65572) && self.current.contains_key(&(handle as i64)) &&
                    seconds>=self.applied_since && (0.0..=5.0).contains(&(now-seconds)) {
                    let stat=timing.entry(typ).or_insert((stamp,stamp,0));
                    if stat.2==0 || stamp>stat.1 {stat.1=stamp;stat.2+=1;}
                }
                if u32::from_le_bytes(record[20..24].try_into().unwrap())==65561 {
                    let values:Vec<f32>=record[24..40].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
                    let timestamp=i64::from_le_bytes(record[8..16].try_into().unwrap());
                    if values.iter().all(|v|v.is_finite()) && timestamp>=0 {
                        spo2=json!({"timestamp_us":timestamp/1000,"oxygen":values[0],"confidence":values[1],
                            "algorithm":values[2],"signal":values[3]});
                    }
                }
                if u32::from_le_bytes(record[20..24].try_into().unwrap())==21 {
                    let bpm=f32::from_le_bytes(record[24..28].try_into().unwrap());
                    let timestamp=i64::from_le_bytes(record[8..16].try_into().unwrap()) as f64/1e9;
                    if bpm.is_finite() && bpm>0.0 && now-timestamp>=0.0 && now-timestamp<10.0 {
                        hr=json!({"bpm":bpm,"boottime_seconds":timestamp});
                    }
                }
            }
        }
        let observed:serde_json::Map<String,Value>=timing.into_iter().filter(|(_,s)|s.2>=3 && s.1-s.0>=500_000_000)
            .map(|(typ,(first,last,count))|(typ.to_string(),json!((count-1) as f64*1e9/(last-first) as f64))).collect();
        let value=json!({"version":1,"revision":self.revision,"epoch":self.epoch,"ready":true,"error":null,
            "boottime_seconds":now,"status_valid_seconds":self.maintenance_interval()+5.0,"buffered_full":self.suspend_capable(),"optical_window":self.optical,"heart_rate":hr,"spo2":spo2,
            "applied_rates_hz":applied_rates(&self.current),"observed_rates_hz":observed});
        let temp=directory.join("broker-status.tmp");
        fs::write(&temp,serde_json::to_vec(&value)?)?;
        fs::rename(temp,directory.join("broker-status.json"))?;
        Ok(())
    }
}

fn applied_rates(plan:&Plan)->Value {
    let mut rates=serde_json::Map::new();
    for item in plan.values() {
        let typ=item["sensor"]["type"].as_i64().unwrap();
        if matches!(typ,1|2|4|6|9|10|11|14|15|16|20|35|65572) {
            rates.insert(typ.to_string(),json!(1e9/item["period_ns"].as_u64().unwrap() as f64));
        }
    }
    Value::Object(rates)
}

#[cfg(test)] mod tests {
    use super::*;
    fn inventory()->Value {json!({"sensors":([1,4,18,19,21,31,65561,65574,65572].map(|t|
        json!({"type":t,"handle":t,"flags":1,"min_delay_us":0,"max_delay_us":0})))})}
    #[test] fn union_preserves_full_and_fastest_rates() {
        let inv=inventory();
        let both=union(&inv,&[json!("sleep"),json!("running"),json!("full")],false).unwrap();
        assert!(both.contains_key(&65572)); assert!(both.contains_key(&65574));
        assert_eq!(both[&1]["period_ns"],20_000_000);
        assert!(!both.contains_key(&65561));
    }
    #[test] fn optical_windows_remove_only_conflicting_channels() {
        let inv=inventory(); let plan=union(&inv,&[json!("running")],true).unwrap();
        assert!(plan.contains_key(&65561)); assert!(plan.contains_key(&21));
        assert!(!plan.contains_key(&31)); assert!(!plan.contains_key(&65574));
        assert!(union(&inv,&[json!("off")],false).unwrap().is_empty());
        assert!(union(&inv,&[json!("bad")],false).is_err());
    }
    #[test] fn rates_override_defaults_and_fastest_remaining_consumer_wins() {
        let mut inv=inventory();
        for s in inv["sensors"].as_array_mut().unwrap() {s["min_delay_us"]=json!(20000);s["max_delay_us"]=json!(1000000);}
        let slow=json!({"profile":"running","rates_hz":{"1":10,"4":10}});
        let fast=json!({"profile":"running","rates_hz":{"1":50,"4":50}});
        let alone=union(&inv,&[slow.clone()],false).unwrap();
        assert_eq!(alone[&1]["period_ns"],100_000_000);
        let both=union(&inv,&[slow.clone(),fast.clone()],false).unwrap();
        assert_eq!(applied_rates(&both)["1"],50.0);
        assert_eq!(union(&inv,&[fast,slow.clone()],false).unwrap(),both);
        assert_eq!(applied_rates(&union(&inv,&[slow],false).unwrap())["1"],10.0);
        let clamped=union(&inv,&[json!({"profile":"running","rates_hz":{"1":1000,"4":0.1}})],false).unwrap();
        assert_eq!(applied_rates(&clamped)["1"],50.0);
        assert_eq!(applied_rates(&clamped)["4"],1.0);
        for rates in [json!({"1":0}),json!({"1":true}),json!({"21":10}),json!({"65572":25}),json!({"01":25})] {
            assert!(union(&inv,&[json!({"profile":"running","rates_hz":rates})],false).is_err());
        }
    }
}

#[cfg(test)]
#[path = "consumer_broker_tests.rs"]
mod integration_tests;
