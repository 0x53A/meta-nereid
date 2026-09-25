//! Wayland protocol handling via smithay 0.7.
//!
//! Registers wl_compositor, wl_shm, xdg_wm_base, wl_seat, and
//! zwlr_layer_shell_v1 globals. Tracks mapped toplevel and layer surfaces.

use smithay::delegate_compositor;
use smithay::delegate_layer_shell;
use smithay::delegate_output;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_shell;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::Transform;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    with_states,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use tracing::{info, warn};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::protocol::wl_buffer::WlBuffer;
use wayland_server::protocol::wl_callback::WlCallback;
use wayland_server::protocol::wl_output::WlOutput;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, Display, Resource};

use crate::Compositor;

/// Per-client state required by smithay.
pub struct ClientState {
    pub compositor: CompositorClientState,
}

impl wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        client_id: wayland_server::backend::ClientId,
        reason: wayland_server::backend::DisconnectReason,
    ) {
        info!(?client_id, ?reason, "Client disconnected");
    }
}

/// Wayland protocol state.
pub struct WaylandState {
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub seat_state: SeatState<Compositor>,
    pub seat: Seat<Compositor>,
    pub output: Output,
}

impl WaylandState {
    pub fn new(display: &Display<Compositor>, width: u32, height: u32) -> Self {
        let dh = display.handle();
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_touch();
        seat.add_pointer();
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("keyboard init");

        // Create wl_output global — required by winit/sctk for proper input routing
        let output = Output::new(
            "hoki-display".into(),
            PhysicalProperties {
                size: (33, 33).into(), // 1.28" ≈ 33mm
                subpixel: Subpixel::Unknown,
                make: "Fossil".into(),
                model: "Gen 6".into(),
            },
        );
        let mode = OutputMode {
            size: (width as i32, height as i32).into(),
            refresh: 45000, // 45 Hz
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let global_id = output.create_global::<Compositor>(&dh);
        info!(?global_id, "wl_output global created");

        Self {
            compositor_state: CompositorState::new::<Compositor>(&dh),
            shm_state: ShmState::new::<Compositor>(&dh, vec![]),
            xdg_shell_state: XdgShellState::new::<Compositor>(&dh),
            layer_shell_state: WlrLayerShellState::new::<Compositor>(&dh),
            seat_state,
            seat,
            output,
        }
    }
}

// --- smithay handler implementations ---

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.wayland.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("client missing ClientState")
            .compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Check if it's a toplevel surface
        let toplevel_surfaces: Vec<ToplevelSurface> = self
            .wayland
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .cloned()
            .collect();

        for toplevel in &toplevel_surfaces {
            if toplevel.wl_surface() == surface {
                self.handle_toplevel_commit(surface, toplevel);
                return;
            }
        }

        // Check if it's a layer surface
        if let Some(idx) = self
            .layer_surfaces
            .iter()
            .position(|ls| ls.surface.wl_surface() == surface)
        {
            self.handle_layer_commit(surface, idx);
        }
    }
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.wayland.shm_state
    }
}

