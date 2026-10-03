//! Native headless watch endpoint; shared library owns all Pict authorization.
use anyhow::Result;
use pict_host::{control, core, resources, server};
use std::sync::{Arc, Mutex};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|v| v == "--ctl") {
        return control::request(&args[2..]).await;
    }
    anyhow::ensure!(args.len() == 1, "Usage: nereid-pict [--ctl COMMAND ...]");
    let _resources = resources::init()?;
    let port = std::env::var("PICT_PORT")
        .unwrap_or_else(|_| "8787".into())
        .parse::<u16>()?;
    let origin =
        std::env::var("PICT_ORIGIN").unwrap_or_else(|_| format!("http://127.0.0.1:{port}"));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let view = Arc::new(Mutex::new(core::View::default()));
    let ctl = control::Control::bind()?;
    let engine =
        core::Core::load_with_backend(view.clone(), events, Arc::new(nereid_pict::Nereid)).await?;
    let mut engine = tokio::spawn(engine.run(rx, event_rx));
    let ctl = tokio::spawn(ctl.run(tx.clone(), view));
    let server = tokio::spawn(server::serve(tx.clone(), origin, port));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::pin!(server);
    tokio::select! {
        _=tokio::signal::ctrl_c()=>{},
        _=terminate.recv()=>{},
        result=&mut server=>{eprintln!("HTTP server stopped: {result:?}");},
        result=&mut engine=>{result?;ctl.abort();server.abort();return Ok(());}
    }
    let _ = tx.send(core::Command::Quit);
    engine.await?;
    ctl.abort();
    server.abort();
    Ok(())
}
