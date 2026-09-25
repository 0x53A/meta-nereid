mod radio;
use std::process::Command;
use zbus::{connection, interface, Connection, Proxy};

struct RadioManager { conn: Connection }

fn response(result: Result<(), String>) -> String {
    result.map(|()| "ok".to_owned()).unwrap_or_else(|e| e)
}

#[interface(name = "org.hoki.radio.Manager")]
impl RadioManager {
    async fn status(&self) -> zbus::fdo::Result<(String, bool)> {
        radio::status(&self.conn).await.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    // Compatibility methods now enable one radio without disabling the other.
    async fn switch_to_wifi(&self) -> String { response(radio::set_power(&self.conn, "wifi", true).await) }
    async fn switch_to_bt(&self) -> String { response(radio::set_power(&self.conn, "bluetooth", true).await) }
    async fn set_wifi_enabled(&self, enabled: bool) -> String { response(radio::set_power(&self.conn, "wifi", enabled).await) }
    async fn set_bluetooth_enabled(&self, enabled: bool) -> String { response(radio::set_power(&self.conn, "bluetooth", enabled).await) }
    async fn disable_radio(&self) -> String { response(radio::offline(&self.conn, true).await) }
    async fn enable_radio(&self) -> String { response(radio::offline(&self.conn, false).await) }

    async fn set_usb_mode(&self, mode: String) -> String {
        if !matches!(mode.as_str(), "adb_mode" | "developer_mode" | "charging_only") {
            return "Unsupported USB mode".into();
        }
        // usb-moded is the sole mode owner. Never tear down ADB before knowing
        // whether it accepts the requested mode.
        let proxy = match Proxy::new(&self.conn, "com.meego.usb_moded", "/com/meego/usb_moded", "com.meego.usb_moded").await {
            Ok(proxy) => proxy,
            Err(e) => return e.to_string(),
        };
        match proxy.call_method("set_mode", &(mode,)).await {
            Ok(_) => "ok".into(),
            Err(e) => e.to_string(),
        }
    }

    /// Reboot the system.
    async fn reboot(&self) -> String {
        match Command::new("systemctl").arg("reboot").status() {
            Ok(status) if status.success() => "ok".into(),
            Ok(status) => format!("reboot failed: {status}"),
            Err(error) => format!("reboot failed: {error}"),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Hoki WLAN teardown can legitimately take just over 15 seconds. Keep
    // this below the settings client's 30-second deadline.
    let conn = connection::Builder::system()?.method_timeout(std::time::Duration::from_secs(25)).build().await?;
    let _conn = connection::Builder::system()?
        .name("org.hoki.radio")?
        .serve_at("/org/hoki/radio", RadioManager { conn })?
        .build()
        .await?;

    std::future::pending::<()>().await;

    Ok(())
}
