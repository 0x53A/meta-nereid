//! ConnMan owns radio power and persistence; neither radio excludes the other.
use std::collections::HashMap;
use zbus::{Connection, Proxy, zvariant::{OwnedObjectPath, OwnedValue, Value}};

type Technologies = Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>;

pub fn mode(wifi: bool, bluetooth: bool) -> &'static str {
    match (wifi, bluetooth) {
        (true, true) => "wifi+bt",
        (true, false) => "wifi",
        (false, true) => "bt",
        (false, false) => "off",
    }
}

async fn manager(conn: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(conn, "net.connman", "/", "net.connman.Manager").await
}

async fn technologies(conn: &Connection) -> zbus::Result<Technologies> {
    manager(conn).await?.call("GetTechnologies", &()).await
}

pub async fn status(conn: &Connection) -> zbus::Result<(String, bool)> {
    let mut wifi = false;
    let mut bt = false;
    for (_, props) in technologies(conn).await? {
        let kind = props.get("Type").and_then(|v| <&str>::try_from(v).ok());
        let powered = props.get("Powered").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        match kind {
            Some("wifi") => wifi = powered,
            Some("bluetooth") => bt = powered,
            _ => {}
        }
    }
    // Keep the existing wire signature. ConnMan persists its Powered preference.
    Ok((mode(wifi, bt).into(), wifi))
}

pub async fn set_power(conn: &Connection, kind: &str, enabled: bool) -> Result<(), String> {
    for (path, props) in technologies(conn).await.map_err(|e| e.to_string())? {
        if props.get("Type").and_then(|v| <&str>::try_from(v).ok()) == Some(kind) {
            let current = props.get("Powered").and_then(|v| bool::try_from(v).ok());
            if current == Some(enabled) { return Ok(()); }
            let proxy = Proxy::new(conn, "net.connman", path, "net.connman.Technology")
                .await.map_err(|e| e.to_string())?;
            return proxy.call::<_, _, ()>("SetProperty", &("Powered", Value::from(enabled)))
                .await.map_err(|e| e.to_string());
        }
    }
    Err(format!("{kind} technology is unavailable"))
}

pub async fn offline(conn: &Connection, enabled: bool) -> Result<(), String> {
    manager(conn).await.map_err(|e| e.to_string())?
        .call::<_, _, ()>("SetProperty", &("OfflineMode", Value::from(enabled)))
        .await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_radios_are_not_reported_as_exclusive() {
        assert_eq!(mode(true, true), "wifi+bt");
        assert_eq!(mode(true, false), "wifi");
        assert_eq!(mode(false, true), "bt");
        assert_eq!(mode(false, false), "off");
    }
}

#[cfg(test)]
mod integration {
    use super::*;
    use std::sync::{Arc, Mutex};
    use zbus::interface;
    type State = Arc<Mutex<(bool, bool, Vec<(String, bool)>)>>;
    struct Manager(State);
    struct Technology { kind: &'static str, state: State }
    #[interface(name = "net.connman.Manager")]
    impl Manager {
        fn get_technologies(&self) -> Technologies {
            let state = self.0.lock().unwrap();
            [("wifi", state.0), ("bluetooth", state.1)].into_iter().map(|(kind, powered)| {
                (OwnedObjectPath::try_from(format!("/net/connman/technology/{kind}")).unwrap(),
                 HashMap::from([
                     ("Type".into(), OwnedValue::try_from(Value::from(kind)).unwrap()),
                     ("Powered".into(), OwnedValue::from(powered)),
                 ]))
            }).collect()
        }
    }
    #[interface(name = "net.connman.Technology")]
    impl Technology {
        fn set_property(&self, name: &str, value: Value<'_>) -> zbus::fdo::Result<()> {
            if name != "Powered" { return Err(zbus::fdo::Error::InvalidArgs(name.into())); }
            let powered = bool::try_from(value).map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
            let mut state = self.state.lock().unwrap();
            state.2.push((self.kind.into(), powered));
            if self.kind == "wifi" { state.0 = powered; } else { state.1 = powered; }
            Ok(())
        }
    }
    #[tokio::test]
    #[ignore = "run inside dbus-run-session to isolate the mock ConnMan"]
    async fn enabling_wifi_preserves_bluetooth_and_errors_do_not_fake_success() {
        let state: State = Arc::new(Mutex::new((false, true, Vec::new())));
        let _service = zbus::connection::Builder::session().unwrap()
            .name("net.connman").unwrap()
            .serve_at("/", Manager(state.clone())).unwrap()
            .serve_at("/net/connman/technology/wifi", Technology { kind: "wifi", state: state.clone() }).unwrap()
            .serve_at("/net/connman/technology/bluetooth", Technology { kind: "bluetooth", state: state.clone() }).unwrap()
            .build().await.unwrap();
        let client = Connection::session().await.unwrap();
        set_power(&client, "wifi", true).await.unwrap();
        assert_eq!(status(&client).await.unwrap(), ("wifi+bt".into(), true));
        set_power(&client, "wifi", true).await.unwrap(); // idempotent
        assert_eq!(state.lock().unwrap().2, vec![("wifi".into(), true)]);
        set_power(&client, "wifi", false).await.unwrap();
        assert_eq!(status(&client).await.unwrap(), ("bt".into(), false));
        assert!(set_power(&client, "missing", true).await.is_err());
        assert!(state.lock().unwrap().1);
    }
}
