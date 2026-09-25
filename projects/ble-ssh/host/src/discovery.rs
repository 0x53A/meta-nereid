//! Discover actual GATT objects: Device1.ServicesResolved can be cleared by
//! BR/EDR disconnect even while the LE GATT connection remains usable.
use dbus::{
    arg::{PropMap, Variant},
    channel::{BusType, Channel, MatchingReceiver},
    message::MatchRule,
    nonblock::{Proxy, SyncConnection},
    Path,
};
use std::{collections::HashMap, future::Future, io, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use uuid::Uuid;

type Objects = HashMap<Path<'static>, HashMap<String, PropMap>>;

pub struct Discovery {
    connection: Arc<SyncConnection>,
    resource: JoinHandle<()>,
    closed: watch::Receiver<bool>,
}

impl Discovery {
    pub fn new() -> Result<Self, dbus::Error> {
        let channel = Channel::get_private(BusType::System)?;
        let (resource, connection) = dbus_tokio::connection::from_channel::<SyncConnection>(channel)?;
        // Setup rediscovery may temporarily monitor more than one service path.
        connection.set_signal_match_mode(true);
        let (closed_sender, closed) = watch::channel(false);
        Ok(Self {
            connection,
            closed,
            resource: tokio::spawn(async move {
                let _ = resource.await;
                let _ = closed_sender.send(true);
            }),
        })
    }

    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        if !*closed.borrow_and_update() {
            let _ = closed.changed().await;
        }
    }

    /// Install before checking live state. A bounded queue preserves a disconnect
    /// that arrives while setup is awaiting another D-Bus reply.
    pub async fn monitor_disconnect(
        &self,
        device: &bluer::Device,
    ) -> Result<(mpsc::Sender<()>, mpsc::Receiver<()>), dbus::Error> {
        let (sender, receiver) = mpsc::channel(1);
        for rule in disconnect_rules(&device_path(device)) {
            let sender = sender.clone();
            let match_string = rule.match_str();
            self.connection.start_receive(
                rule,
                Box::new(move |message, _| {
                    if is_disconnect(&message) {
                        let _ = sender.try_send(());
                    }
                    true
                }),
            );
            self.connection.add_match_no_cb(&match_string).await?;
        }
        Ok((sender, receiver))
    }

    /// AcquireNotify's local socket can outlive its remote service. Observe the
    /// exact service before START so daemon replacement also ends an idle SSH.
    pub async fn monitor_gatt_service(
        &self,
        device: &bluer::Device,
        service: u16,
        sender: mpsc::Sender<()>,
    ) -> Result<(), dbus::Error> {
        let service = format!("{}/service{service:04x}", device_path(device));
        let rule = MatchRule::new_signal("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved")
            .with_sender("org.bluez");
        let match_string = rule.match_str();
        self.connection.start_receive(rule, Box::new(move |message, _| {
            if is_service_removed(&message, &service) {
                let _ = sender.try_send(());
            }
            true
        }));
        self.connection.add_match_no_cb(&match_string).await
    }

    /// Returns false only when neither explicit LE API is available.
    pub async fn connect_le(&self, device: &bluer::Device, uuid: Uuid) -> bluer::Result<bool> {
        // Controller reconnect attempts plus fresh GATT discovery can exceed 30s.
        tokio::time::timeout(Duration::from_secs(60), self.connect_le_inner(device, uuid))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out establishing live LE GATT connection",
                )
            })?
    }

    async fn wait_live(&self, device: &bluer::Device, uuid: Uuid) -> bluer::Result<bool> {
        loop {
            let objects = self.objects().await.map_err(io::Error::other)?;
            let path = device_path(device);
            // MTU appears on cached objects as soon as BlueZ attaches its GATT
            // client, before discovery completes. A freshly established bearer
            // must also finish service discovery before using those handles.
            if fresh_gatt_ready(&objects, &path, uuid) {
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn connect_le_inner(&self, device: &bluer::Device, uuid: Uuid) -> bluer::Result<bool> {
        let path = device_path(device);
        let objects = self.objects().await.map_err(io::Error::other)?;
        if reusable_gatt(&objects, &path, uuid) {
            return Ok(true);
        }
        let proxy = Proxy::new(
            "org.bluez",
            path,
            Duration::from_secs(30),
            &*self.connection,
        );
        let result: Result<(), dbus::Error> = proxy
            .method_call("org.bluez.Bearer.LE1", "Connect", ())
            .await;
        match classify_connect(result)? {
            true => return self.wait_live(device, uuid).await,
            false => (),
        }
        let address_type = match device.address_type().await? {
            bluer::AddressType::LeRandom => "random",
            _ => "public",
        };
        let mut options = PropMap::new();
        options.insert(
            "Address".into(),
            Variant(Box::new(device.address().to_string())),
        );
        options.insert(
            "AddressType".into(),
            Variant(Box::new(address_type.to_owned())),
        );
        let adapter = format!("/org/bluez/{}", device.adapter_name());
        let proxy = Proxy::new(
            "org.bluez",
            adapter,
            Duration::from_secs(30),
            &*self.connection,
        );
        let result: Result<(Path<'static>,), dbus::Error> = proxy
            .method_call("org.bluez.Adapter1", "ConnectDevice", (options,))
            .await;
        if classify_connect(result.map(|_| ()))? {
            self.wait_live(device, uuid).await
        } else {
            Ok(false)
        }
    }

    async fn objects(&self) -> Result<Objects, dbus::Error> {
        let proxy = Proxy::new("org.bluez", "/", Duration::from_secs(5), &*self.connection);
        let (objects,): (Objects,) = proxy
            .method_call(
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                (),
            )
            .await?;
        Ok(objects)
    }

    pub async fn services(
        &self,
        device: &bluer::Device,
        uuid: Uuid,
    ) -> Result<Vec<u16>, dbus::Error> {
        let objects = self.objects().await?;
        let path = device_path(device);
        // Cached objects survive disconnect; MTU requires an attached GATT client.
        if device_bool(&objects, &path, "org.bluez.Bearer.LE1", "ServicesResolved") == Some(false)
            || !has_live_gatt(&objects, &path, uuid)
        {
            return Ok(Vec::new());
        }
        Ok(service_ids(&objects, &path, uuid))
    }
}

fn disconnect_rules(path: &str) -> Vec<MatchRule<'static>> {
    let mut rules = vec![
        MatchRule::new_signal("org.bluez.Bearer.LE1", "Disconnected")
            .with_sender("org.bluez")
            .with_path(path.to_owned()),
        MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
            .with_sender("org.bluez")
            .with_path(path.to_owned()),
    ];
    rules.push(
        MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
            .with_strict_sender("org.freedesktop.DBus")
            .with_path("/org/freedesktop/DBus"),
    );
    rules
}

fn is_disconnect(message: &dbus::Message) -> bool {
    if message.interface().as_deref() == Some("org.freedesktop.DBus") {
        return message
            .read3::<String, String, String>()
            .is_ok_and(|(name, old, new)| name == "org.bluez" && old != new);
    }
    if message.interface().as_deref() == Some("org.bluez.Bearer.LE1") {
        return message.member().as_deref() == Some("Disconnected");
    }
    if let Ok((interface, props, _)) = message.read3::<String, PropMap, Vec<String>>() {
        let false_property = |name| props.get(name).and_then(|value| value.0.as_i64()) == Some(0);
        return (interface == "org.bluez.Device1" && false_property("Connected"))
            || (interface == "org.bluez.Bearer.LE1"
                && (false_property("Connected") || false_property("ServicesResolved")));
    }
    false
}

fn is_service_removed(message: &dbus::Message, service: &str) -> bool {
    let Ok((path, interfaces)) = message.read2::<Path<'static>, Vec<String>>() else {
        return false;
    };
    (&*path == service || path.starts_with(&format!("{service}/")))
        && interfaces.iter().any(|interface| matches!(interface.as_str(),
            "org.bluez.GattService1" | "org.bluez.GattCharacteristic1"))
}

fn device_path(device: &bluer::Device) -> String {
    format!(
        "/org/bluez/{}/dev_{}",
        device.adapter_name(),
        device.address().to_string().replace(':', "_")
    )
}

fn classify_connect(result: Result<(), dbus::Error>) -> bluer::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error.name(),
                Some("org.bluez.Error.AlreadyConnected" | "org.bluez.Error.InProgress")
            ) =>
        {
            Ok(true)
        }
        Err(error)
            if matches!(
                error.name(),
                Some(
                    "org.freedesktop.DBus.Error.UnknownMethod"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                )
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(io::Error::other(error).into()),
    }
}