impl SeatHandler for Compositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.wayland.seat_state
    }
}

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.wayland.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Match the Wayland client to a role by comparing PIDs
        let client_pid = surface
            .wl_surface()
            .client()
            .and_then(|c: Client| c.get_credentials(&self.display_handle).ok())
            .map(|creds| creds.pid as u32);

        if let Some(pid) = client_pid {
            if self.watchface.child_pid == Some(pid) && self.watchface.surface.is_none() {
                self.watchface.surface = Some(surface.wl_surface().clone());
                info!(pid, "Claimed toplevel as watchface surface (by PID)");
            } else if self.launcher.child_pid == Some(pid) && self.launcher.surface.is_none() {
                self.launcher.surface = Some(surface.wl_surface().clone());
                info!(pid, "Claimed toplevel as launcher surface (by PID)");
            } else if self.settings.child_pid == Some(pid) && self.settings.surface.is_none() {
                self.settings.surface = Some(surface.wl_surface().clone());
                info!(pid, "Claimed toplevel as settings surface (by PID)");
            }
        }
        if !self.is_role_surface(surface.wl_surface()) {
            self.app_surfaces.push(crate::AppSurface {
                surface: surface.wl_surface().clone(),
                buffer: None,
            });
        }
        // Tell the client this surface is on our output (required by winit/sctk for input)
        self.wayland.output.enter(surface.wl_surface());
        self.configure_toplevel_fullscreen(&surface);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        self.configure_toplevel_fullscreen(&surface);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.configure_toplevel_fullscreen(&surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl = surface.wl_surface();
        self.cancel_surface_touches(wl);
        self.app_surfaces.retain(|entry| &entry.surface != wl);
        self.frame_callbacks.retain(|(s, _)| s != wl);
        self.app_unmapped(wl);
        for role in [&mut self.watchface, &mut self.launcher, &mut self.settings] {
            if role.is_surface(wl) {
                role.surface = None;
                role.buffer = None;
                self.damage = true;
            }
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        warn!("Popup surfaces not yet supported");
    }

    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl WlrLayerShellHandler for Compositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.wayland.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // Configure the layer surface to fill the display
        surface.with_pending_state(|state| {
            state.size = Some((self.display_width as i32, self.display_height as i32).into());
        });
        surface.send_configure();
        // Tell the client this surface is on our output
        self.wayland.output.enter(surface.wl_surface());

        info!(
            ?layer,
            namespace,
            "New layer surface configured {}x{}",
            self.display_width,
            self.display_height,
        );

        self.layer_surfaces.push(LayerEntry {
            surface,
            layer,
            namespace,
            has_content: false,
            pending_buffer: None,
            frame_callbacks: Vec::new(),
            visible: true,
        });

        // Keep sorted by z-order
        self.sort_layer_surfaces();
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let wl_surf = surface.wl_surface().clone();

        if let Some(idx) = self
            .layer_surfaces
            .iter()
            .position(|ls| ls.surface == surface)
        {
            let entry = self.layer_surfaces.remove(idx);
            info!(namespace = entry.namespace, "Layer surface destroyed");
        }

        // Remove only callbacks belonging to the destroyed layer.
        self.frame_callbacks
            .retain(|(surface, _)| surface != &wl_surf);
        self.damage = true;
        if self
            .touch_targets
            .values()
            .any(|(surface, _)| surface == &wl_surf)
        {
            self.cancel_touches();
        }

        // If this was the touch-focused surface, clear focus
        if let Some(ref focused) = self.focused_surface {
            if *focused == wl_surf {
                self.focused_surface = None;
            }
        }
    }
}

impl OutputHandler for Compositor {}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_seat!(Compositor);
delegate_layer_shell!(Compositor);
delegate_output!(Compositor);

impl Compositor {
    fn configure_toplevel_fullscreen(&self, surface: &ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.size = Some((self.display_width as i32, self.display_height as i32).into());
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        info!(
            "Toplevel configured fullscreen {}x{}",
            self.display_width, self.display_height
        );
    }

    fn handle_toplevel_commit(&mut self, surface: &WlSurface, _toplevel: &ToplevelSurface) {
        let (assignment, frame_callbacks) = extract_surface_state(surface);

        self.frame_callbacks
            .extend(frame_callbacks.into_iter().map(|cb| (surface.clone(), cb)));

        if let Some(assignment) = assignment {
            match assignment {
                BufferAssignment::NewBuffer(buffer) => {
                    match extract_shm_buffer(&buffer) {
                        Ok(buf) => {
                            if self.watchface.is_surface(surface) {
                                self.watchface.buffer = Some(buf);
                            } else if self.launcher.is_surface(surface) {
                                self.launcher.buffer = Some(buf);
                            } else if self.settings.is_surface(surface) {
                                self.settings.buffer = Some(buf);
                            } else {
                                if let Some(entry) =
                                    self.app_surfaces.iter_mut().find(|e| &e.surface == surface)
                                {
                                    let newly_mapped = entry.buffer.is_none();
                                    entry.buffer = Some(buf);
                                    if newly_mapped && self.display_on {
                                        self.cancel_touches();
                                        self.focused_surface = Some(surface.clone());
                                        self.switch_mode(crate::ShellMode::App);
                                    }
                                }
                            }
                            if self.surface_visible(surface) {
                                self.damage = true;
                            }
                        }
                        Err(e) => warn!("Failed to read toplevel shm buffer: {:?}", e),
                    }
                    buffer.release();
                }
                BufferAssignment::Removed => {
                    self.cancel_surface_touches(surface);
                    if self.watchface.is_surface(surface) {
                        self.watchface.buffer = None;
                    } else if self.launcher.is_surface(surface) {
                        self.launcher.buffer = None;
                    } else if self.settings.is_surface(surface) {
                        self.settings.buffer = None;
                    } else {
                        if let Some(entry) =
                            self.app_surfaces.iter_mut().find(|e| &e.surface == surface)
                        {
                            entry.buffer = None;
                        }
                        self.app_unmapped(surface);
                    }
                    self.damage = true;
                }
            }
        }
    }

