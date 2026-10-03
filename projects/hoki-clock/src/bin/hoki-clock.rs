// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
use serde_json::{Value, json};
use slint::{ComponentHandle, Model};
use std::{sync::mpsc, time::Duration};
slint::include_modules!();
fn entries(snapshot: &Value, key: &str) -> Vec<ClockEntry> {
    snapshot[key]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let alarm = key == "alarms";
            let ringing = v["ringing"] == true;
            let running = if alarm {
                v["enabled"] == true
            } else {
                v["running"] == true
            };
            let title = if alarm {
                format!(
                    "{:02}:{:02}",
                    v["hour"].as_u64().unwrap_or(0),
                    v["minute"].as_u64().unwrap_or(0)
                )
            } else {
                hoki_clock::duration((v["remaining"].as_i64().unwrap_or(0) + 999).max(0))
            };
            let detail = if ringing {
                "Time’s up".into()
            } else if alarm {
                if !v["snooze"].is_null() {
                    "Snoozed · 5 minutes".into()
                } else {
                    format!(
                        "{} · {}",
                        match v["days"].as_u64().unwrap_or(0) {
                            0 => "Once",
                            31 => "Weekdays",
                            127 => "Every day",
                            _ => "Custom days",
                        },
                        if running { "On" } else { "Off" }
                    )
                }
            } else {
                format!(
                    "{} · {}",
                    v["label"].as_str().unwrap_or("Timer"),
                    if running { "Running" } else { "Paused" }
                )
            };
            ClockEntry {
                id: v["id"].to_string().into(),
                title: title.into(),
                detail: detail.into(),
                running,
                ringing,
                hour: v["hour"].as_i64().unwrap_or(0) as i32,
                minute: v["minute"].as_i64().unwrap_or(0) as i32,
                repeat: match v["days"].as_u64().unwrap_or(0) {
                    31 => 1,
                    127 => 2,
                    _ => 0,
                },
            }
        })
        .collect()
}
fn apply(w: &MainWindow, s: &Value) {
    let a = entries(s, "alarms");
    let t = entries(s, "timers");
    w.set_alarm_index(w.get_alarm_index().min(a.len().saturating_sub(1) as i32));
    w.set_timer_index(w.get_timer_index().min(t.len().saturating_sub(1) as i32));
    w.set_alarms(std::rc::Rc::new(slint::VecModel::from(a)).into());
    w.set_timers(std::rc::Rc::new(slint::VecModel::from(t)).into());
    let ms = s["stopwatch"]["elapsed"].as_i64().unwrap_or(0);
    w.set_elapsed(format!("{}.{:01}", hoki_clock::duration(ms), ms / 100 % 10).into());
    w.set_stopwatch_running(s["stopwatch"]["running"] == true);
    let laps = s["stopwatch"]["laps"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .rev()
        .take(3)
        .map(|(i, v)| {
            format!(
                "Lap {}     {}",
                i + 1,
                hoki_clock::duration(v.as_i64().unwrap_or(0))
            )
            .into()
        })
        .collect::<Vec<slint::SharedString>>();
    w.set_laps(std::rc::Rc::new(slint::VecModel::from(laps)).into());
    w.set_connected(true);
    if let Some(error) = s["delivery_error"].as_str() {
        w.set_error(error.into());
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--command") {
        println!(
            "{}",
            hoki_clock::request(args.get(2).ok_or("expected JSON")?)
                .map_err(std::io::Error::other)?
        );
        return Ok(());
    }
    if let Some(i) = args.iter().position(|s| s == "--preview") {
        return preview(
            args.get(i + 1).ok_or("expected PNG path")?,
            args.get(i + 2).map(String::as_str).unwrap_or("timer"),
        );
    }
    let w = MainWindow::new()?;
    let (tx, rx) = mpsc::sync_channel::<Value>(4);
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let cmd = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => json!({"op":"snapshot"}),
                Err(_) => break,
            };
            let result = hoki_clock::request(&cmd.to_string())
                .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()));
            if result_tx.send((cmd["op"] != "snapshot", result)).is_err() {
                break;
            }
        }
    });
    let weak = w.as_weak();
    w.on_action(move |action| {let Some(w)=weak.upgrade() else{return};
        if action=="new" {w.set_editing_id("".into());w.set_hours(if w.get_page()==0{7}else{0});w.set_minutes(if w.get_page()==0{0}else{5});w.set_seconds(0);w.set_repeat(0);w.set_editing(true);return;}
        let alarm=w.get_page()==0;
        let entry=if alarm {w.get_alarms().row_data(w.get_alarm_index() as usize)}else{w.get_timers().row_data(w.get_timer_index() as usize)};
        if action=="edit" {if let Some(e)=entry {w.set_hours(e.hour);w.set_minutes(e.minute);w.set_repeat(e.repeat);w.set_editing_id(e.id);w.set_editing(true);}return;}
        let mut cmd=match action.as_str() {
            "save" if alarm=>json!({"op":if w.get_editing_id().is_empty(){"alarm-add"}else{"alarm-update"},"hour":w.get_hours(),"minute":w.get_minutes(),"days":match w.get_repeat(){1=>31,2=>127,_=>0}}),
            "save"=>json!({"op":"timer-add","seconds":w.get_hours()*3600+w.get_minutes()*60+w.get_seconds()}),
            "toggle"=>json!({"op":if alarm{"alarm-toggle"}else if entry.as_ref().is_some_and(|e|e.running){"timer-pause"}else{"timer-resume"}}),
            "dismiss"=>json!({"op":if alarm{"alarm-dismiss"}else{"timer-dismiss"}}),
            "delete"=>json!({"op":if alarm{"alarm-delete"}else{"timer-delete"}}),
            "snooze"=>json!({"op":"alarm-snooze"}),
            action=>json!({"op":action}),
        };
        if action=="save" && alarm {cmd["id"]=json!(w.get_editing_id().parse::<u64>().unwrap_or(0));}
        else if let Some(e)=entry {cmd["id"]=json!(e.id.parse::<u64>().unwrap_or(0));}
        if tx.try_send(cmd).is_ok(){w.set_busy(true);}else{w.set_error("Clock is busy. Try again.".into());}
    });
    let weak = w.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(w) = weak.upgrade() else { return };
            for (command, result) in result_rx.try_iter() {
                match result {
                    Ok(s) => {
                        apply(&w, &s);
                        if command {
                            w.set_editing(false);
                            w.set_error("".into());
                        }
                    }
                    Err(e) => {
                        if command {
                            w.set_error(e.into());
                        } else {
                            w.set_connected(false);
                        }
                    }
                }
                if command {
                    w.set_busy(false);
                }
            }
        },
    );
    w.on_close_app(|| {
        let _ = slint::quit_event_loop();
    });
    w.run()?;
    Ok(())
}
fn preview(path: &str, scene: &str) -> Result<(), Box<dyn std::error::Error>> {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    struct Platform(std::rc::Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Platform {
        fn create_window_adapter(
            &self,
        ) -> Result<std::rc::Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    let r = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Platform(r.clone())))?;
    let w = MainWindow::new()?;
    w.show()?;
    r.set_size(slint::PhysicalSize::new(416, 416));
    apply(
        &w,
        &json!({"alarms":[{"id":1,"hour":7,"minute":30,"days":31,"enabled":true}],"timers":[{"id":2,"label":"Timer","total":300000,"remaining":182000,"running":true}],"stopwatch":{"elapsed":83200,"running":true,"laps":[20000,42100,67900]}}),
    );
    w.set_page(match scene {
        "alarm" => 0,
        "stopwatch" => 2,
        _ => 1,
    });
    if scene == "edit" {
        w.set_editing(true);
        w.set_hours(0);
        w.set_minutes(5);
    }
    let mut pixels = vec![slint::Rgb8Pixel::default(); 416 * 416];
    r.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 416);
    });
    let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    image::save_buffer(path, &bytes, 416, 416, image::ColorType::Rgb8)?;
    Ok(())
}
