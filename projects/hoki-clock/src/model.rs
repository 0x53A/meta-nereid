// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
use chrono::{DateTime, Datelike, Local, LocalResult, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const LIMIT: usize = 32;
#[derive(Clone, Debug)]
pub struct Now {
    pub utc: i64,
    pub boot: i64,
    pub boot_id: String,
}
impl Now {
    pub fn read() -> Self {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) },
            0
        );
        Self {
            utc: Utc::now().timestamp_millis(),
            boot: ts.tv_sec as i64 * 1000 + ts.tv_nsec as i64 / 1_000_000,
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap_or_default()
                .trim()
                .into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Anchor {
    pub utc: i64,
    pub boot: i64,
    pub boot_id: String,
}
impl Anchor {
    fn new(now: &Now) -> Self {
        Self {
            utc: now.utc,
            boot: now.boot,
            boot_id: now.boot_id.clone(),
        }
    }
    fn elapsed(&self, now: &Now) -> i64 {
        if self.boot_id == now.boot_id {
            now.boot.saturating_sub(self.boot).max(0)
        } else {
            now.utc.saturating_sub(self.utc).max(0)
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timer {
    pub id: u64,
    pub label: String,
    pub total: i64,
    pub remaining: i64,
    pub started: Option<Anchor>,
    pub ringing: bool,
}
impl Timer {
    pub fn left(&self, now: &Now) -> i64 {
        (self.remaining - self.started.as_ref().map_or(0, |a| a.elapsed(now))).max(0)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Alarm {
    pub id: u64,
    pub hour: u32,
    pub minute: u32,
    pub days: u8,
    pub enabled: bool,
    pub label: String,
    pub next: Option<i64>,
    pub ringing: bool,
    pub snooze: Option<Anchor>,
    pub last_date: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stopwatch {
    pub elapsed: i64,
    pub started: Option<Anchor>,
    pub laps: Vec<i64>,
}
impl Stopwatch {
    pub fn elapsed(&self, now: &Now) -> i64 {
        self.elapsed
            .saturating_add(self.started.as_ref().map_or(0, |a| a.elapsed(now)))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub next_id: u64,
    pub alarms: Vec<Alarm>,
    pub timers: Vec<Timer>,
    pub stopwatch: Stopwatch,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            alarms: vec![],
            timers: vec![],
            stopwatch: Stopwatch::default(),
        }
    }
}

/// Local wall-clock schedule: skip nonexistent DST times, choose the first future
/// occurrence of an ambiguous time, and fire at most once per local date.
fn next_alarm<T: TimeZone>(alarm: &Alarm, after: DateTime<T>) -> Option<i64> {
    let time = NaiveTime::from_hms_opt(alarm.hour, alarm.minute, 0)?;
    let tz = after.timezone();
    for offset in 0..9 {
        let date = after
            .date_naive()
            .checked_add_days(chrono::Days::new(offset))?;
        if alarm.last_date.as_deref() == Some(date.to_string().as_str()) {
            continue;
        }
        if alarm.days != 0 && alarm.days & (1 << date.weekday().num_days_from_monday()) == 0 {
            continue;
        }
        let mut candidates = match tz.from_local_datetime(&date.and_time(time)) {
            LocalResult::Single(t) => vec![t],
            LocalResult::Ambiguous(a, b) => vec![a, b],
            LocalResult::None => vec![],
        };
        candidates.sort_by_key(|t| t.timestamp_millis());
        if let Some(t) = candidates
            .into_iter()
            .find(|t| t.timestamp_millis() > after.timestamp_millis())
        {
            return Some(t.timestamp_millis());
        }
    }
    None
}
impl State {
    fn id(&mut self) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or("ID space exhausted")?;
        Ok(id)
    }
    pub fn reconcile(&mut self, now: &Now) -> bool {
        let mut changed = false;
        for timer in &mut self.timers {
            if timer.started.is_some() && timer.left(now) == 0 {
                timer.remaining = 0;
                timer.started = None;
                timer.ringing = true;
                changed = true;
            }
        }
        let local = Local.timestamp_millis_opt(now.utc).single().unwrap();
        for alarm in &mut self.alarms {
            if alarm.ringing {
                continue;
            }
            if let Some(start) = &alarm.snooze {
                if start.elapsed(now) >= 5 * 60 * 1000 {
                    alarm.snooze = None;
                    alarm.ringing = true;
                    changed = true;
                }
                continue;
            }
            if !alarm.enabled {
                continue;
            }
            if alarm.next.is_some_and(|next| next <= now.utc) {
                // Delayed wake/restart still produces one alert; never a backlog.
                alarm.ringing = true;
                alarm.last_date = Some(local.date_naive().to_string());
                if alarm.days == 0 {
                    alarm.enabled = false;
                }
                alarm.next = None;
                changed = true;
            } else {
                let next = next_alarm(alarm, local);
                if next != alarm.next {
                    alarm.next = next;
                    changed = true;
                }
            }
        }
        changed
    }
    pub fn ringing(&self) -> bool {
        self.alarms.iter().any(|a| a.ringing) || self.timers.iter().any(|t| t.ringing)
    }
    pub fn next_wait(&self, now: &Now) -> i64 {
        let timer = self
            .timers
            .iter()
            .filter(|t| t.started.is_some())
            .map(|t| t.left(now));
        let alarm = self.alarms.iter().filter_map(|a| {
            if let Some(s) = &a.snooze {
                Some((300_000 - s.elapsed(now)).max(0))
            } else {
                a.next.map(|n| (n - now.utc).max(0))
            }
        });
        timer
            .chain(alarm)
            .min()
            .unwrap_or(60_000)
            .min(60_000)
            .max(1)
    }
    pub fn snapshot(&self, now: &Now) -> Value {
        json!({"alarms": self.alarms, "timers": self.timers.iter().map(|t| json!({"id":t.id,"label":t.label,"total":t.total,"remaining":t.left(now),"running":t.started.is_some(),"ringing":t.ringing})).collect::<Vec<_>>(),
            "stopwatch":{"elapsed":self.stopwatch.elapsed(now),"running":self.stopwatch.started.is_some(),"laps":self.stopwatch.laps},"ringing":self.ringing()})
    }
    pub fn command(&mut self, command: &Value, now: &Now) -> Result<(), String> {
        let op = command["op"].as_str().ok_or("Missing operation")?;
        let id = command["id"].as_u64().unwrap_or(0);
        match op {
            "snapshot" => return Ok(()),
            "timer-add" => {
                if self.timers.len() >= LIMIT {
                    return Err("At most 32 timers".into());
                }
                let seconds = command["seconds"]
                    .as_i64()
                    .filter(|s| (1..=604800).contains(s))
                    .ok_or("Timer must be 1 second to 7 days")?;
                let id = self.id()?;
                self.timers.push(Timer {
                    id,
                    label: label(command, "Timer")?,
                    total: seconds * 1000,
                    remaining: seconds * 1000,
                    started: Some(Anchor::new(now)),
                    ringing: false,
                });
            }
            "timer-pause" | "timer-resume" | "timer-delete" | "timer-dismiss" => {
                let t = self
                    .timers
                    .iter_mut()
                    .find(|t| t.id == id)
                    .ok_or("Unknown timer")?;
                match op {
                    "timer-pause" => {
                        t.remaining = t.left(now);
                        t.started = None;
                    }
                    "timer-resume" => {
                        if t.remaining == 0 || t.ringing {
                            return Err("Timer has finished".into());
                        }
                        if t.started.is_none() {
                            t.started = Some(Anchor::new(now));
                        }
                    }
                    _ => {
                        self.timers.retain(|t| t.id != id);
                    }
                }
            }
            "alarm-add" | "alarm-update" => {
                if op == "alarm-add" && self.alarms.len() >= LIMIT {
                    return Err("At most 32 alarms".into());
                }
                let hour = command["hour"]
                    .as_u64()
                    .filter(|h| *h < 24)
                    .ok_or("Invalid hour")? as u32;
                let minute = command["minute"]
                    .as_u64()
                    .filter(|m| *m < 60)
                    .ok_or("Invalid minute")? as u32;
                let days = command["days"]
                    .as_u64()
                    .filter(|d| *d < 128)
                    .ok_or("Invalid repeat days")? as u8;
                let id = if op == "alarm-add" { self.id()? } else { id };
                let alarm = Alarm {
                    id,
                    hour,
                    minute,
                    days,
                    enabled: true,
                    label: label(command, "Alarm")?,
                    next: None,
                    ringing: false,
                    snooze: None,
                    last_date: None,
                };
                if op == "alarm-update" {
                    *self
                        .alarms
                        .iter_mut()
                        .find(|a| a.id == id)
                        .ok_or("Unknown alarm")? = alarm;
                } else {
                    self.alarms.push(alarm);
                }
            }
            "alarm-toggle" | "alarm-delete" | "alarm-dismiss" | "alarm-snooze" => {
                let a = self
                    .alarms
                    .iter_mut()
                    .find(|a| a.id == id)
                    .ok_or("Unknown alarm")?;
                match op {
                    "alarm-toggle" => {
                        a.enabled = a.snooze.is_none() && !a.enabled;
                        a.ringing = false;
                        a.snooze = None;
                        a.next = None;
                        a.last_date = None;
                    }
                    "alarm-delete" => self.alarms.retain(|a| a.id != id),
                    "alarm-dismiss" => {
                        a.ringing = false;
                        a.snooze = None;
                    }
                    "alarm-snooze" => {
                        if !a.ringing {
                            return Err("Alarm is not ringing".into());
                        }
                        a.ringing = false;
                        a.snooze = Some(Anchor::new(now));
                    }
                    _ => unreachable!(),
                }
            }
            "stopwatch-start" => {
                if self.stopwatch.started.is_none() {
                    self.stopwatch.started = Some(Anchor::new(now));
                }
            }
            "stopwatch-pause" => {
                self.stopwatch.elapsed = self.stopwatch.elapsed(now);
                self.stopwatch.started = None;
            }
            "stopwatch-reset" => {
                if self.stopwatch.started.is_some() {
                    return Err("Pause before resetting".into());
                }
                self.stopwatch = Stopwatch::default();
            }
            "stopwatch-lap" => {
                if self.stopwatch.started.is_none() {
                    return Err("Start before recording a lap".into());
                }
                if self.stopwatch.laps.len() >= 100 {
                    return Err("At most 100 laps".into());
                }
                self.stopwatch.laps.push(self.stopwatch.elapsed(now));
            }
            _ => return Err("Unknown operation".into()),
        }
        self.reconcile(now);
        Ok(())
    }
}
fn label(command: &Value, fallback: &str) -> Result<String, String> {
    let text = command["label"].as_str().unwrap_or(fallback).trim();
    if text.chars().count() > 64 || text.chars().any(|c| c.is_control()) {
        return Err("Label must be at most 64 printable characters".into());
    }
    Ok(if text.is_empty() { fallback } else { text }.into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn now(ms: i64) -> Now {
        Now {
            utc: 1_800_000_000_000 + ms,
            boot: 1000 + ms,
            boot_id: "boot-a".into(),
        }
    }
    #[test]
    fn countdown_ignores_wall_clock_changes_and_survives_restart() {
        let mut s = State::default();
        s.command(&json!({"op":"timer-add","seconds":60}), &now(0))
            .unwrap();
        let s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        let mut n = now(20_000);
        n.utc += 86_400_000;
        assert_eq!(s.timers[0].left(&n), 40_000);
        n.boot_id = "reboot".into();
        n.utc = now(20_000).utc;
        assert_eq!(s.timers[0].left(&n), 40_000);
    }
    #[test]
    fn pause_resume_and_expiry_do_not_duplicate() {
        let mut s = State::default();
        s.command(&json!({"op":"timer-add","seconds":10}), &now(0))
            .unwrap();
        s.command(&json!({"op":"timer-pause","id":1}), &now(3000))
            .unwrap();
        assert_eq!(s.timers[0].left(&now(100000)), 7000);
        s.command(&json!({"op":"timer-resume","id":1}), &now(100000))
            .unwrap();
        assert!(s.reconcile(&now(107000)));
        assert!(!s.reconcile(&now(108000)));
        assert!(s.ringing());
        s.command(&json!({"op":"timer-dismiss","id":1}), &now(108000))
            .unwrap();
        assert!(!s.ringing());
    }
    #[test]
    fn stopwatch_laps_and_pause_include_suspend() {
        let mut s = State::default();
        s.command(&json!({"op":"stopwatch-start"}), &now(0))
            .unwrap();
        s.command(&json!({"op":"stopwatch-lap"}), &now(4000))
            .unwrap();
        s.command(&json!({"op":"stopwatch-pause"}), &now(6000))
            .unwrap();
        assert_eq!(s.stopwatch.elapsed(&now(100000)), 6000);
        assert_eq!(s.stopwatch.laps, [4000]);
    }
    #[test]
    fn repeating_alarm_fires_once_and_snooze_survives_restart() {
        let mut s = State::default();
        s.command(
            &json!({"op":"alarm-add","hour":7,"minute":30,"days":127}),
            &now(0),
        )
        .unwrap();
        let due = s.alarms[0].next.unwrap();
        let n = Now { utc: due, ..now(0) };
        assert!(s.reconcile(&n));
        assert!(s.ringing());
        s.command(&json!({"op":"alarm-snooze","id":1}), &n).unwrap();
        let mut s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        s.reconcile(&Now {
            boot: n.boot + 300000,
            utc: n.utc + 300000,
            ..n.clone()
        });
        assert!(s.ringing());
        s.command(&json!({"op":"alarm-dismiss","id":1}), &n)
            .unwrap();
        assert!(!s.ringing());
        assert!(s.alarms[0].next.unwrap() > due);
    }
    #[test]
    fn weekday_schedule_and_next_day_one_shot() {
        let a = Alarm {
            id: 1,
            hour: 7,
            minute: 30,
            days: 31,
            enabled: true,
            label: "".into(),
            next: None,
            ringing: false,
            snooze: None,
            last_date: None,
        };
        let friday = Utc.with_ymd_and_hms(2026, 10, 2, 8, 0, 0).unwrap();
        assert_eq!(
            next_alarm(&a, friday),
            Some(
                Utc.with_ymd_and_hms(2026, 10, 5, 7, 30, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        let mut a = a;
        a.days = 0;
        assert_eq!(
            next_alarm(&a, friday),
            Some(
                Utc.with_ymd_and_hms(2026, 10, 3, 7, 30, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }
}

#[cfg(test)]
#[test]
fn dst_schedules_skip_gaps_and_do_not_repeat_on_fall_back() {
    if std::env::var_os("HOKI_CLOCK_DST_CHILD").is_none() {
        let zone = [
            "/etc/zoneinfo/America/New_York",
            "/usr/share/zoneinfo/America/New_York",
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("tzdata required for DST tests");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "model::dst_schedules_skip_gaps_and_do_not_repeat_on_fall_back",
            ])
            .env("TZ", zone)
            .env("HOKI_CLOCK_DST_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let mut a = Alarm {
        id: 1,
        hour: 2,
        minute: 30,
        days: 127,
        enabled: true,
        label: "".into(),
        next: None,
        ringing: false,
        snooze: None,
        last_date: None,
    };
    let before = Local.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).unwrap();
    assert_eq!(
        next_alarm(&a, before),
        Some(
            Local
                .with_ymd_and_hms(2026, 3, 9, 2, 30, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    a.hour = 1;
    let before = Local.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    assert_eq!(
        next_alarm(&a, before),
        Some(
            Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    a.last_date = Some("2026-11-01".into());
    assert_eq!(
        next_alarm(&a, before),
        Some(
            Local
                .with_ymd_and_hms(2026, 11, 2, 1, 30, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
}
