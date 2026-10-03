use nereid_auth::{backend::Native, service::AuthService, Error, BUS, PATH};
use std::{path::PathBuf, sync::Arc};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    // No secrets until dump/swap protection succeeds. systemd grants memlock.
    unsafe {
        libc::umask(0o077);
        if libc::geteuid() != 0
            || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
            || libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) != 0
        {
            return Err("Cannot protect authentication memory".into());
        }
    }
    let backend = Native::new(
        PathBuf::from("/var/lib/nereid-auth"),
        PathBuf::from("/usr/libexec/nereid-auth/backend.py"),
    )?;
    let backend = Arc::new(backend);
    let connection = zbus::connection::Builder::system()?
        .name(BUS)?
        .serve_at(PATH, AuthService::new(backend.clone()))?
        .build()
        .await?;
    let interface = connection
        .object_server()
        .interface::<_, AuthService>(PATH)
        .await?;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _=interval.tick()=>interface.get().await.expire(),
            _=terminate.recv()=>break,
        }
    }
    // Drop the public bus name before cleanup, so the compositor locks while
    // we wait for any pending native request and ordinary unmount/close.
    let _ = connection.release_name(BUS).await;
    interface.get().await.shutdown().await?;
    drop(interface);
    drop(connection);
    Ok(())
}