fn device_bool(objects: &Objects, device: &str, interface: &str, property: &str) -> Option<bool> {
    objects
        .get(&Path::new(device.to_owned()).ok()?)?
        .get(interface)?
        .get(property)?
        .0
        .as_i64()
        .map(|value| value != 0)
}

fn reusable_gatt(objects: &Objects, device: &str, uuid: Uuid) -> bool {
    // Patched BlueZ exposes freshness for this particular LE connection. Unlike
    // a cached MTU or the aggregate Device1 flag, it cannot be satisfied by BR.
    if device_bool(objects, device, "org.bluez.Bearer.LE1", "ServicesResolved").is_some() {
        return fresh_gatt_ready(objects, device, uuid);
    }
    // Only an already-connected LE bearer may bypass global ServicesResolved:
    // unrelated BR/EDR teardown can clear that flag without affecting LE.
    let connected = device_bool(objects, device, "org.bluez.Bearer.LE1", "Connected")
        .unwrap_or_else(|| {
            device_bool(objects, device, "org.bluez.Device1", "ServicesResolved") == Some(true)
        });
    connected && has_live_gatt(objects, device, uuid)
}

fn fresh_gatt_ready(objects: &Objects, device: &str, uuid: Uuid) -> bool {
    device_bool(objects, device, "org.bluez.Bearer.LE1", "ServicesResolved")
        .or_else(|| device_bool(objects, device, "org.bluez.Device1", "ServicesResolved")) == Some(true)
        && device_bool(objects, device, "org.bluez.Bearer.LE1", "Connected") != Some(false)
        && has_live_gatt(objects, device, uuid)
}

