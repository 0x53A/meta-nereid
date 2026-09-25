//! Read-only Sensors 1.0 descriptor query. No activate, batch, poll, or flush.
#[allow(dead_code)]
#[path = "../ffi.rs"] mod ffi;
#[allow(dead_code)]
#[path = "../decode.rs"] mod decode;
use std::{ffi::c_void, ptr};
use ffi::{Api,Handle,RawReader};
#[repr(C)] struct Buffer { data:*const u8,size:usize }
fn run()->Result<(),String>{
 let api=Api::load()?;
 let lib=unsafe{libloading::Library::new("libgbinder.so.1")}.map_err(|e|e.to_string())?;
 let vec:unsafe extern "C" fn(*mut RawReader,*mut usize,*mut usize)->*const u8=unsafe{*lib.get(b"gbinder_reader_read_hidl_vec\0").map_err(|e|e.to_string())?};
 let buffer:unsafe extern "C" fn(*mut RawReader)->*mut Buffer=unsafe{*lib.get(b"gbinder_reader_read_buffer\0").map_err(|e|e.to_string())?};
 let free:unsafe extern "C" fn(*mut Buffer)=unsafe{*lib.get(b"gbinder_buffer_free\0").map_err(|e|e.to_string())?};
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
 println!("handle\ttype\tname\tvendor\tmin_delay_us\tmax_delay_us\tfifo_reserved\tfifo_max\tflags\tpower_ma\tmax_range\tresolution");
 for i in 0..count{
  let bytes=unsafe{std::slice::from_raw_parts(data.add(i*size),size)};
  let mut strings=Vec::new();
  for _ in 0..4{
   let b=unsafe{buffer(&mut reader)};if b.is_null(){return Err("missing string buffer".into())}
   let (p,n)=unsafe{((*b).data,(*b).size)};
   if p.is_null()||n>4096{unsafe{free(b)};return Err("invalid string buffer".into())}
   let text=unsafe{std::slice::from_raw_parts(p,n)};let end=text.iter().position(|c|*c==0).unwrap_or(n);
   strings.push(String::from_utf8_lossy(&text[..end]).replace(['\t','\n','\r']," "));unsafe{free(b)};
  }
  // sensor_t layout from sensorfw/core/hybrisbindertypes.h:112B, HIDL strings16B.
  let u=|o:usize|u32::from_ne_bytes(bytes[o..o+4].try_into().unwrap());
  let f=|o|f32::from_bits(u(o));
  println!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",u(0) as i32,u(44) as i32,strings[0],strings[1],u(76) as i32,u(104) as i32,u(80),u(84),u(108),f(72),f(64),f(68));
 }
 Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("SENSOR INVENTORY: {e}");std::process::exit(1)}}
