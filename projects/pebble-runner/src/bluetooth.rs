//! Compatibility policy: legacy Bluetooth indicator means adapter powered.
//! Modern Pebble app/PebbleKit connection APIs remain disconnected.
static mut HANDLER: Option<extern "C" fn(bool)> = None;
static mut LAST: Option<bool> = None;
#[cfg(target_arch = "arm")]
static mut CONNECTION: Option<zbus::blocking::Connection> = None;

pub fn peek() -> bool {
    #[cfg(target_arch = "arm")]
    unsafe {
        if CONNECTION.is_none() {
            CONNECTION = zbus::blocking::connection::Builder::system()
                .ok()
                .and_then(|b| {
                    b.method_timeout(std::time::Duration::from_millis(300))
                        .build()
                        .ok()
                });
        }
        let powered = CONNECTION
            .as_ref()
            .and_then(|c| {
                c.call_method(
                    Some("org.bluez"),
                    "/org/bluez/hci0",
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &("org.bluez.Adapter1", "Powered"),
                )
                .ok()
            })
            .and_then(|r| r.body().deserialize::<zbus::zvariant::OwnedValue>().ok())
            .and_then(|v| bool::try_from(v).ok());
        if powered.is_none() {
            CONNECTION = None;
        }
        return powered.unwrap_or(false);
    }
    #[cfg(not(target_arch = "arm"))]
    false
}
pub fn subscribe(handler: Option<extern "C" fn(bool)>) {
    unsafe {
        HANDLER = handler;
        LAST = Some(peek());
    }
}
pub fn poll() -> bool {
    let Some(handler) = (unsafe { HANDLER }) else {
        return false;
    };
    let powered = peek();
    unsafe {
        if LAST == Some(powered) {
            return false;
        }
        LAST = Some(powered);
    }
    handler(powered);
    true
}
pub fn reset() {
    unsafe {
        HANDLER = None;
        LAST = None;
        #[cfg(target_arch = "arm")]
        {
            CONNECTION = None;
        }
    }
}
