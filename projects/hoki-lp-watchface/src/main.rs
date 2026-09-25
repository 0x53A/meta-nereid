mod decode;

mod brightness;
mod ffi;
mod hwc;
mod render;
mod resources;
use decode::Reader;
use ffi::{Api, Handle, RawWriter};
use std::{ffi::{CString,c_void}, sync::atomic::{AtomicBool, Ordering}, time::{Duration,Instant}};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn signal_handler(_: i32) { STOP.store(true, Ordering::Relaxed); }
const V1: &str = "vendor.google_clockwork.sidekickgraphics@1.0::ISidekickGraphics";
struct Session<'a> { api: &'a Api, remote: *mut c_void, _sm: Handle<'a> }
impl<'a> Session<'a> {
    fn new(api: &'a Api) -> Result<Self,String> {
        let sm = Handle::new(api, unsafe {(api.gbinder_servicemanager_new)(c"/dev/hwbinder".as_ptr())}, api.gbinder_servicemanager_unref,"service manager")?;
        let names = unsafe {api.strings((api.gbinder_servicemanager_list_sync)(sm.ptr))}?;
        let name="vendor.google_clockwork.sidekickgraphics@1.2::ISidekickGraphics/default";
        if !names.iter().any(|s|s==name) {return Err("Sidekick v1.2 is not already registered".into())}
        let name=CString::new(name).unwrap(); let mut status=0;
        let remote=unsafe{(api.gbinder_servicemanager_get_service_sync)(sm.ptr,name.as_ptr(),&mut status)};
        if remote.is_null() || status!=0 {return Err(format!("lookup failed: {status}"))}
        Ok(Self{api,remote,_sm:sm})
    }
    fn capabilities(&self)->Result<decode::Capabilities,String> {
        let api=self.api;
        let iface=CString::new(V1).unwrap();
        let client=Handle::new(api,unsafe{(api.gbinder_client_new)(self.remote,iface.as_ptr())},api.gbinder_client_unref,"client")?;
        let req=Handle::new(api,unsafe{(api.gbinder_client_new_request)(client.ptr)},api.gbinder_local_request_unref,"request")?;
        println!("CALL getCapabilities (required vendor memory/color initialization)");
        let mut status=0;
        let raw=unsafe{(api.gbinder_client_transact_sync_reply)(client.ptr,2,req.ptr,&mut status)};
        let reply=Handle::new(api,raw,api.gbinder_remote_reply_unref,"capabilities reply");
        if status!=0 { return Err(format!("getCapabilities Binder status {status}")); }
        let reply=reply?;
        // This reply contains a Binder buffer, so the scalar-only call decoder
        // cannot decode it. Reuse the tested capability probe decoder.
        let capabilities=decode::capabilities(&mut api.reader(&reply))?;
        println!("CAPABILITIES {capabilities:?}");
        resources::validate_capabilities(&capabilities)?;
        Ok(capabilities)
    }
    fn call(&self, iface: &str, code:u32, name:&str, write:impl FnOnce(&Api,&mut RawWriter)) -> Result<Vec<u32>,String> {
        let api=self.api;let iface=CString::new(iface).unwrap();
        let client=Handle::new(api,unsafe{(api.gbinder_client_new)(self.remote,iface.as_ptr())},api.gbinder_client_unref,"client")?;
        let req=Handle::new(api,unsafe{(api.gbinder_client_new_request)(client.ptr)},api.gbinder_local_request_unref,"request")?;
        let mut writer=RawWriter{data:[std::ptr::null();4]};
        unsafe{(api.gbinder_local_request_init_writer)(req.ptr,&mut writer)};
        write(api,&mut writer);
        println!("CALL {name}");
        let mut status=0;let raw=unsafe{(api.gbinder_client_transact_sync_reply)(client.ptr,code,req.ptr,&mut status)};
        let reply=Handle::new(api,raw,api.gbinder_remote_reply_unref,"reply");
        if status!=0 {return Err(format!("{name} Binder status {status}"))}
        let reply=reply?;let mut r=api.reader(&reply);
        decode::check_status(r.word()?,"HIDL exception")?;
        // endDisplay returns Return<void>, unlike beginDisplay/reset.
        if code==12 {
            if !r.at_end(){return Err("unexpected endDisplay payload".into())}
            println!("RESULT {name}: HIDL success (no HAL result in this API)");
            return Ok(vec![])
        }
        let status=r.word()?;
        println!("RESULT {name}: HAL status {status}");
        decode::check_status(status,name)?;
        let mut words=vec![];
        while !r.at_end() {if words.len()==8{return Err("unexpected oversized reply".into())} words.push(r.word()?);}
        Ok(words)
    }
    fn simple(&self,code:u32,name:&str,value:Option<u32>)->Result<(),String>{
        let words=self.call(V1,code,name,|api,w| if let Some(v)=value {unsafe{(api.gbinder_writer_append_int32)(w,v)}})?;
        if !words.is_empty(){return Err(format!("unexpected trailing {name} reply"))} Ok(())
    }
    fn reset(&self)->Result<(),String>{self.simple(1,"reset",None)}
    fn release(&self)->Result<(),String>{
        // A failed/partial entry is recovered through the vendor reset API;
        // no firmware ioctl is used by this client.
        let end = self.simple(12,"endDisplay(ACTIVE)",Some(0));
        let reset = self.reset(); // Always attempt both, but never hide either failure.
        release_result(end, reset)
    }
    fn bitmap(&self)->Result<(),String>{
        let d=resources::drawable(14301,240,160,86.,126.,resources::DrawableKind::Bitmap);
        let png=include_bytes!("../red-bg.png");
        let words=self.call(V1,16,"sendBitmapPng8888(red BG)",|api,w|unsafe{
            (api.gbinder_writer_append_buffer_object)(w,d.as_ptr().cast(),d.len());
            (api.gbinder_writer_append_hidl_vec)(w,png.as_ptr().cast(),png.len() as u32,1);
        })?;
        if words.len()!=1{return Err(format!("expected one bitmap size/result, got {words:?}"))}
        println!("BITMAP accepted, vendor result {} (resource uses supplied ID 14301)",words[0]); Ok(())
    }
    fn clock_backing(&self)->Result<(),String>{
        let d=resources::drawable(14304,288,64,62.,174.,resources::DrawableKind::Bitmap);
        let png=include_bytes!("../clock-backing.png");
        let words=self.call(V1,16,"sendBitmapPng8888(opaque black clock backing, z0)",|api,w|unsafe{
            (api.gbinder_writer_append_buffer_object)(w,d.as_ptr().cast(),d.len());
            (api.gbinder_writer_append_hidl_vec)(w,png.as_ptr().cast(),png.len() as u32,1);
        })?;
        if words.len()!=1 {return Err(format!("unexpected backing reply {words:?}"))}
        println!("BACKING accepted: {words:?}; clock z1");
        Ok(())
    }
    fn clock(&self, show_seconds:bool, custom_font:bool, backing:bool)->Result<(),String>{
        if backing { self.clock_backing()?; }
        let font_id=14302u32;
        let mut font=[0u8;16];
        for (i,value) in [48u32,64,10,font_id].iter().enumerate(){font[i*4..i*4+4].copy_from_slice(&value.to_le_bytes());}
        let png=include_bytes!("../digits.png");
        let words=if custom_font {
            let (font,glyphs)=resources::custom_digit_font(font_id);
            self.call("vendor.google_clockwork.sidekickgraphics@1.2::ISidekickGraphics",27,"sendCustomFont(Unicode digits)",|api,w|unsafe{
                (api.gbinder_writer_append_buffer_object)(w,font.as_ptr().cast(),font.len());
                (api.gbinder_writer_append_hidl_vec)(w,glyphs.as_ptr().cast(),10,4);
                (api.gbinder_writer_append_hidl_vec)(w,std::ptr::null(),0,6);
                (api.gbinder_writer_append_hidl_vec)(w,png.as_ptr().cast(),png.len() as u32,1);
            })?
        } else {
            self.call(V1,17,"sendFontPng8888(10 digits)",|api,w|unsafe{
                (api.gbinder_writer_append_buffer_object)(w,font.as_ptr().cast(),font.len());
                (api.gbinder_writer_append_hidl_vec)(w,png.as_ptr().cast(),png.len() as u32,1);
            })?
        };
        println!("FONT accepted, vendor result {words:?}, supplied ID {font_id}");
        let (width,x,pattern)=(192,110.,"HHmm");
        let _ = show_seconds;
        let mut d=resources::drawable(14303,width,64,x,174.,resources::DrawableKind::DateTime);
        if backing { d[88..92].copy_from_slice(&1u32.to_le_bytes()); }
        let format:Vec<u16>=pattern.encode_utf16().collect();
        let mut time=[0u8;48];
        // v1.2 sendDateTimeResource consumes base day/ms offsets at 0/4,
        // font ID at 8, foreground/background colors at 12/16, UTF16 vec at
        // 24 and trailing format option at 40. Zero offsets use native time.
        for (off,value) in [(8,font_id),(12,0xffe0e0e0u32),(16,0xff000000u32)] {time[off..off+4].copy_from_slice(&value.to_le_bytes());}
        time[24..32].copy_from_slice(&(format.as_ptr() as u64).to_le_bytes());
        time[32..36].copy_from_slice(&(format.len() as u32).to_le_bytes());
        let words=self.call("vendor.google_clockwork.sidekickgraphics@1.2::ISidekickGraphics",30,"sendDateTimeResource",|api,w|unsafe{
            (api.gbinder_writer_append_buffer_object)(w,d.as_ptr().cast(),d.len());
            let index=(api.gbinder_writer_append_buffer_object)(w,time.as_ptr().cast(),time.len());
            let parent=ffi::Parent{index,offset:24};
            (api.gbinder_writer_append_buffer_object_with_parent)(w,format.as_ptr().cast(),format.len()*2,&parent);
        })?;
        println!("CLOCK resource accepted: format={pattern}, vendor result {words:?}");
        Ok(())
    }

}
impl brightness::Transport for Session<'_> {
    fn levels(&self, bright:u16, dim:u16)->Result<(),String> {
        let bright=[bright]; let dim=[dim];
        let words=self.call(V1,4,"setBrightness(manual 127/64)",|api,w|unsafe{
            // HIDL scalar bool occupies a padded four-byte parcel slot.
            (api.gbinder_writer_append_int32)(w,1);
            (api.gbinder_writer_append_hidl_vec)(w,std::ptr::null(),0,2);
            (api.gbinder_writer_append_hidl_vec)(w,std::ptr::null(),0,2);
            (api.gbinder_writer_append_hidl_vec)(w,bright.as_ptr().cast(),1,2);
            (api.gbinder_writer_append_hidl_vec)(w,dim.as_ptr().cast(),1,2);
        })?;
        if !words.is_empty(){return Err("unexpected brightness reply".into())} Ok(())
    }
    fn als_off(&self, alpha:f32)->Result<(),String> {
        let words=self.call(V1,3,"setAlsMode(OFF,80,80)",|api,w|unsafe{
            (api.gbinder_writer_append_int32)(w,0);
            // Parcel floats are their IEEE754 bits in a four-byte aligned slot.
            (api.gbinder_writer_append_int32)(w,alpha.to_bits());
            (api.gbinder_writer_append_int32)(w,alpha.to_bits());
        })?;
        if !words.is_empty(){return Err("unexpected ALS reply".into())} Ok(())
    }
}
fn display_mode(mode:&str)->bool {
    matches!(mode,"display"|"display-no-reset"|"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit")
}
fn log_brightness(session:&Session<'_>,phase:&str) {
    // Diagnostic only: these raw readings do not establish pixel visibility.
    println!("BG BRIGHTNESS {phase}: {:?}",session.call(V1,5,"getLastBrightness",|_,_|{}));
}
fn run(mode:&str,seconds:u64)->Result<(),String>{
    if display_mode(mode) && (unsafe { libc::geteuid() } != 1000
        || std::env::var("XDG_RUNTIME_DIR").as_deref() != Ok("/run/user/1000")) {
        return Err("display mode requires ceres UID 1000 and /run/user/1000".into());
    }
    let api=Api::load()?;let session=Session::new(&api)?;
    if mode=="release" {return session.release()}
    if mode=="brightness" {let values=session.call(V1,5,"getLastBrightness",|_,_|{})?;println!("BRIGHTNESS {values:?}");return Ok(())}
    // Must run in this client, even after reboot/HAL restart. The vendor query
    // initializes its resource memory limit and encoder color configuration.
    session.capabilities()?;
    if mode=="preflight" { return Ok(()); }

    session.reset()?;
    if matches!(mode,"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit"|"configure-brightness") {
        brightness::configure(&session)?;
        log_brightness(&session,"after configuration");
        if mode=="configure-brightness" { return Ok(()); }
    }
    session.simple(7,"beginResources",None)?;
    if let Err(e)=(if matches!(mode,"clock-upload"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") {session.clock(mode!="clock-upload",matches!(mode,"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit"),mode=="clock-backed-lit")}else{session.bitmap()}).and_then(|_|session.simple(8,"endResources",None)) {
        let cleanup=session.reset();return Err(format!("upload: {e}; cleanup={cleanup:?}"))
    }
    if mode=="upload" || mode=="clock-upload" {return session.reset()}
    // Only run this mode after the orchestration service stops both normal
    // display owners. HWC is created here, after upload succeeds.
    // Process-scoped graphics lifetime on EVERY exit path. Teardown of this old
    // libhybris stack aborted in the first trial; external recovery owns restart.
    let mut hwc=std::mem::ManuallyDrop::new(hwc::HwcBackend::new().map_err(|e|format!("HWC init: {e:#}"))?);
    let mut renderer=std::mem::ManuallyDrop::new(render::Renderer::new(&hwc).map_err(|e|format!("EGL init: {e:#}"))?);
    frame(&mut hwc, &mut renderer, [0.,0.,0.])?;
    println!("PHASE PREPARE: black main-processor frame");
    if STOP.load(Ordering::Relaxed) { return session.release(); }
    frame(&mut hwc, &mut renderer, [0.,0.,0.])?;
    // Drain the CURRENT black frame before handing over the panel.
    // HWC2 DOZE_SUSPEND=3; no main CPU suspend is requested.
    let entry = hwc.set_power_mode(3).map_err(|e|e.to_string())
        .and_then(|_| {
            if STOP.load(Ordering::Relaxed) { return Err("cancelled before entry".into()); }
            session.simple(11,"beginDisplay(AMBIENT)",Some(1))
        });
    if entry.is_ok(){
        println!("PHASE SIDEKICK: entry accepted; {}. Visibility UNVERIFIED; no main-processor frame submissions or updateDisplayTime calls", if matches!(mode,"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") {"native HHmm pale clock"} else {"red BG bitmap"});
        if matches!(mode,"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") { log_brightness(&session,"ambient entry"); }
        std::fs::write("/run/user/1000/hoki-lp-ready", b"ready\n").map_err(|e|e.to_string())?;
        pause(seconds);
        let _=std::fs::remove_file("/run/user/1000/hoki-lp-ready");
        if matches!(mode,"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") { log_brightness(&session,"ambient end"); }
    }
    let release = if reset_after_display(mode, entry.is_ok()) {
        session.release()
    } else {
        println!("NORMAL EXIT: endDisplay only; no additional reset; vendor internal exit result is unavailable");
        session.simple(12,"endDisplay(ACTIVE)",Some(0))
    };
    release.map_err(|e|format!("release failed: {e}; entry={entry:?}"))?;
    if matches!(mode,"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") { log_brightness(&session,"after exit"); }
    hwc.set_power_mode(2).map_err(|e|e.to_string())?;
    frame(&mut hwc, &mut renderer, [0.,0.,0.])?;
    println!("PHASE RELEASED: restoring normal UI externally");
    println!("API SEQUENCE COMPLETE; external coordinator restores normal UI; user must verify visible recovery");
    entry
}
fn pause(seconds:u64) {
    let deadline=Instant::now()+Duration::from_secs(seconds);
    while !STOP.load(Ordering::Relaxed) {
        let left=deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {break;}
        unsafe {libc::poll(std::ptr::null_mut(),0,left.as_millis().min(i32::MAX as u128) as i32);}
    }
}

fn frame(hwc:&mut hwc::HwcBackend, renderer:&mut render::Renderer, rgb:[f32;3])->Result<(),String> {
    renderer.clear(rgb[0],rgb[1],rgb[2],1.);
    renderer.swap_buffers().map_err(|e|e.to_string())?;
    hwc.drain_frame().map_err(|e|e.to_string())
}
fn reset_after_display(mode: &str, entry_succeeded: bool) -> bool {
    !matches!(mode,"display-no-reset"|"display-lit"|"clock-display-lit"|"clock-custom-lit"|"clock-backed-lit"|"audit-display-lit") || !entry_succeeded
}
fn release_result(end:Result<(),String>, reset:Result<(),String>)->Result<(),String> {
    match (end,reset) {
        (Ok(()),Ok(())) => Ok(()),
        (end,reset) => Err(format!("endDisplay={end:?}; reset={reset:?}; ownership unverified")),
    }
}
#[cfg(test)] mod recovery_tests {
    use super::*;
    #[test] fn no_reset_trial_keeps_failed_entry_recovery() {
        assert!(!reset_after_display("display-no-reset", true));
        assert!(reset_after_display("display-no-reset", false));
        assert!(reset_after_display("display", true));
        assert!(reset_after_display("display", false));
    }
    #[test] fn reset_success_does_not_hide_end_failure() {
        assert!(release_result(Err("binder failed".into()),Ok(())).is_err());
        assert!(release_result(Ok(()),Err("reset failed".into())).is_err());
        assert!(release_result(Err("end failed".into()),Err("reset failed".into())).is_err());
        assert!(release_result(Ok(()),Ok(())).is_ok());
    }
}

fn main(){
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args:Vec<_>=std::env::args().collect();
    let mode=match args.get(1).map(String::as_str) {
        Some("face")=>"clock-backed-lit", Some("release")=>"release", Some("preflight")=>"preflight",
        _=>{eprintln!("usage: hoki-lp-watchface face|release|preflight [seconds:1..180]");std::process::exit(2)}
    };
    let seconds=args.get(2).map(|s|s.parse::<u64>()).unwrap_or(Ok(15)).unwrap_or(0);
    if !(1..=180).contains(&seconds){std::process::exit(2)}
    unsafe{libc::signal(libc::SIGTERM,signal_handler as *const () as usize);libc::signal(libc::SIGINT,signal_handler as *const () as usize);}
    std::thread::spawn(move||{std::thread::sleep(Duration::from_secs(seconds+45));unsafe{libc::_exit(124)}});
    if let Err(e)=run(mode,seconds){eprintln!("AMBIENT TEST FAILED: {e}");std::process::exit(1)}
}
