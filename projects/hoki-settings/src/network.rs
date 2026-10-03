//! ConnMan saved-network selection and local, read-only network diagnostics.
use std::{collections::HashMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue},
};
type Properties = HashMap<String, OwnedValue>;
type Services = Vec<(OwnedObjectPath, Properties)>;
#[derive(Clone, Default)]
pub struct Network {
    pub path: String,
    pub name: String,
    pub state: String,
    pub strength: Option<u8>,
    pub connected: bool,
    pub details: Vec<(String, String)>,
}
#[derive(Clone, Default)]
pub struct Snapshot {
    pub available: bool,
    pub status: String,
    pub wifi_powered: bool,
    pub networks: Vec<Network>,
    pub diagnostics: Vec<(String, String)>,
}
fn connection(timeout: u64) -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::system()?
        .method_timeout(Duration::from_secs(timeout))
        .build()
}
fn text(props: &Properties, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .to_owned()
}
fn boolean(props: &Properties, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}
fn dictionary(props: &Properties, key: &str) -> Properties {
    props
        .get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Properties::try_from(v).ok())
        .unwrap_or_default()
}
fn strings(props: &Properties, key: &str) -> Vec<String> {
    props
        .get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .unwrap_or_default()
}
fn row(rows: &mut Vec<(String, String)>, label: &str, value: String) {
    if !value.is_empty() {
        rows.push((label.into(), value));
    }
}
fn saved(props: &Properties) -> bool {
    text(props, "Type") == "wifi" && (boolean(props, "Favorite") || boolean(props, "Immutable"))
}
fn connected(state: &str) -> bool {
    matches!(state, "ready" | "online")
}
fn parse_network(path: &str, props: &Properties) -> Network {
    let state = text(props, "State");
    let strength = props.get("Strength").and_then(|v| u8::try_from(v).ok());
    let mut details = vec![("State".into(), state.clone())];
    row(
        &mut details,
        "Security",
        strings(props, "Security").join(", "),
    );
    if let Some(strength) = strength {
        row(&mut details, "Signal", format!("{strength}%"));
    }
    row(
        &mut details,
        "Auto-connect",
        if boolean(props, "AutoConnect") {
            "On"
        } else {
            "Off"
        }
        .into(),
    );
    for family in ["IPv4", "IPv6"] {
        let config = dictionary(props, family);
        row(&mut details, family, text(&config, "Address"));
        row(
            &mut details,
            &format!("{family} gateway"),
            text(&config, "Gateway"),
        );
        row(
            &mut details,
            &format!("{family} method"),
            text(&config, "Method"),
        );
    }
    row(
        &mut details,
        "DNS",
        strings(props, "Nameservers").join("\n"),
    );
    let ethernet = dictionary(props, "Ethernet");
    row(&mut details, "Interface", text(&ethernet, "Interface"));
    row(&mut details, "MAC", text(&ethernet, "Address"));
    row(&mut details, "Last error", text(props, "Error"));
    let name = text(props, "Name");
    Network {
        path: path.into(),
        name: if name.is_empty() {
            "Hidden saved network".into()
        } else {
            name
        },
        connected: connected(&state),
        state,
        strength,
        details,
    }
}
fn read_on(conn: &Connection) -> zbus::Result<Snapshot> {
    let manager = Proxy::new(conn, "net.connman", "/", "net.connman.Manager")?;
    let properties: Properties = manager.call("GetProperties", &())?;
    let services: Services = manager.call("GetServices", &())?;
    let technologies: Services = manager.call("GetTechnologies", &())?;
    let wifi_powered = technologies
        .iter()
        .any(|(_, p)| text(p, "Type") == "wifi" && boolean(p, "Powered"));
    let status = text(&properties, "State");
    let mut diagnostics = vec![
        ("ConnMan".into(), status.clone()),
        (
            "Airplane mode".into(),
            if boolean(&properties, "OfflineMode") {
                "On"
            } else {
                "Off"
            }
            .into(),
        ),
        (
            "Wi-Fi".into(),
            if wifi_powered { "On" } else { "Off" }.into(),
        ),
    ];
    for (_, p) in &services {
        if connected(&text(p, "State")) {
            let name = text(p, "Name");
            row(
                &mut diagnostics,
                "Connected service",
                format!(
                    "{} · {}",
                    if name.is_empty() {
                        text(p, "Type")
                    } else {
                        name
                    },
                    text(p, "State")
                ),
            );
            let ipv4 = dictionary(p, "IPv4");
            row(&mut diagnostics, "Gateway", text(&ipv4, "Gateway"));
            row(
                &mut diagnostics,
                "Service DNS",
                strings(p, "Nameservers").join("\n"),
            );
        }
    }
    let networks = services
        .iter()
        .filter(|(_, p)| saved(p))
        .map(|(path, p)| parse_network(path.as_str(), p))
        .collect();
    Ok(Snapshot {
        available: true,
        status,
        wifi_powered,
        networks,
        diagnostics,
    })
}
pub fn read() -> Snapshot {
    if crate::simulated::enabled() {
        return simulated();
    }
    let mut state = connection(2)
        .and_then(|c| read_on(&c))
        .unwrap_or_else(|_| Snapshot {
            status: "ConnMan unavailable".into(),
            diagnostics: vec![("ConnMan".into(), "Unavailable".into())],
            ..Default::default()
        });
    local_diagnostics(&mut state.diagnostics);
    state
}
fn local_diagnostics(rows: &mut Vec<(String, String)>) {
    let mut addresses: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut first = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut first) } == 0 {
        let mut current = first;
        while !current.is_null() {
            let entry = unsafe { &*current };
            let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            if name != "lo" && !entry.ifa_addr.is_null() {
                let family = unsafe { (*entry.ifa_addr).sa_family } as i32;
                let address = match family {
                    libc::AF_INET => {
                        let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                        Some(
                            std::net::Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes())
                                .to_string(),
                        )
                    }
                    libc::AF_INET6 => {
                        let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                        Some(std::net::Ipv6Addr::from(addr.sin6_addr.s6_addr).to_string())
                    }
                    _ => None,
                };
                let values = addresses.entry(name).or_default();
                if let Some(address) = address {
                    values.push(address);
                }
            }
            current = entry.ifa_next;
        }
        unsafe { libc::freeifaddrs(first) };
    }
    for (interface, addresses) in addresses {
        let state = std::fs::read_to_string(format!("/sys/class/net/{interface}/operstate"))
            .unwrap_or_else(|_| "unknown".into());
        if addresses.is_empty() {
            rows.push((
                format!("{interface} · {}", state.trim()),
                "No IP address".into(),
            ));
        } else {
            for address in addresses {
                rows.push((format!("{interface} · {}", state.trim()), address));
            }
        }
    }
    let resolver = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let dns = resolver
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("nameserver")).then(|| fields.next().unwrap_or("").to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");
    row(
        rows,
        "System DNS",
        if dns.is_empty() {
            "Not configured".into()
        } else {
            dns
        },
    );
}
pub fn action(action: &str) -> Result<String, String> {
    if crate::simulated::enabled() {
        return Ok("Simulator: request accepted".into());
    }
    let conn = connection(40).map_err(|e| e.to_string())?;
    action_on(&conn, action)
}
fn action_on(conn: &Connection, action: &str) -> Result<String, String> {
    let manager =
        Proxy::new(conn, "net.connman", "/", "net.connman.Manager").map_err(|e| e.to_string())?;
    if action == "network-refresh" {
        return Ok(String::new());
    }
    if action == "network-scan" {
        let technologies: Services = manager
            .call("GetTechnologies", &())
            .map_err(|e| e.to_string())?;
        let (path, props) = technologies
            .iter()
            .find(|(_, p)| text(p, "Type") == "wifi")
            .ok_or("Wi-Fi unavailable")?;
        if !boolean(props, "Powered") {
            return Err("Turn on Wi-Fi first".into());
        }
        let wifi = Proxy::new(conn, "net.connman", path.as_str(), "net.connman.Technology")
            .map_err(|e| e.to_string())?;
        wifi.call::<_, _, ()>("Scan", &())
            .map_err(|e| e.to_string())?;
        return Ok(String::new());
    }
    let (operation, path) = action.split_once(':').ok_or("Invalid network action")?;
    let method = match operation {
        "network-connect" => "Connect",
        "network-disconnect" => "Disconnect",
        _ => return Err("Invalid network action".into()),
    };
    // Resolve again at action time. Never connect a new/unsaved service, and
    // never let an SSID or stale list index choose a different network.
    let services: Services = manager
        .call("GetServices", &())
        .map_err(|e| e.to_string())?;
    let (_, props) = services
        .iter()
        .find(|(p, props)| p.as_str() == path && saved(props))
        .ok_or("Saved network is no longer available; refresh the list")?;
    let current = text(props, "State");
    if method == "Connect" && connected(&current) {
        return Ok(String::new());
    }
    if method == "Disconnect" && !connected(&current) {
        return Ok(String::new());
    }
    let service =
        Proxy::new(conn, "net.connman", path, "net.connman.Service").map_err(|e| e.to_string())?;
    service
        .call::<_, _, ()>(method, &())
        .map_err(|e| format!("{e}. Saved credentials must already be configured."))?;
    Ok(String::new())
}
fn simulated() -> Snapshot {
    Snapshot {
        available: true,
        status: "ready".into(),
        wifi_powered: true,
        networks: vec![Network {
            path: "/net/connman/service/wifi_saved".into(),
            name: "Home Wi-Fi".into(),
            state: "ready".into(),
            strength: Some(72),
            connected: true,
            details: vec![
                ("State".into(), "ready".into()),
                ("IPv4".into(), "192.0.2.10".into()),
                ("DNS".into(), "192.0.2.1".into()),
            ],
        }],
        diagnostics: vec![
            ("ConnMan".into(), "ready".into()),
            ("wlan0 · up".into(), "192.0.2.10".into()),
            ("System DNS".into(), "192.0.2.1".into()),
        ],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn props(favorite: bool) -> Properties {
        HashMap::from([
            ("Type".into(), zbus::zvariant::Str::from("wifi").into()),
            ("Name".into(), zbus::zvariant::Str::from("same name").into()),
            ("Favorite".into(), favorite.into()),
        ])
    }
    #[test]
    fn saved_filter_excludes_new_networks_and_keeps_service_identity() {
        assert!(!saved(&props(false)));
        assert!(saved(&props(true)));
        let a = parse_network("/net/connman/service/one", &props(true));
        let b = parse_network("/net/connman/service/two", &props(true));
        assert_eq!(a.name, b.name);
        assert_ne!(a.path, b.path);
        assert_eq!(a.strength, None);
        assert!(!a.connected);
        assert!(connected("ready"));
        assert!(connected("online"));
        assert!(!connected("association"));
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    };
    struct Manager {
        present: Arc<AtomicBool>,
    }
    #[zbus::interface(name = "net.connman.Manager")]
    impl Manager {
        fn get_services(&self) -> Services {
            if !self.present.load(Ordering::SeqCst) {
                return vec![];
            }
            [true, false]
                .into_iter()
                .map(|saved| {
                    let path = if saved {
                        "/net/connman/service/saved"
                    } else {
                        "/net/connman/service/new"
                    };
                    (
                        OwnedObjectPath::try_from(path).unwrap(),
                        HashMap::from([
                            ("Type".into(), zbus::zvariant::Str::from("wifi").into()),
                            (
                                "Name".into(),
                                zbus::zvariant::Str::from("Duplicate SSID").into(),
                            ),
                            ("State".into(), zbus::zvariant::Str::from("idle").into()),
                            ("Favorite".into(), saved.into()),
                        ]),
                    )
                })
                .collect()
        }
    }
    struct Service(Arc<AtomicU32>);
    #[zbus::interface(name = "net.connman.Service")]
    impl Service {
        fn connect(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    #[ignore = "run under dbus-run-session for isolated ConnMan mock"]
    fn actions_revalidate_saved_service_paths() {
        let count = Arc::new(AtomicU32::new(0));
        let present = Arc::new(AtomicBool::new(true));
        let _server = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name("net.connman")
            .unwrap()
            .serve_at(
                "/",
                Manager {
                    present: present.clone(),
                },
            )
            .unwrap()
            .serve_at("/net/connman/service/saved", Service(count.clone()))
            .unwrap()
            .serve_at("/net/connman/service/new", Service(count.clone()))
            .unwrap()
            .build()
            .unwrap();
        let client = Connection::session().unwrap();
        assert!(action_on(&client, "network-connect:/net/connman/service/new").is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        action_on(&client, "network-connect:/net/connman/service/saved").unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        present.store(false, Ordering::SeqCst);
        assert!(action_on(&client, "network-connect:/net/connman/service/saved").is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