fn has_live_gatt(objects: &Objects, device: &str, uuid: Uuid) -> bool {
    let ids = service_ids(objects, device, uuid);
    objects.iter().any(|(path, interfaces)| {
        let Some(props) = interfaces.get("org.bluez.GattCharacteristic1") else {
            return false;
        };
        if props
            .get("MTU")
            .and_then(|value| value.0.as_u64())
            .is_none()
        {
            return false;
        }
        let Some(owner) = props.get("Service").and_then(|value| value.0.as_str()) else {
            return false;
        };
        ids.iter().any(|id| {
            let service = format!("{device}/service{id:04x}");
            owner == service && path.starts_with(&format!("{service}/char"))
        })
    })
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.resource.abort();
    }
}

fn service_ids(objects: &Objects, device: &str, uuid: Uuid) -> Vec<u16> {
    let prefix = format!("{device}/service");
    objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let suffix = path.strip_prefix(&prefix)?;
            if suffix.len() != 4 || !suffix.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let props = interfaces.get("org.bluez.GattService1")?;
            let actual_device = props.get("Device")?.0.as_str()?;
            let actual_uuid = props.get("UUID")?.0.as_str()?;
            if actual_device != device || Uuid::parse_str(actual_uuid).ok()? != uuid {
                return None;
            }
            u16::from_str_radix(suffix, 16).ok()
        })
        .collect()
}

/// Discard setup-era transitions only before a fresh state snapshot. Events
/// arriving during that snapshot remain queued and still terminate the session.
pub async fn establish_baseline(
    events: &mut mpsc::Receiver<()>,
    check: impl Future<Output = bluer::Result<bool>>,
) -> bluer::Result<()> {
    loop {
        match events.try_recv() {
            Ok(()) => (),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Bluetooth disconnect monitor closed",
                )
                .into());
            }
        }
    }
    if !check.await? {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "LE GATT disconnected during setup validation",
        )
        .into());
    }
    Ok(())
}

