//! Publish physical-network availability as mutually exclusive systemd targets.
use std::ffi::CStr;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

#[derive(Clone, Copy)]
struct Timing {
    loss: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            loss: Duration::from_secs(120),
        }
    }
}

fn boottime() -> io::Result<Duration> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

struct Timer(OwnedFd);
impl Timer {
    fn new() -> io::Result<Self> {
        fd(unsafe {
            libc::timerfd_create(libc::CLOCK_BOOTTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK)
        })
        .map(Self)
    }
    fn arm(&self, deadline: Option<Duration>) -> io::Result<()> {
        let mut value: libc::itimerspec = unsafe { std::mem::zeroed() };
        if let Some(deadline) = deadline {
            value.it_value.tv_sec = deadline.as_secs() as _;
            value.it_value.tv_nsec = deadline.subsec_nanos() as _;
        }
        if unsafe {
            libc::timerfd_settime(
                self.0.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME,
                &value,
                std::ptr::null_mut(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn drain(&self) -> io::Result<()> {
        let mut ticks = 0u64;
        let n = unsafe { libc::read(self.0.as_raw_fd(), &mut ticks as *mut _ as *mut _, 8) };
        if n == 8 || (n < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN)) {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn wait_events(network: &OwnedFd, signals: &OwnedFd, timer: &Timer) -> io::Result<bool> {
    let mut polls = [
        network.as_raw_fd(),
        signals.as_raw_fd(),
        timer.0.as_raw_fd(),
    ]
    .map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    loop {
        if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, -1) } >= 0 {
            break;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
    // Netlink POLLERR can be queue overflow; drain_network resnapshots after ENOBUFS.
    if polls
        .iter()
        .any(|p| p.revents & (libc::POLLHUP | libc::POLLNVAL) != 0)
        || polls[1..].iter().any(|p| p.revents & libc::POLLERR != 0)
    {
        return Err(io::Error::other("network gate notification fd failed"));
    }
    let shutdown = drain_signals(signals)?;
    drain_network(network)?;
    timer.drain()?;
    Ok(shutdown)
}

fn usable_address(ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && ip != IpAddr::V4(Ipv4Addr::BROADCAST)
}

fn physical_interface(root: &Path, name: &str) -> bool {
    let p = root.join(name);
    // Hoki USB NCM is physical transport but is represented as a virtual NIC.
    // Reject bridge/tun even if named usb0, and never count overlays as underlay.
    !p.join("tun_flags").exists()
        && !p.join("bridge").exists()
        && (p.join("device").exists()
            || p.join("wireless").exists()
            || (name == "usb0"
                && fs::read_to_string(p.join("type")).is_ok_and(|t| t.trim() == "1")))
}

fn ipv6_ready(contents: &str, name: &str, ip: Ipv6Addr) -> bool {
    let hex: String = ip.octets().iter().map(|b| format!("{b:02x}")).collect();
    contents.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() == 6
            && fields[0] == hex
            && fields[5] == name
            && u32::from_str_radix(fields[4], 16).is_ok_and(|flags| flags & (0x40 | 0x08) == 0)
    }) // IFA_F_TENTATIVE / IFA_F_DADFAILED must not open the gate.
}

fn ready_interfaces() -> io::Result<Vec<String>> {
    let mut list = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _guard = Addresses(list);
    let ipv6 = fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
    let mut ready = Vec::new();
    let mut ptr = list;
    while !ptr.is_null() {
        let addr = unsafe { &*ptr };
        ptr = addr.ifa_next;
        let flags = addr.ifa_flags as i32;
        if addr.ifa_addr.is_null()
            || addr.ifa_name.is_null()
            || flags & libc::IFF_LOOPBACK != 0
            || flags & (libc::IFF_UP | libc::IFF_RUNNING) != libc::IFF_UP | libc::IFF_RUNNING
        {
            continue;
        }
        let name = unsafe { CStr::from_ptr(addr.ifa_name) }
            .to_string_lossy()
            .into_owned();
        if !physical_interface(Path::new("/sys/class/net"), &name) {
            continue;
        }
        let ip = match unsafe { (*addr.ifa_addr).sa_family as i32 } {
            libc::AF_INET => {
                let addr = unsafe { &*(addr.ifa_addr as *const libc::sockaddr_in) };
                IpAddr::V4(Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()))
            }
            libc::AF_INET6 => {
                let addr = unsafe { &*(addr.ifa_addr as *const libc::sockaddr_in6) };
                let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
                if !ipv6_ready(&ipv6, &name, ip) {
                    continue;
                }
                IpAddr::V6(ip)
            }
            _ => continue,
        };
        if usable_address(ip) && !ready.contains(&name) {
            ready.push(name);
        }
    }
    ready.sort();
    Ok(ready)
}

fn fd(result: i32) -> io::Result<OwnedFd> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(result) })
    }
}

