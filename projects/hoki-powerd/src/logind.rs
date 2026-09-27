//! Standard Linux inhibition and sleep lifecycle. Never bypass root inhibitors.
use futures_util::StreamExt;
use std::{io, time::Duration};
type Inhibitor = (String, String, String, String, u32, u32);
fn blocks(what: &str, mode: &str) -> bool {
    matches!(mode, "block" | "block-weak") && what.split(':').any(|w| matches!(w, "sleep" | "idle"))
}
async fn proxy(connection: &zbus::Connection) -> zbus::Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
}
pub async fn blockers() -> Result<Vec<String>, String> {
    let connection = zbus::connection::Builder::system()
        .map_err(|e| e.to_string())?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let p = proxy(&connection).await.map_err(|e| e.to_string())?;
    let inhibitors: Vec<Inhibitor> = p
        .call("ListInhibitors", &())
        .await
        .map_err(|e| e.to_string())?;
    Ok(inhibitors
        .into_iter()
        .filter(|(what, _, _, mode, _, _)| blocks(what, mode))
        .map(|(_, who, why, _, _, _)| format!("{who}: {why}"))
        .collect())
}
pub async fn suspend() -> io::Result<()> {
    let result = async {
        let connection = zbus::connection::Builder::system()?
            .method_timeout(Duration::from_secs(15))
            .build()
            .await?;
        suspend_on(&connection).await
    };
    tokio::time::timeout(Duration::from_secs(90), result)
        .await
        .map_err(|_| io::Error::other("logind sleep completion timed out"))?
        .map_err(io::Error::other)
}
async fn suspend_on(connection: &zbus::Connection) -> zbus::Result<()> {
    let p = proxy(connection).await?;
    // Subscribe before requesting: the method can return before or after
    // preparation begins. logind handles delay locks and its own hooks.
    let mut signals = p.receive_signal("PrepareForSleep").await?;
    p.call::<_, _, ()>("SuspendWithFlags", &1u64).await?;
    // 0x01 = SD_LOGIND_ROOT_CHECK_INHIBITORS. No fallback to Suspend(false)
    // or a direct mem write on old/absent logind: those can bypass locks.
    let mut prepared = false;
    while let Some(message) = signals.next().await {
        let (preparing,): (bool,) = message.body().deserialize()?;
        if preparing {
            prepared = true;
        } else if prepared {
            return Ok::<(), zbus::Error>(());
        }
    }
    Err(zbus::Error::Failure(
        "logind sleep signal stream ended".into(),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    struct FakeLogind;
    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeLogind {
        async fn suspend_with_flags(
            &self,
            flags: u64,
            #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            if flags != 1 {
                return Err(zbus::fdo::Error::Failed("root inhibitors bypassed".into()));
            }
            Self::prepare_for_sleep(&emitter, true).await.unwrap();
            Self::prepare_for_sleep(&emitter, false).await.unwrap();
            Ok(())
        }
        #[zbus(signal)]
        async fn prepare_for_sleep(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            active: bool,
        ) -> zbus::Result<()>;
    }
    #[tokio::test]
    async fn private_bus_observes_both_signals_even_before_method_reply() {
        use std::io::BufRead;
        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut bus = Bus(std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon required for isolated integration test"));
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let server = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/login1", FakeLogind)
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), suspend_on(&client))
            .await
            .unwrap()
            .unwrap();
        drop(server);
    }
    #[test]
    fn standard_block_and_idle_locks_are_respected_but_delays_are_negotiated() {
        assert!(blocks("shutdown:sleep", "block"));
        assert!(blocks("idle", "block-weak"));
        assert!(!blocks("sleep", "delay"));
        assert!(!blocks("handle-power-key", "block"));
    }
}
