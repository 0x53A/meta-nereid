#[path = "../../shared/sleep_client.rs"]
mod sleep_client;
mod transport;
mod wire;
use slint::ComponentHandle;
use std::io::{BufRead, Write};
slint::include_modules!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--register") {
        use std::os::unix::net::UnixStream;
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
        let mut socket = UnixStream::connect(format!("{runtime}/hoki-compositor.sock"))?;
        socket.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        socket.set_write_timeout(Some(std::time::Duration::from_secs(3)))?;
        socket.write_all(b"set-agent /usr/lib/hoki-argyroneta --role\n")?;
        let mut response = String::new();
        std::io::BufReader::new(socket).read_line(&mut response)?;
        if response.trim() != "ok" {
            return Err(response.into());
        }
        return Ok(());
    }
    let role = args.iter().any(|a| a == "--role");
    let preview = args
        .iter()
        .position(|a| a == "--preview")
        .and_then(|i| args.get(i + 1));
    let capture = args
        .iter()
        .position(|a| a == "--capture")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let ui = AssistantWindow::new()?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut updates) = tokio::sync::mpsc::unbounded_channel::<transport::Update>();
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            tokio::spawn(async move {
                while let Some(update) = updates.recv().await {
                    let _: Result<(), _> = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_status(update.status.into());
                        ui.set_detail(update.detail.into());
                        ui.set_listening(update.listening);
                        ui.set_busy(update.busy);
                    });
                }
            });
            transport::worker(rx, events).await;
        });
    });
    let weak = ui.as_weak();
    let action_tx = tx.clone();
    let is_preview = preview.is_some();
    ui.on_action(move || {
        if is_preview {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            let command = if ui.get_listening() {
                transport::Control::Send
            } else if ui.get_busy() {
                transport::Control::Cancel
            } else {
                transport::Control::Start
            };
            let _ = action_tx.send(command);
        }
    });
    let close_tx = tx.clone();
    ui.on_dismiss(move || {
        let _ = close_tx.send(transport::Control::Cancel);
        if role {
            println!("dismiss");
            let _ = std::io::stdout().flush();
        } else {
            let _ = slint::quit_event_loop();
        }
    });
    if role && !is_preview {
        let input_tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line.as_deref() {
                    Ok("activate") => {
                        let _ = input_tx.send(transport::Control::Start);
                    }
                    Ok("cancel") | Ok("visibility:hidden") => {
                        let _ = input_tx.send(transport::Control::Cancel);
                    }
                    _ => {}
                }
            }
            let _ = input_tx.send(transport::Control::Cancel);
            let _ = slint::quit_event_loop();
        });
    }
    if let Some(mode) = preview {
        ui.set_status(
            if mode == "listening" {
                "Listening"
            } else if mode == "error" {
                "Unavailable"
            } else {
                "Reply"
            }
            .into(),
        );
        ui.set_detail(if mode=="listening"{"Speak now. Tap Send when finished."}else if mode=="error"{"Open Argyroneta’s watch settings on your phone to enable the connection."}else{"Your ten minute timer is running. This longer response checks wrapping on the round screen."}.into());
        ui.set_listening(mode == "listening");
        ui.set_busy(mode == "listening");
    }
    let screenshot = slint::Timer::default();
    if let Some(path) = capture {
        let weak = ui.as_weak();
        screenshot.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_secs(2),
            move || {
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let ui = weak.upgrade().ok_or("Window closed")?;
                    let pixels = ui.window().take_snapshot()?;
                    image::save_buffer(
                        &path,
                        pixels.as_bytes(),
                        pixels.width(),
                        pixels.height(),
                        image::ColorType::Rgba8,
                    )?;
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("Capture failed: {e}");
                }
                let _ = slint::quit_event_loop();
            },
        );
    }
    let result = ui.run();
    let _ = tx.send(transport::Control::Shutdown);
    let _ = worker.join();
    result?;
    Ok(())
}