fn netlink() -> io::Result<OwnedFd> {
    let socket = fd(unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    })?;
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as _;
    address.nl_groups = 1 | 0x10 | 0x100; // RTMGRP_LINK, IPV4_IFADDR, IPV6_IFADDR
    if unsafe {
        libc::bind(
            socket.as_raw_fd(),
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&address) as _,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

fn signals() -> io::Result<OwnedFd> {
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGTERM, libc::SIGINT] {
            libc::sigaddset(&mut set, signal);
        }
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
    }
    fd(unsafe { libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK) })
}

fn drain_network(socket: &OwnedFd) -> io::Result<()> {
    let mut bytes = [0u8; 8192];
    loop {
        let n = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        if n >= 0 {
            continue;
        } // Payload isn't used: resnapshot all interfaces after draining.
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN) => return Ok(()),
            Some(libc::EINTR | libc::ENOBUFS) => continue, // Queue overflow: resnapshot, not stale state.
            _ => return Err(error),
        }
    }
}

fn drain_signals(socket: &OwnedFd) -> io::Result<bool> {
    let mut shutdown = false;
    loop {
        let mut signal: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let n = unsafe {
            libc::read(
                socket.as_raw_fd(),
                &mut signal as *mut _ as *mut _,
                std::mem::size_of_val(&signal),
            )
        };
        if n == std::mem::size_of_val(&signal) as isize {
            shutdown |= matches!(signal.ssi_signo as i32, libc::SIGTERM | libc::SIGINT);
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN) => return Ok(shutdown),
            Some(libc::EINTR) => continue,
            _ => return Err(error),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Action {
    Start,
    Stop,
    Wait,
}
#[derive(Default)]
struct LossGrace {
    since: Option<Duration>,
}
impl LossGrace {
    fn action(&mut self, ready: bool, running: bool, now: Duration, grace: Duration) -> Action {
        if ready || !running {
            self.since = None;
        }
        match (ready, running) {
            (true, false) => Action::Start,
            (false, true) => {
                let since = *self.since.get_or_insert(now);
                if now.saturating_sub(since) >= grace {
                    Action::Stop
                } else {
                    Action::Wait
                }
            }
            _ => Action::Wait,
        }
    }
}

struct Publisher {
    user: bool,
    prefix: String,
}
impl Publisher {
    fn publish(&self, online: bool) -> io::Result<()> {
        let target = format!(
            "{}-{}.target",
            self.prefix,
            if online { "online" } else { "offline" }
        );
        self.request("start", &target)
    }
    fn stop_online(&self) -> io::Result<()> {
        self.request("stop", &format!("{}-online.target", self.prefix))
    }
    fn request(&self, action: &str, target: &str) -> io::Result<()> {
        let mut command = Command::new("systemctl");
        if self.user {
            command.arg("--user");
        }
        command.args(["--no-block", action, target]);
        // Submit a systemd transaction only; never wait for consumer shutdown.
        // systemctl must not inherit our blocked termination signals.
        unsafe {
            command.pre_exec(|| {
                let mut empty: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut empty);
                let error = libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                Ok(())
            });
        }
        let status = command.status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "cannot publish {target}: {status}"
            )));
        }
        eprintln!("network availability: requested {action} {target}");
        Ok(())
    }
}

