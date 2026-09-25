//! Minimal nl80211 WoWLAN query/configuration for the suspend investigation.
use std::{io, os::fd::{AsRawFd, FromRawFd, OwnedFd}};
fn attr(kind:u16, payload:&[u8])->Vec<u8>{
 let mut a=Vec::new();a.extend_from_slice(&((payload.len()+4) as u16).to_ne_bytes());a.extend_from_slice(&kind.to_ne_bytes());a.extend_from_slice(payload);a.resize((a.len()+3)&!3,0);a
}
fn attrs(mut b:&[u8])->io::Result<Vec<(u16,Vec<u8>)>>{
 let mut out=Vec::new();while !b.is_empty(){if b.len()<4{return Err(io::Error::other("short attr"))}let n=u16::from_ne_bytes(b[..2].try_into().unwrap()) as usize;let k=u16::from_ne_bytes(b[2..4].try_into().unwrap())&0x3fff;if n<4||n>b.len(){return Err(io::Error::other("bad attr"))}out.push((k,b[4..n].to_vec()));let next=(n+3)&!3;if next>b.len(){return Err(io::Error::other("bad padding"))}b=&b[next..];}Ok(out)
}
fn request(fd:i32,family:u16,cmd:u8,seq:u32,a:Vec<u8>,ack:bool)->io::Result<Vec<u8>>{
 let mut b=Vec::new();b.extend_from_slice(&((20+a.len()) as u32).to_ne_bytes());b.extend_from_slice(&family.to_ne_bytes());b.extend_from_slice(&(if ack{5u16}else{1u16}).to_ne_bytes());b.extend_from_slice(&seq.to_ne_bytes());b.extend_from_slice(&0u32.to_ne_bytes());b.extend_from_slice(&[cmd,1,0,0]);b.extend(a);
 if unsafe{libc::send(fd,b.as_ptr().cast(),b.len(),0)}!=b.len() as isize{return Err(io::Error::last_os_error())}
 let mut buf=[0u8;16384];let n=unsafe{libc::recv(fd,buf.as_mut_ptr().cast(),buf.len(),0)};if n<0{return Err(io::Error::last_os_error())}let n=n as usize;if n<20{return Err(io::Error::other("short reply"))}
 let len=u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;if len>n||len<20||u32::from_ne_bytes(buf[8..12].try_into().unwrap())!=seq{return Err(io::Error::other("invalid reply"))}
 let kind=u16::from_ne_bytes(buf[4..6].try_into().unwrap());if kind==2{let err=i32::from_ne_bytes(buf[16..20].try_into().unwrap());return if err==0{Ok(Vec::new())}else{Err(io::Error::from_raw_os_error(-err))}}
 if kind!=family{return Err(io::Error::other("unexpected family"))}Ok(buf[20..len].to_vec())
}
fn run()->io::Result<()>{
 let args:Vec<_>=std::env::args().collect();let mode=args.get(1).map(String::as_str).unwrap_or("show");if !["show","any","off"].contains(&mode){return Err(io::Error::other("usage: hoki-wowlan show|any|off [phy-index]"))}let phy:u32=args.get(2).map(String::as_str).unwrap_or("1").parse().map_err(io::Error::other)?;
 let raw=unsafe{libc::socket(libc::AF_NETLINK,libc::SOCK_RAW|libc::SOCK_CLOEXEC,libc::NETLINK_GENERIC)};if raw<0{return Err(io::Error::last_os_error())}let fd=unsafe{OwnedFd::from_raw_fd(raw)};let mut addr:libc::sockaddr_nl=unsafe{std::mem::zeroed()};addr.nl_family=libc::AF_NETLINK as u16;
 if unsafe{libc::connect(fd.as_raw_fd(),(&addr as *const libc::sockaddr_nl).cast(),std::mem::size_of_val(&addr) as u32)}<0{return Err(io::Error::last_os_error())}
 let tv=libc::timeval{tv_sec:5,tv_usec:0};if unsafe{libc::setsockopt(raw,libc::SOL_SOCKET,libc::SO_RCVTIMEO,(&tv as *const libc::timeval).cast(),std::mem::size_of_val(&tv) as u32)}<0{return Err(io::Error::last_os_error())}
 let reply=request(raw,16,3,1,attr(2,b"nl80211\0"),false)?;let family=attrs(&reply)?.into_iter().find(|(k,_)|*k==1).ok_or_else(||io::Error::other("no family"))?.1;if family.len()!=2{return Err(io::Error::other("bad family id"))}let family=u16::from_ne_bytes(family.try_into().unwrap());
 let mut a=attr(1,&phy.to_ne_bytes());if mode!="show"{if mode=="any"{a.extend(attr(117|0x8000,&attr(1,&[])))}request(raw,family,74,2,a,true)?;println!("WoWLAN {mode} accepted");a=attr(1,&phy.to_ne_bytes());}
 let reply=request(raw,family,73,3,a,false)?;let triggers=attrs(&reply)?.into_iter().find(|(k,_)|*k==117);match triggers{None=>println!("phy{phy}: WoWLAN disabled"),Some((_,v))=>println!("phy{phy}: WoWLAN trigger IDs {:?}",attrs(&v)?.iter().map(|(k,_)|*k).collect::<Vec<_>>())}Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("WoWLAN: {e}");std::process::exit(1)}}
