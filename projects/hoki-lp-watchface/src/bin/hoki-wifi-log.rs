//! Bounded receive-only listener for Prima's NETLINK_USERSOCK multicast logs.
use std::{io,os::fd::{AsRawFd,FromRawFd,OwnedFd},time::{Duration,Instant}};
fn run()->io::Result<()> {
    let fd=unsafe{libc::socket(libc::AF_NETLINK,libc::SOCK_RAW|libc::SOCK_CLOEXEC,libc::NETLINK_USERSOCK)};
    if fd<0{return Err(io::Error::last_os_error())}
    let socket=unsafe{OwnedFd::from_raw_fd(fd)};
    let mut addr:libc::sockaddr_nl=unsafe{std::mem::zeroed()};
    addr.nl_family=libc::AF_NETLINK as _;addr.nl_groups=1;
    if unsafe{libc::bind(socket.as_raw_fd(),(&addr as *const libc::sockaddr_nl).cast(),std::mem::size_of_val(&addr) as _)}<0{return Err(io::Error::last_os_error())}
    println!("LISTENING kernel Prima multicast group1 (receive only), 20 seconds");
    let end=Instant::now()+Duration::from_secs(20);
    let mut buf=[0u8;65536];let mut messages=0;
    while Instant::now()<end && messages<256 {
        let mut poll=libc::pollfd{fd:socket.as_raw_fd(),events:libc::POLLIN,revents:0};
        let ms=end.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
        let n=unsafe{libc::poll(&mut poll,1,ms)};
        if n==0{break} if n<0{return Err(io::Error::last_os_error())}
        let mut from:libc::sockaddr_nl=unsafe{std::mem::zeroed()};let mut len=std::mem::size_of_val(&from) as libc::socklen_t;
        let got=unsafe{libc::recvfrom(socket.as_raw_fd(),buf.as_mut_ptr().cast(),buf.len(),0,(&mut from as *mut libc::sockaddr_nl).cast(),&mut len)};
        if got<0{return Err(io::Error::last_os_error())}
        if from.nl_pid!=0 {continue;}
        messages+=1;
        // Print only text lines about power/suspend, never packet payload dumps.
        for line in buf[..got as usize].split(|b|*b==b'\n') {
            let text=String::from_utf8_lossy(line);let lower=text.to_ascii_lowercase();
            if ["suspend","collapse","pmcstate","bmps","imps","roaming"].iter().any(|s|lower.contains(s)) {
                println!("{}",text.chars().filter(|c|!c.is_control() || *c=='\t').collect::<String>());
            }
        }
    }
    println!("DONE messages={messages}");Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("WIFI LOG: {e}");std::process::exit(1)}}