fn run(publisher: &Publisher, timing: Timing) -> io::Result<()> {
    let network = netlink()?; // Subscribe BEFORE initial snapshot to avoid lost transitions.
    let signals = signals()?;
    let timer = Timer::new()?;
    let mut loss = LossGrace::default();
    let mut published = None;
    loop {
        let interfaces = ready_interfaces()?;
        let ready = !interfaces.is_empty();
        let decision = loss.action(ready, published == Some(true), boottime()?, timing.loss);
        let desired = match decision {
            Action::Start => Some(true),
            Action::Stop => Some(false),
            Action::Wait if published.is_none() => Some(ready),
            _ => None,
        };
        if let Some(online) = desired {
            publisher.publish(online)?;
            published = Some(online);
            loss.since = None;
        }
        timer.arm(loss.since.map(|since| since + timing.loss))?;
        if wait_events(&network, &signals, &timer)? {
            return Ok(());
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--check"] {
        return match ready_interfaces() {
            Ok(interfaces) => {
                println!("{}", interfaces.join("\n"));
                ExitCode::from(if interfaces.is_empty() { 1 } else { 0 })
            }
            Err(error) => {
                eprintln!("network availability: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let mut timing = Timing::default();
    let mut publisher = Publisher {
        user: false,
        prefix: "hoki-network".into(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--user" => publisher.user = true,
            "--loss-grace-seconds" => {
                let Some(seconds) = args
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|n| (1..=86400).contains(n))
                else {
                    eprintln!("--loss-grace-seconds requires seconds in 1..86400");
                    return ExitCode::FAILURE;
                };
                timing.loss = Duration::from_secs(seconds);
            }
            "--unit-prefix" => {
                let Some(prefix) = args.next().filter(|p| {
                    !p.is_empty()
                        && p.len() <= 128
                        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }) else {
                    eprintln!("--unit-prefix requires an alphanumeric/hyphen unit prefix");
                    return ExitCode::FAILURE;
                };
                publisher.prefix = prefix.clone();
            }
            _ => {
                eprintln!("usage: hoki-networkd [--user] [--loss-grace-seconds N] [--unit-prefix NAME] | --check");
                return ExitCode::FAILURE;
            }
        }
    }
    let result = run(&publisher, timing);
    // Withdraw online on termination/error. Do not start a new target during
    // system shutdown: offline conflicts with shutdown.target.
    let offline = publisher.stop_online();
    match result.and(offline) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("network availability: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_addresses_work_without_internet_but_non_hosts_do_not() {
        for ip in [
            "192.168.222.15",
            "10.0.0.3",
            "169.254.1.2",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
        ] {
            assert!(usable_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "ff02::1",
        ] {
            assert!(!usable_address(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn ipv6_dad_must_complete_on_the_matching_interface() {
        let ip = "fe80::1".parse().unwrap();
        for flags in ["00", "80"] {
            assert!(ipv6_ready(
                &format!("fe800000000000000000000000000001 02 40 20 {flags} usb0"),
                "usb0",
                ip
            ));
        }
        for flags in ["40", "08", "48"] {
            assert!(!ipv6_ready(
                &format!("fe800000000000000000000000000001 02 40 20 {flags} usb0"),
                "usb0",
                ip
            ));
        }
        assert!(!ipv6_ready(
            "fe800000000000000000000000000001 02 40 20 80 wlan0",
            "usb0",
            ip
        ));
        assert!(!ipv6_ready("", "usb0", ip));
    }
    #[test]
    fn physical_wifi_usb_and_ethernet_exclude_overlays_and_bridges() {
        let root = std::env::temp_dir().join(format!("hoki-network-test-{}", std::process::id()));
        fs::create_dir_all(root.join("eth0/device")).unwrap();
        fs::create_dir_all(root.join("wlan0/wireless")).unwrap();
        fs::create_dir_all(root.join("usb0")).unwrap();
        fs::write(root.join("usb0/type"), "1\n").unwrap();
        for name in ["eth0", "wlan0", "usb0"] {
            assert!(physical_interface(&root, name));
        }
        for name in ["lo", "tailscale0", "tun0", "veth0", "br0"] {
            assert!(!physical_interface(&root, name));
        }
        fs::write(root.join("usb0/tun_flags"), "1").unwrap();
        assert!(!physical_interface(&root, "usb0"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn persistent_loss_requires_two_minutes_and_recovery_resets_it() {
        let mut loss = LossGrace::default();
        let timing = Timing::default();
        assert_eq!(timing.loss, Duration::from_secs(120));
        for (ready, running, seconds, expected) in [
            (false, false, 0, Action::Wait),
            (true, false, 0, Action::Start),
            (false, true, 10, Action::Wait),
            (false, true, 129, Action::Wait),
            (true, true, 129, Action::Wait),
            (false, true, 130, Action::Wait),
            (false, true, 249, Action::Wait),
            (false, true, 250, Action::Stop),
            (false, false, 310, Action::Wait),
            (true, false, 310, Action::Start),
        ] {
            assert_eq!(
                loss.action(ready, running, Duration::from_secs(seconds), timing.loss),
                expected
            );
        }
    }
}
