//! Standard FD-scoped Linux suspend inhibition for application work.
pub fn acquire(who: &str, why: &str) -> Result<zbus::zvariant::OwnedFd, String> {
    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| e.to_string())?
        .method_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .map_err(|e| e.to_string())?;
    proxy
        .call("Inhibit", &("sleep", who, why, "block"))
        .map_err(|e| e.to_string())
}
