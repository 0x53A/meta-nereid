//! Bounded suspend experiment. Kernel alarmtimer arbitrates shared RTC alarms.
use std::{fs, io, os::fd::{AsRawFd, FromRawFd, OwnedFd}, time::Duration};
fn now(clock: libc::clockid_t)->io::Result<f64> {
    let mut ts=libc::timespec{tv_sec:0,tv_nsec:0};
    if unsafe{libc::clock_gettime(clock,&mut ts)}!=0{return Err(io::Error::last_os_error())}
    Ok(ts.tv_sec as f64+ts.tv_nsec as f64/1e9)
}
fn run()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<_>=std::env::args().collect();
    let mode=args.get(1).map(String::as_str).unwrap_or("");
    if !matches!(mode,"awake"|"mem"|"cycle"){return Err("usage: hoki-suspend-check awake|mem|cycle seconds(3..60)".into())}
    let seconds:u32=args.get(2).ok_or("missing seconds")?.parse()?;
    if !(3..=60).contains(&seconds){return Err("seconds outside 3..60".into())}
    let raw=unsafe{libc::timerfd_create(libc::CLOCK_BOOTTIME_ALARM,libc::TFD_CLOEXEC|libc::TFD_NONBLOCK)};
    if raw<0{return Err(io::Error::last_os_error().into())}
    let alarm=unsafe{OwnedFd::from_raw_fd(raw)};
    let spec=libc::itimerspec{it_interval:libc::timespec{tv_sec:0,tv_nsec:0},it_value:libc::timespec{tv_sec:seconds as _,tv_nsec:0}};
    if unsafe{libc::timerfd_settime(alarm.as_raw_fd(),0,&spec,std::ptr::null_mut())}!=0{return Err(io::Error::last_os_error().into())}
    let mut got=libc::itimerspec{it_interval:libc::timespec{tv_sec:0,tv_nsec:0},it_value:libc::timespec{tv_sec:0,tv_nsec:0}};
    if unsafe{libc::timerfd_gettime(alarm.as_raw_fd(),&mut got)}!=0 || got.it_value.tv_sec<1{return Err("alarm readback failed".into())}
    println!("ALARM armed via CLOCK_BOOTTIME_ALARM: {}s; mode={mode}",got.it_value.tv_sec);
    let boot=now(libc::CLOCK_BOOTTIME)?; let mono=now(libc::CLOCK_MONOTONIC)?;
    if mode=="mem" || mode=="cycle" {
        // Save the kernel event generation before entry; reject races with wake events.
        let count=fs::read_to_string("/sys/power/wakeup_count")?;
        fs::write("/sys/power/wakeup_count",count.trim())?;
        // wakeup_count can block behind existing wake locks. Never enter sleep
        // with an expired fallback alarm after waiting for those locks.
        if unsafe{libc::timerfd_gettime(alarm.as_raw_fd(),&mut got)}!=0 || got.it_value.tv_sec<2 {
            return Err("fallback alarm expired or too close while waiting for wake locks".into())
        }
        println!("SUSPEND requesting mem");
        fs::write("/sys/power/state",b"mem")?;
    } else {
        let mut p=libc::pollfd{fd:alarm.as_raw_fd(),events:libc::POLLIN,revents:0};
        if unsafe{libc::poll(&mut p,1,((seconds+2)*1000) as i32)}!=1{return Err("alarm did not fire".into())}
    }
    let elapsed=now(libc::CLOCK_BOOTTIME)?-boot;
    let awake=now(libc::CLOCK_MONOTONIC)?-mono;
    let mut ticks=0u64;
    let n=unsafe{libc::read(alarm.as_raw_fd(),(&mut ticks as *mut u64).cast(),8)};
    println!("RETURN elapsed={elapsed:.3}s awake={awake:.3}s suspended_estimate={:.3}s alarm_expired={}",elapsed-awake,n==8 && ticks>0);
    if mode=="mem" && elapsed-awake<0.5{return Err("no meaningful suspend residency observed".into())}
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("SUSPEND CHECK: {e}");std::process::exit(1)}}
