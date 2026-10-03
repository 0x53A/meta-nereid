#[path = "../../shared/ambient_face.rs"]
mod ambient_face;
use std::io::BufRead;
slint::include_modules!();
#[cfg(test)]
mod ui_tests;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id = std::env::args().nth(1).ok_or("expected ambient face ID")?;
    let face = ambient_face::load(&id).map_err(std::io::Error::other)?;
    let window = MainWindow::new()?;
    window.set_face_name(face.name.into());
    // Drain managed-role messages. This static placeholder has no timers or
    // animations and needs no redraw while hidden or while Sidekick owns the panel.
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            if line.is_err() { break; }
        }
    });
    window.run()?;
    Ok(())
}
