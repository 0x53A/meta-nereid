//! Fixed Android Prima early-suspend preparation command, not system suspend.
use std::{ffi::c_char, io, os::fd::{AsRawFd, FromRawFd, OwnedFd}};
#[repr(C)]
struct PrivateCommand { buf: *mut c_char, used_len: i32, total_len: i32 }
fn run()->io::Result<()> {
    let arg=std::env::args().nth(1).unwrap_or_default();
    if arg=="state" || arg=="trace-levels" { return read_state(arg=="trace-levels"); }
    let value=match arg.as_str(){"quiet"=>b'1',"resume"=>b'0',_=>return Err(io::Error::new(io::ErrorKind::InvalidInput,"usage: hoki-wifi-quiet quiet|resume|state"))};
    // ABI verified in the built Prima hdd_priv_data_t and hdd_driver_ioctl.
    let mut command=*b"SETSUSPENDMODE 0\0";
    command[14]=b' '; command[15]=value;
    let mut data=PrivateCommand{buf:command.as_mut_ptr().cast(),used_len:0,total_len:command.len() as i32};
    let raw=unsafe{libc::socket(libc::AF_INET,libc::SOCK_DGRAM|libc::SOCK_CLOEXEC,0)};
    if raw<0{return Err(io::Error::last_os_error())}
    let socket=unsafe{OwnedFd::from_raw_fd(raw)};
    let mut req:libc::ifreq=unsafe{std::mem::zeroed()};
    for (dst,src) in req.ifr_name.iter_mut().zip(b"wlan0\0") {*dst=*src as c_char;}
    req.ifr_ifru.ifru_data=(&mut data as *mut PrivateCommand).cast();
    // SIOCDEVPRIVATE+1 is this driver's Android command entry.
    let result=unsafe{libc::ioctl(socket.as_raw_fd(),0x89f1 as libc::c_ulong,&mut req)};
    if result<0{return Err(io::Error::last_os_error())}
    println!("SETSUSPENDMODE {} accepted; underlying preparation is void, hardware state unverified",value as char);
    Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("WIFI QUIET: {e}");std::process::exit(1)}}

// WEXT fixed-size inline integer query, reviewed in iw_setnone_getint.
fn read_state(trace:bool)->io::Result<()> {
    #[repr(C)] struct WirelessRequest { name:[u8;16], data:[u8;16] }
    let raw=unsafe{libc::socket(libc::AF_INET,libc::SOCK_DGRAM|libc::SOCK_CLOEXEC,0)};
    if raw<0{return Err(io::Error::last_os_error())}
    let socket=unsafe{OwnedFd::from_raw_fd(raw)};
    let mut req=WirelessRequest{name:[0;16],data:[0;16]};
    req.name[..5].copy_from_slice(b"wlan0");
    req.data[..4].copy_from_slice(&(if trace {4i32} else {3i32}).to_ne_bytes()); // WE_PMC_STATE
    if unsafe{libc::ioctl(socket.as_raw_fd(),0x8be1 as libc::c_ulong,&mut req)}<0 {
        return Err(io::Error::last_os_error())
    }
    if trace { println!("TRACE_LEVELS dumped to kernel log"); return Ok(()); }
    let state=i32::from_ne_bytes(req.data[..4].try_into().unwrap());
    let names=["STOPPED","FULL_POWER","LOW_POWER","REQUEST_IMPS","IMPS","REQUEST_BMPS","BMPS","REQUEST_FULL_POWER","REQUEST_START_UAPSD","REQUEST_STOP_UAPSD","UAPSD","REQUEST_STANDBY","STANDBY","REQUEST_ENTER_WOWL","REQUEST_EXIT_WOWL","WOWL"];
    println!("PMC_STATE={state} {}",names.get(state as usize).unwrap_or(&"UNKNOWN"));
    Ok(())
}