pub async fn wait<T, F, Fut>(mut probe: F) -> bluer::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bluer::Result<Option<T>>>,
{
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(value) = probe().await? {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for live SSH GATT characteristics",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::arg::Variant;

    fn service(path: &str, owner: &str, uuid: Uuid) -> (Path<'static>, HashMap<String, PropMap>) {
        let mut props = PropMap::new();
        props.insert(
            "Device".into(),
            Variant(Box::new(Path::new(owner.to_owned()).unwrap())),
        );
        props.insert("UUID".into(), Variant(Box::new(uuid.to_string())));
        (
            Path::new(path.to_owned()).unwrap(),
            HashMap::from([("org.bluez.GattService1".into(), props)]),
        )
    }

    #[test]
    fn selects_exact_device_service_even_when_global_services_resolved_is_false() {
        let uuid = Uuid::nil();
        let device = "/org/bluez/hci0/dev_11_22_33_44_55_66";
        let mut objects = Objects::from([
            service(&format!("{device}/service000a"), device, uuid),
            service(&format!("{device}/service000b/char000c"), device, uuid),
            service(
                &format!("{device}/service000d"),
                "/org/bluez/hci1/dev_11_22_33_44_55_66",
                uuid,
            ),
            service(
                "/org/bluez/hci1/dev_11_22_33_44_55_66/service000e",
                device,
                uuid,
            ),
            service(&format!("{device}/service000f"), device, Uuid::from_u128(1)),
        ]);
        let mut device_props = PropMap::new();
        device_props.insert("ServicesResolved".into(), Variant(Box::new(false)));
        objects.insert(
            Path::new(device.to_owned()).unwrap(),
            HashMap::from([("org.bluez.Device1".into(), device_props)]),
        );
        assert_eq!(service_ids(&objects, device, uuid), vec![10]);
    }

    #[test]
    fn ignores_bredr_property_disconnect_and_filters_peer_paths() {
        let device = "/org/bluez/hci0/dev_11_22_33_44_55_66";
        for (interface, expected) in [
            ("org.bluez.Device1", true),
            ("org.bluez.Bearer.BREDR1", false),
        ] {
            let mut props = PropMap::new();
            props.insert("Connected".into(), Variant(Box::new(false)));
            let message = dbus::Message::new_signal(
                device,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
            )
            .unwrap()
            .append3(interface.to_owned(), props, Vec::<String>::new());
            assert_eq!(is_disconnect(&message), expected);
        }
        let rules = disconnect_rules(device);
        let le = dbus::Message::new_signal(device, "org.bluez.Bearer.LE1", "Disconnected").unwrap();
        assert!(rules.iter().any(|rule| rule.matches(&le)) && is_disconnect(&le));
        let bredr =
            dbus::Message::new_signal(device, "org.bluez.Bearer.BREDR1", "Disconnected").unwrap();
        assert!(!rules.iter().any(|rule| rule.matches(&bredr)));
        let other = dbus::Message::new_signal(
            "/org/bluez/hci1/dev_11_22_33_44_55_66",
            "org.bluez.Bearer.LE1",
            "Disconnected",
        )
        .unwrap();
        assert!(!rules.iter().any(|rule| rule.matches(&other)));
    }

    #[test]
    fn invalidates_only_le_discovery_and_selected_service() {
        let service = "/org/bluez/hci0/dev_11_22_33_44_55_66/service1000";
        for (path, interface, expected) in [
            (service.to_owned(), "org.bluez.GattService1", true),
            (format!("{service}/char1003"), "org.bluez.GattCharacteristic1", true),
            (format!("{service}1"), "org.bluez.GattService1", false),
            (service.replace("hci0", "hci1"), "org.bluez.GattService1", false),
            (service.to_owned(), "org.bluez.Battery1", false),
        ] {
            let message = dbus::Message::new_signal("/", "org.freedesktop.DBus.ObjectManager", "InterfacesRemoved")
                .unwrap().append2(Path::new(path).unwrap(), vec![interface.to_owned()]);
            assert_eq!(is_service_removed(&message, service), expected);
        }
        for (interface, expected) in [
            ("org.bluez.Device1", false),
            ("org.bluez.Bearer.BREDR1", false),
            ("org.bluez.Bearer.LE1", true),
        ] {
            let mut props = PropMap::new();
            props.insert("ServicesResolved".into(), Variant(Box::new(false)));
            let message = dbus::Message::new_signal("/test", "org.freedesktop.DBus.Properties", "PropertiesChanged")
                .unwrap().append3(interface.to_owned(), props, Vec::<String>::new());
            assert_eq!(is_disconnect(&message), expected);
        }
    }

    #[test]
    fn owner_monitor_ignores_unrelated_bus_names() {
        for (name, expected) in [("org.bluez", true), ("org.example.Other", false)] {
            let message = dbus::Message::new_signal(
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameOwnerChanged",
            )
            .unwrap()
            .append3(name.to_owned(), ":1.2".to_owned(), String::new());
            assert_eq!(is_disconnect(&message), expected);
        }
    }

    #[test]
    fn explicit_connection_falls_back_only_for_missing_api() {
        assert!(classify_connect(Ok(())).unwrap());
        for name in [
            "org.bluez.Error.AlreadyConnected",
            "org.bluez.Error.InProgress",
        ] {
            assert!(classify_connect(Err(dbus::Error::new_custom(name, "pending"))).unwrap());
        }
        for name in [
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
        ] {
            assert!(!classify_connect(Err(dbus::Error::new_custom(name, "missing"))).unwrap());
        }
        for name in [
            "org.bluez.Error.AuthenticationFailed",
            "org.bluez.Error.NotReady",
            "org.freedesktop.DBus.Error.AccessDenied",
        ] {
            assert!(classify_connect(Err(dbus::Error::new_custom(name, "failed"))).is_err());
        }
    }

    #[test]
    fn cached_services_require_live_mtu_on_the_matching_device() {
        let device = "/org/bluez/hci0/dev_11_22_33_44_55_66";
        let uuid = Uuid::nil();
        let service_path = format!("{device}/service000a");
        let mut objects = Objects::from([service(&service_path, device, uuid)]);
        assert!(!has_live_gatt(&objects, device, uuid));
        let mut props = PropMap::new();
        props.insert(
            "Service".into(),
            Variant(Box::new(Path::new(service_path.clone()).unwrap())),
        );
        props.insert("MTU".into(), Variant(Box::new(23_u16)));
        let char_path = Path::new(format!("{service_path}/char000b")).unwrap();
        objects.insert(
            char_path,
            HashMap::from([("org.bluez.GattCharacteristic1".into(), props)]),
        );
        assert!(has_live_gatt(&objects, device, uuid));
        assert!(!has_live_gatt(
            &objects,
            "/org/bluez/hci1/dev_11_22_33_44_55_66",
            uuid
        ));
    }

    #[test]
    fn fresh_connection_waits_for_discovery_while_existing_le_survives_global_flag_reset() {
        let device = "/org/bluez/hci0/dev_11_22_33_44_55_66";
        let uuid = Uuid::nil();
        let service_path = format!("{device}/service000a");
        let mut objects = Objects::from([service(&service_path, device, uuid)]);
        let mut props = PropMap::new();
        props.insert(
            "Service".into(),
            Variant(Box::new(Path::new(service_path.clone()).unwrap())),
        );
        props.insert("MTU".into(), Variant(Box::new(23_u16)));
        objects.insert(
            Path::new(format!("{service_path}/char000b")).unwrap(),
            HashMap::from([("org.bluez.GattCharacteristic1".into(), props)]),
        );
        let device_path = Path::new(device.to_owned()).unwrap();
        for (le, resolved) in [(true, false), (true, true), (false, true)] {
            let mut le_props = PropMap::new();
            le_props.insert("Connected".into(), Variant(Box::new(le)));
            let mut device_props = PropMap::new();
            device_props.insert("ServicesResolved".into(), Variant(Box::new(resolved)));
            objects.insert(
                device_path.clone(),
                HashMap::from([
                    ("org.bluez.Bearer.LE1".into(), le_props),
                    ("org.bluez.Device1".into(), device_props),
                ]),
            );
            assert_eq!(reusable_gatt(&objects, device, uuid), le);
            assert_eq!(fresh_gatt_ready(&objects, device, uuid), resolved && le);
        }
        // A BR discovery result must never authorize cached LE handles, and a
        // BR disconnect must not invalidate freshly discovered LE handles.
        for (connected, le_ready, global_ready, expected) in [
            (true, false, true, false),
            (true, true, false, true),
            (false, true, true, false),
        ] {
            let mut le_props = PropMap::new();
            le_props.insert("Connected".into(), Variant(Box::new(connected)));
            le_props.insert("ServicesResolved".into(), Variant(Box::new(le_ready)));
            let mut device_props = PropMap::new();
            device_props.insert("ServicesResolved".into(), Variant(Box::new(global_ready)));
            objects.insert(device_path.clone(), HashMap::from([
                ("org.bluez.Bearer.LE1".into(), le_props),
                ("org.bluez.Device1".into(), device_props),
            ]));
            assert_eq!(reusable_gatt(&objects, device, uuid), expected);
            assert_eq!(fresh_gatt_ready(&objects, device, uuid), expected);
        }
    }

    #[tokio::test]
    async fn baseline_discards_setup_events_but_preserves_disconnect_during_recheck() {
        let (sender, mut events) = mpsc::channel(1);
        sender.try_send(()).unwrap();
        establish_baseline(&mut events, async {
            sender.try_send(()).unwrap();
            Ok(true)
        })
        .await
        .unwrap();
        assert_eq!(events.try_recv(), Ok(()));
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(establish_baseline(&mut events, async { Ok(false) })
            .await
            .is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn missing_objects_time_out() {
        let start = tokio::time::Instant::now();
        let result = wait(|| async { Ok::<Option<()>, bluer::Error>(None) }).await;
        assert!(result.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn waits_for_objects_to_appear() {
        let mut attempts = 0;
        let value = wait(|| {
            attempts += 1;
            let found = (attempts == 3).then_some(7);
            async move { Ok(found) }
        })
        .await
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(attempts, 3);
    }
}
