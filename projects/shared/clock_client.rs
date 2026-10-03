//! Session-bus API for persistent clock activities. Calls are bounded.
use std::cell::RefCell;
thread_local! { static CONNECTION: RefCell<Option<zbus::blocking::Connection>> = const { RefCell::new(None) }; }
pub fn request(command: &str) -> Result<String, String> {
    CONNECTION.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(
                zbus::blocking::connection::Builder::session()
                    .map_err(|e| e.to_string())?
                    .method_timeout(std::time::Duration::from_secs(2))
                    .build()
                    .map_err(|e| e.to_string())?,
            );
        }
        let result = zbus::blocking::Proxy::new(
            cached.as_ref().unwrap(),
            "org.hoki.Clock1",
            "/org/hoki/Clock1",
            "org.hoki.Clock1",
        )
        .and_then(|proxy| proxy.call("Command", &(command,)))
        .map_err(|e| e.to_string());
        if result.is_err() {
            *cached = None;
        }
        result
    })
}
