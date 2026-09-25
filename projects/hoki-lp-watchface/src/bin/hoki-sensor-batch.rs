//! Bounded accelerometer batching experiment; sensorfwd must be stopped externally.
#[allow(dead_code)]
#[path = "../ffi.rs"] mod ffi;
#[allow(dead_code)]
#[path = "../decode.rs"] mod decode;
use std::{ffi::c_void, ptr};
use ffi::{Api,Handle,RawReader};
#[repr(C)] struct Buffer { data:*const u8,size:usize }
fn run()->Result<(),String>{
 let mode=std::env::args().nth(1).unwrap_or_default();
 if !["immediate","batch","off"].contains(&mode.as_str()){return Err("usage: hoki-sensor-batch immediate|batch|off".into())}
 let api=Api::load()?;
 let lib=unsafe{libloading::Library::new("libgbinder.so.1")}.map_err(|e|e.to_string())?;
 let vec:unsafe extern "C" fn(*mut RawReader,*mut usize,*mut usize)->*const u8=unsafe{*lib.get(b"gbinder_reader_read_hidl_vec\0").map_err(|e|e.to_string())?};
 let remote_unref:unsafe extern "C" fn(*mut c_void)=unsafe{*lib.get(b"gbinder_remote_object_unref\0").map_err(|e|e.to_string())?};
 let remote_ref:unsafe extern "C" fn(*mut c_void)->*mut c_void=unsafe{*lib.get(b"gbinder_remote_object_ref\0").map_err(|e|e.to_string())?};
 let sm=Handle::new(&api,unsafe{(api.gbinder_servicemanager_new)(c"/dev/hwbinder".as_ptr())},api.gbinder_servicemanager_unref,"manager")?;
 let mut status=0;
 let remote=Handle::new(&api,unsafe{remote_ref((api.gbinder_servicemanager_get_service_sync)(sm.ptr,c"android.hardware.sensors@1.0::ISensors/default".as_ptr(),&mut status))},remote_unref,"sensor service")?;
 if status!=0{return Err(format!("service status {status}"))}
 let client=Handle::new(&api,unsafe{(api.gbinder_client_new)(remote.ptr,c"android.hardware.sensors@1.0::ISensors".as_ptr())},api.gbinder_client_unref,"client")?;
 let reply=Handle::new(&api,unsafe{(api.gbinder_client_transact_sync_reply)(client.ptr,1,ptr::null_mut(),&mut status)},api.gbinder_remote_reply_unref,"reply")?;
 if status!=0{return Err(format!("transaction status {status}"))}
 let mut reader:RawReader=unsafe{std::mem::zeroed()};unsafe{(api.gbinder_remote_reply_init_reader)(reply.ptr,&mut reader)};
 let mut result=0;if unsafe{(api.gbinder_reader_read_uint32)(&mut reader,&mut result)}==0||result!=0{return Err(format!("reply status {result}"))}
 let(mut count,mut size)=(0,0);let data=unsafe{vec(&mut reader,&mut count,&mut size)};
 if data.is_null()||size!=112||count>256{return Err(format!("invalid sensor vector count={count} size={size}"))}

 // Discover non-wakeup accelerometer by type/flag; do not assume stable handles.
 let mut selected=None;
 for i in 0..count {
  let b=unsafe{std::slice::from_raw_parts(data.add(i*size),size)};
  let u=|o:usize|u32::from_ne_bytes(b[o..o+4].try_into().unwrap());
  if u(44)==1 && u(108)&1==0 {selected=Some(u(0));}
 }
 let sensor=selected.ok_or("no non-wakeup accelerometer")?;
 let write64:unsafe extern "C" fn(*mut ffi::RawWriter,i64)=unsafe{*lib.get(b"gbinder_writer_append_int64\0").map_err(|e|e.to_string())?};
 let call=|code:u32,fill:&dyn Fn(&mut ffi::RawWriter)|->Result<Handle<'_>,String>{
  let req=Handle::new(&api,unsafe{(api.gbinder_client_new_request)(client.ptr)},api.gbinder_local_request_unref,"request")?;
  let mut w:ffi::RawWriter=unsafe{std::mem::zeroed()};unsafe{(api.gbinder_local_request_init_writer)(req.ptr,&mut w)};fill(&mut w);
  let mut st=0;let rep=Handle::new(&api,unsafe{(api.gbinder_client_transact_sync_reply)(client.ptr,code,req.ptr,&mut st)},api.gbinder_remote_reply_unref,"control reply")?;
  if st!=0{return Err(format!("transport {st}"))}Ok(rep)
 };
 let check=|r:&Handle<'_>|->Result<(),String>{
  let mut reader:RawReader=unsafe{std::mem::zeroed()};unsafe{(api.gbinder_remote_reply_init_reader)(r.ptr,&mut reader)};
  for _ in 0..2 {let mut v=0;if unsafe{(api.gbinder_reader_read_uint32)(&mut reader,&mut v)}==0||v!=0{return Err(format!("HAL result {}",v as i32))}}Ok(())
 };
 let activate=|on:u32|->Result<(),String>{let r=call(3,&|w|unsafe{(api.gbinder_writer_append_int32)(w,sensor);(api.gbinder_writer_append_int32)(w,on)})?;check(&r)};
 if mode=="off"{activate(0)?;println!("DEACTIVATED handle={sensor}");return Ok(())}
 let latency=if mode=="batch"{2_000_000_000}else{0};
 let r=call(5,&|w|unsafe{(api.gbinder_writer_append_int32)(w,sensor);write64(w,40_000_000);write64(w,latency)})?;check(&r)?;
 activate(1)?;
 let experiment=(||->Result<(),String>{
  println!("START handle={sensor} sample_ns=40000000 latency_ns={latency}");
  let start=std::time::Instant::now();let mut total=0;let mut flushing=false;let mut complete=false;
  for _ in 0..500{
   let rep=call(4,&|w|unsafe{(api.gbinder_writer_append_int32)(w,128)})?;
   let mut reader:RawReader=unsafe{std::mem::zeroed()};unsafe{(api.gbinder_remote_reply_init_reader)(rep.ptr,&mut reader)};
   for _ in 0..2{let mut v=0;if unsafe{(api.gbinder_reader_read_uint32)(&mut reader,&mut v)}==0||v!=0{return Err(format!("poll status {v}"))}}
   let(mut n,mut size)=(0,0);let p=unsafe{vec(&mut reader,&mut n,&mut size)};
   if size!=80||n>128||(p.is_null()&&n!=0){return Err(format!("bad events {n}x{size}"))}
   let arrival=start.elapsed().as_secs_f64();println!("BATCH arrival_s={arrival:.6} events={n}");
   for i in 0..n {
    let b=unsafe{std::slice::from_raw_parts(p.add(i*size),size)};let u=|o:usize|u32::from_ne_bytes(b[o..o+4].try_into().unwrap());
    if u(8)!=sensor{continue}let timestamp=i64::from_ne_bytes(b[..8].try_into().unwrap());let typ=u(12);
    if typ==1 {total+=1;println!("EVENT {arrival:.6} {timestamp} {} {} {}",f32::from_bits(u(16)),f32::from_bits(u(20)),f32::from_bits(u(24)));}
    if typ==0 && u(16)==1 {println!("FLUSH_COMPLETE handle={sensor}");complete=true;}
   }
   if complete{break}
   if total>=100 && !flushing{let r=call(6,&|w|unsafe{(api.gbinder_writer_append_int32)(w,sensor)})?;check(&r)?;flushing=true;}
   if start.elapsed().as_secs()>10{return Err("experiment deadline exceeded".into())}
  }
  if !complete{return Err("no flush completion".into())}println!("DONE samples={total}");Ok(())
 })();
 let cleanup=activate(0);experiment.and(cleanup)
}
fn main(){if let Err(e)=run(){eprintln!("SENSOR BATCH: {e}");std::process::exit(1)}}
