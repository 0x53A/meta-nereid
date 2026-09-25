//! Bounded display-off owner. Stop compositor and HWC proxy before running;
//! independent systemd cleanup must restart both afterwards.
#[allow(dead_code)]
#[path = "../hwc.rs"] mod hwc;
#[allow(dead_code)]
#[path = "../render.rs"] mod render;
use std::{sync::atomic::{AtomicBool,Ordering},time::{Duration,Instant}};
static STOP:AtomicBool=AtomicBool::new(false);
extern "C" fn stop(_:i32){STOP.store(true,Ordering::Relaxed);}
fn run()->anyhow::Result<()> {
 anyhow::ensure!(unsafe{libc::geteuid()}==1000 && std::env::var("XDG_RUNTIME_DIR").as_deref()==Ok("/run/user/1000"),"requires ceres runtime");
 let seconds=std::env::args().nth(1).ok_or_else(||anyhow::anyhow!("missing duration"))?.parse::<u64>()?;
 anyhow::ensure!((1..=86400).contains(&seconds),"duration outside 1..86400");
 unsafe{libc::signal(libc::SIGTERM,stop as *const () as usize);libc::signal(libc::SIGINT,stop as *const () as usize);}
 // Existing HWC/EGL teardown is unsafe on this vendor stack. Process exit
 // releases resources; independent cleanup restores the display on errors.
 let mut hwc=std::mem::ManuallyDrop::new(hwc::HwcBackend::new()?);
 let mut renderer=std::mem::ManuallyDrop::new(render::Renderer::new(&hwc)?);
 renderer.clear(0.,0.,0.,1.);
 renderer.swap_buffers()?;
 hwc.drain_frame()?;
 hwc.set_power_mode(hwc::HWC2_POWER_MODE_OFF)?;
 std::fs::write("/run/user/1000/hoki-screen-off-ready",b"ready\n")?;
 println!("DISPLAY_OFF accepted");
 let start=Instant::now();
 while !STOP.load(Ordering::Relaxed) && start.elapsed()<Duration::from_secs(seconds){
  unsafe{libc::poll(std::ptr::null_mut(),0,1000);}
 }
 let _=std::fs::remove_file("/run/user/1000/hoki-screen-off-ready");
 hwc.set_power_mode(hwc::HWC2_POWER_MODE_ON)?;
 Ok(())
}
fn main(){tracing_subscriber::fmt().with_env_filter("info").init();if let Err(e)=run(){eprintln!("SCREEN OFF: {e:#}");std::process::exit(1)}}
