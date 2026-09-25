//! Invalidate all BlueZ registrations whenever its D-Bus owner changes.
use dbus::{
    channel::{BusType, Channel, MatchingReceiver},
    message::MatchRule,
    nonblock::SyncConnection,
};
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};

pub struct BluezOwnerMonitor {
    changed: mpsc::Receiver<String>,
    resource: JoinHandle<()>,
}

impl BluezOwnerMonitor {
    /// Establish this before creating the BlueZ session or reading adapter state.
    /// Changes remain queued even if BlueZ disappears and returns before polling.
    pub async fn new() -> Result<Self, dbus::Error> {
        Self::from_channel(Channel::get_private(BusType::System)?).await
    }

    async fn from_channel(channel: Channel) -> Result<Self, dbus::Error> {
        let (resource, connection): (_, Arc<SyncConnection>) =
            dbus_tokio::connection::from_channel(channel)?;
        let (sender, changed) = mpsc::channel(1);
        let disconnected = sender.clone();
        // Construct the guard before awaiting AddMatch, so cancellation or failure
        // during setup also closes this dedicated connection.
        let monitor = Self {
            changed,
            resource: tokio::spawn(async move {
                let error = resource.await;
                let _ = disconnected.try_send(format!("D-Bus monitor disconnected: {error}"));
            }),
        };
        let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
            .with_strict_sender("org.freedesktop.DBus")
            .with_path("/org/freedesktop/DBus");
        let bus_rule = format!("{},arg0='org.bluez'", rule.match_str());
        // Install locally first: add_match() installs its callback only after the
        // reply, which can race a signal delivered alongside that reply.
        connection.start_receive(
            rule,
            Box::new(move |message, _| {
                if let Ok((name, old, new)) = message.read3::<String, String, String>() {
                    if is_bluez_transition(&name, &old, &new) {
                        let _ = sender.try_send(format!("BlueZ owner changed: {old:?} -> {new:?}"));
                    }
                }
                true
            }),
        );
        connection.add_match_no_cb(&bus_rule).await?;
        Ok(monitor)
    }

    pub async fn changed(&mut self) -> String {
        self.changed
            .recv()
            .await
            .unwrap_or_else(|| "D-Bus owner monitor stopped".into())
    }
}

impl Drop for BluezOwnerMonitor {
    fn drop(&mut self) {
        self.resource.abort();
    }
}

fn is_bluez_transition(name: &str, old: &str, new: &str) -> bool {
    name == "org.bluez" && old != new
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        time::Duration,
    };

    #[test]
    fn filters_unrelated_names_and_accepts_loss_acquisition_and_replacement() {
        assert!(!is_bluez_transition("org.other", ":1.1", ""));
        assert!(!is_bluez_transition("org.bluez", ":1.1", ":1.1"));
        assert!(is_bluez_transition("org.bluez", ":1.1", ""));
        assert!(is_bluez_transition("org.bluez", "", ":1.2"));
        assert!(is_bluez_transition("org.bluez", ":1.1", ":1.2"));
    }

    struct PrivateBus(Child);

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> (PrivateBus, String) {
        let mut bus = PrivateBus(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("private-bus test requires dbus-daemon"),
        );
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        (bus, address.trim().to_owned())
    }

    fn connect(address: &str) -> Channel {
        let mut channel = Channel::open_private(address).unwrap();
        channel.register().unwrap();
        channel
    }

    async fn event(monitor: &mut BluezOwnerMonitor) -> String {
        tokio::time::timeout(Duration::from_secs(2), monitor.changed())
            .await
            .expect("owner transition was not delivered")
    }

    #[tokio::test]
    async fn private_bus_retains_fast_restart_and_reports_bus_loss() {
        let (bus, address) = private_bus();
        let owner = dbus::blocking::Connection::from(connect(&address));
        owner.request_name("org.bluez", true, false, true).unwrap();
        let mut monitor = BluezOwnerMonitor::from_channel(connect(&address))
            .await
            .unwrap();

        owner
            .request_name("org.example.Unrelated", false, false, true)
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), monitor.changed())
                .await
                .is_err()
        );

        // Both transitions occur before the monitor is polled. Retaining either
        // event is sufficient to invalidate all previous registrations.
        owner.release_name("org.bluez").unwrap();
        owner.request_name("org.bluez", true, false, true).unwrap();
        assert!(event(&mut monitor)
            .await
            .starts_with("BlueZ owner changed:"));
        drop(monitor);

        let mut monitor = BluezOwnerMonitor::from_channel(connect(&address))
            .await
            .unwrap();
        let replacement = dbus::blocking::Connection::from(connect(&address));
        replacement
            .request_name("org.bluez", false, true, true)
            .unwrap();
        assert!(event(&mut monitor)
            .await
            .starts_with("BlueZ owner changed:"));
        drop(monitor);

        let mut monitor = BluezOwnerMonitor::from_channel(connect(&address))
            .await
            .unwrap();
        drop(bus);
        assert!(event(&mut monitor)
            .await
            .starts_with("D-Bus monitor disconnected:"));
    }
}