    fn handle_layer_commit(&mut self, surface: &WlSurface, idx: usize) {
        let (assignment, frame_callbacks) = extract_surface_state(surface);

        self.frame_callbacks
            .extend(frame_callbacks.into_iter().map(|cb| (surface.clone(), cb)));

        if let Some(assignment) = assignment {
            match assignment {
                BufferAssignment::NewBuffer(buffer) => {
                    match extract_shm_buffer(&buffer) {
                        Ok(buf) => {
                            info!(
                                width = buf.width,
                                height = buf.height,
                                namespace = self.layer_surfaces[idx].namespace,
                                "Layer surface buffer committed"
                            );
                            self.layer_surfaces[idx].pending_buffer = Some(buf);
                            self.layer_surfaces[idx].has_content = true;
                            self.damage = true;
                        }
                        Err(e) => warn!("Failed to read layer shm buffer: {:?}", e),
                    }
                    buffer.release();
                }
                BufferAssignment::Removed => {
                    self.cancel_surface_touches(surface);
                    self.layer_surfaces[idx].pending_buffer = None;
                    self.layer_surfaces[idx].has_content = false;
                    self.damage = true;
                }
            }
        }
    }

    /// Sort layer surfaces by z-order (Background < Bottom < Top < Overlay).
    fn sort_layer_surfaces(&mut self) {
        self.layer_surfaces.sort_by_key(|ls| match ls.layer {
            Layer::Background => 0,
            Layer::Bottom => 1,
            Layer::Top => 2,
            Layer::Overlay => 3,
        });
    }
}

/// Extract buffer assignment and frame callbacks from a committed surface.
fn extract_surface_state(surface: &WlSurface) -> (Option<BufferAssignment>, Vec<WlCallback>) {
    with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceAttributes>();
        let attrs = guard.current();
        let buffer = attrs.buffer.take();
        let callbacks = std::mem::take(&mut attrs.frame_callbacks);
        (buffer, callbacks)
    })
}

/// Read pixel data from a wl_shm buffer.
fn extract_shm_buffer(
    buffer: &WlBuffer,
) -> Result<SurfaceBuffer, smithay::wayland::shm::BufferAccessError> {
    use wayland_server::protocol::wl_shm::Format;
    smithay::wayland::shm::with_buffer_contents(buffer, |ptr, len, data| {
        let (swap_rb, has_alpha) = match data.format {
            Format::Xrgb8888 => (true, false),
            Format::Argb8888 => (true, true),
            _ => return None,
        };
        unsafe {
            SurfaceBuffer::copy_from_pool(
                ptr,
                len,
                data.offset,
                data.width,
                data.height,
                data.stride,
                swap_rb,
                has_alpha,
            )
        }
    })?
    .ok_or(smithay::wayland::shm::BufferAccessError::BadMap)
}

pub use crate::pixels::SurfaceBuffer;

/// Tracked layer surface with its render state.
pub struct LayerEntry {
    pub surface: LayerSurface,
    pub layer: Layer,
    pub namespace: String,
    /// Whether this layer has ever received a buffer commit.
    pub has_content: bool,
    pub pending_buffer: Option<SurfaceBuffer>,
    pub frame_callbacks: Vec<WlCallback>,
    pub visible: bool,
}

/// Generic desktop keymaps map evdev F13/F14 to multimedia symbols. Our client
/// contract explicitly uses F13/F14 for the watch pushers; do not inherit the
/// development host's keyboard model or layout for these keys.
pub const WATCH_KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compatibility { include "complete" };
    xkb_symbols {
        include "pc+us+inet(evdev)"
        replace key <FK13> { [ F13 ] };
        replace key <FK14> { [ F14 ] };
    };
};"#;

#[cfg(test)]
mod watch_keymap_tests {
    #[test]
    fn pushers_produce_the_client_contract_symbols() {
        use smithay::input::keyboard::xkb;
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(&context, super::WATCH_KEYMAP.into(),
            xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS).unwrap();
        let state = xkb::State::new(&keymap);
        assert_eq!(state.key_get_one_sym(xkb::Keycode::new(183 + 8)), xkb::Keysym::new(xkb::keysyms::KEY_F13));
        assert_eq!(state.key_get_one_sym(xkb::Keycode::new(184 + 8)), xkb::Keysym::new(xkb::keysyms::KEY_F14));
    }
}
