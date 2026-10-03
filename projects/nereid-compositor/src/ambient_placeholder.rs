//! A Wayland companion belongs to the selected ambient face, not the primary face.
use super::*;

pub struct AmbientPlaceholder {
    pub role: ManagedRole,
    pub face: String,
    pub visible: bool,
    pub presented: bool,
    pub since: Option<std::time::Instant>,
}
impl AmbientPlaceholder {
    pub fn new() -> Self {
        Self { role: ManagedRole::new(RoleId::Watchface, vec![]), face: String::new(),
            visible: false, presented: false, since: None }
    }
}

impl Compositor {
    pub(super) fn present_composited_frame(&mut self, index: usize) -> Result<()> {
        self.proxy.send_frame(self.framebuffers[index].fd, self.display_width,
            self.display_height, self.display_width * 4)
            .context("display submission failed; restarting compositor")?;
        if self.placeholder.visible && self.shell_mode == ShellMode::Watchface
            && self.placeholder.role.buffer.is_some() {
            self.placeholder.presented = true;
        }
        Ok(())
    }

    pub fn visible_watchface(&self) -> &ManagedRole {
        if self.placeholder.visible { &self.placeholder.role } else { &self.watchface }
    }

    pub(super) fn show_placeholder(&mut self, visible: bool) {
        if self.placeholder.visible != visible {
            self.cancel_touches();
            self.placeholder.visible = visible;
            self.placeholder.presented = false;
            self.placeholder.since = visible.then(std::time::Instant::now);
            self.damage = true;
        }
    }

    pub(super) fn prepare_placeholder(&mut self) -> Result<(), String> {
        if self.placeholder.face != self.ambient_face {
            let face = ambient_bundle::load(&self.ambient_face)?;
            self.placeholder.role.kill();
            self.placeholder.role.command = face.placeholder_command(&self.ambient_face);
            self.placeholder.face = self.ambient_face.clone();
            self.placeholder.presented = false;
            self.damage = true;
        }
        ensure_role_running(&mut self.placeholder.role, &self.xdg_runtime, &self.wakeup);
        if self.placeholder.role.buffer.is_none() {
            self.placeholder.presented = false;
        }
        if !self.placeholder.presented && self.placeholder.since.is_some_and(|start| start.elapsed().as_secs() >= 3) {
            return Err("ambient Wayland companion did not present within three seconds".into());
        }
        Ok(())
    }
}
