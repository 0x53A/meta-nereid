mod sleep;
mod ambient_placeholder;
mod auth;
use ambient_placeholder::AmbientPlaceholder;
#[path = "../../shared/ambient_face.rs"]
mod ambient_bundle;
#[path = "../../shared/sleep_client.rs"]
mod sleep_client;
mod compose;
mod capture;
mod desktop;
mod output_management;
mod config_file;
mod gesture;
mod input;
mod pixels;
mod proxy;
mod role_input;
mod crown_press;
mod wakeup;
mod wayland;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};

use std::os::unix::io::AsFd;

use anyhow::{Context, Result};
use nix::sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags, EpollTimeout};
use smithay::backend::input::KeyState;
use smithay::backend::input::TouchSlot;
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{AxisFrame, MotionEvent as PointerMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::shell::wlr_layer::Layer;
use tracing::{info, warn};
use wayland_server::protocol::wl_callback::WlCallback;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Display, DisplayHandle, ListeningSocket, Resource};

use wayland::LayerEntry;

#[zbus::proxy(
    interface = "org.hoki.power.Manager",
    default_service = "org.hoki.power",
    default_path = "/org/hoki/power",
    gen_async = false
)]
trait PowerManager {
    fn request_cores(&self, cores: u32, duration_secs: u32, owner: &str) -> zbus::Result<String>;
}

/// Watch button keycodes (from kernel bg_rsb.c).
const KEY_VOLUMEDOWN: u32 = 114; // bottom pusher
const KEY_VOLUMEUP: u32 = 115; // top pusher
const KEY_POWER: u32 = 116; // crown press

/// Evdev codes for F13/F14 — used to remap side pushers for Wayland forwarding.
/// Slint doesn't support volume key constants, so we forward side buttons as
/// F13 (top) / F14 (bottom) which Slint can handle via Key.F13 / Key.F14.
const KEY_F13: u32 = 183;
const KEY_F14: u32 = 184;

/// Shell mode — which surface slot is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMode {
    /// Authentication lock screen is the only visible shell surface.
    LockScreen,
    /// Watchface role is visible.
    Watchface,
    /// Launcher role is visible.
    Launcher,
    /// Settings role is visible.
    Settings,
    Agent,
    /// A toplevel app is in the foreground.
    App,
}

/// Identifies a managed role slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleId {
    LockScreen,
    Watchface,
    Launcher,
    Settings,
    Agent,
    Overlay,
}

/// A managed process that fills a compositor role (watchface or launcher).
struct ManagedRole {
    id: RoleId,
    command: Vec<String>,
    process: Option<Child>,
    /// PID of the spawned child, used to match Wayland client to role.
    child_pid: Option<u32>,
    stdin: Option<role_input::RoleInput>,
    reported_visible: Option<bool>,
    reported_locked: Option<bool>,
    rx: Option<mpsc::Receiver<String>>,
    surface: Option<WlSurface>,
    buffer: Option<wayland::SurfaceBuffer>,
    /// When the process last exited (for respawn backoff).
    last_exit: Option<std::time::Instant>,
}

impl ManagedRole {
    fn new(id: RoleId, command: Vec<String>) -> Self {
        Self {
            id,
            command,
            process: None,
            child_pid: None,
            stdin: None,
            reported_visible: None,
            reported_locked: None,
            rx: None,
            surface: None,
            buffer: None,
            last_exit: None,
        }
    }

    fn send(&mut self, msg: &str) {
        if let Some(stdin) = &mut self.stdin {
            if let Err(e) = stdin.send(msg) {
                self.input_failed(e);
            }
        }
    }

    fn report_visibility(&mut self, visible: bool) {
        if self.stdin.is_some() && self.reported_visible != Some(visible) {
            self.send(if visible {
                "visibility:visible"
            } else {
                "visibility:hidden"
            });
            if self.stdin.is_some() {
                self.reported_visible = Some(visible);
            }
        }
    }

    fn report_lock_state(&mut self, locked: bool) {
        if self.stdin.is_some() && self.reported_locked != Some(locked) {
            self.send(if locked { "lock-state:locked" } else { "lock-state:unlocked" });
            if self.stdin.is_some() {
                self.reported_locked = Some(locked);
            }
        }
    }

    fn reply_to_lock_state_query(&mut self, message: &str, locked: bool) -> bool {
        if message != "get-lock-state" {
            return false;
        }
        self.send(if locked { "lock-state:locked" } else { "lock-state:unlocked" });
        true
    }

    fn flush_input(&mut self) {
        if let Some(stdin) = &mut self.stdin {
            if let Err(e) = stdin.flush() {
                self.input_failed(e);
            }
        }
    }

    fn input_failed(&mut self, error: std::io::Error) {
        warn!(role = ?self.id, %error, "Restarting unresponsive role");
        self.kill();
        self.last_exit = Some(std::time::Instant::now());
    }

    fn is_surface(&self, surface: &WlSurface) -> bool {
        self.surface.as_ref().map_or(false, |s| s == surface)
    }

    /// Check if the process is still alive. Returns true if running.
    fn check_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(?status, role = ?self.id, "Role process exited");
                    self.process = None;
                    self.stdin = None;
                    self.reported_visible = None;
                    self.reported_locked = None;
                    // Keep the receiver until replacement: the stdout reader may
                    // still deliver the exiting process's final commands.
                    self.child_pid = None;
                    self.surface = None;
                    self.buffer = None;
                    self.last_exit = Some(std::time::Instant::now());
                    false
                }
                _ => true,
            }
        } else {
            false
        }
    }

    fn drain_messages(&self) -> Vec<String> {
        self.rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    fn kill(&mut self) {
        if let Some(ref mut child) = self.process {
            child.kill().ok();
            child.wait().ok();
        }
        self.process = None;
        self.child_pid = None;
        self.stdin = None;
        self.reported_visible = None;
        self.reported_locked = None;
        self.rx = None;
        self.surface = None;
        self.buffer = None;
        self.last_exit = None; // intentional kill, no backoff needed
    }
}

/// Control message from the control socket thread.
enum CtlMessage {
    ScreenOff,
    SetRole { id: RoleId, command: Vec<String> },
    LaunchApp { args: Vec<String>, return_mode: ShellMode },
    AuthState(auth::AuthState),
}

struct AppSurface {
    surface: WlSurface,
    buffer: Option<wayland::SurfaceBuffer>,
}

/// Top-level compositor state.
pub struct Compositor {
    proxy: proxy::ProxyClient,
    framebuffers: [compose::MemfdBuffer; 2],
    write_buf_idx: usize,
    input_mgr: input::InputManager,
    wayland: wayland::WaylandState,
    gesture: gesture::GestureRecognizer,
    /// Watchface role slot.
    watchface: ManagedRole,
    /// Authentication lock screen renderer, trusted only by its launched PID.
    lock_screen: ManagedRole,
    /// Launcher role slot.
    launcher: ManagedRole,
    /// Settings role slot.
    settings: ManagedRole,
    agent: ManagedRole,
    overlay: ManagedRole,
    agent_return: ShellMode,
    crown_press: crown_press::CrownPress,
    /// Current toplevel surface buffer (persists until replaced or removed).
    app_surfaces: Vec<AppSurface>,
    /// Frame callbacks to fire after rendering.
    frame_callbacks: Vec<(WlSurface, WlCallback)>,
    last_callback: std::time::Instant,
    touch_targets: std::collections::HashMap<u32, (WlSurface, MotionEvent)>,
    /// Touch sequences that began on the locked watchface are discarded through Up/Cancel.
    swallowed_touch_slots: std::collections::HashSet<u32>,
    /// Currently focused surface (for touch forwarding).
    focused_surface: Option<WlSurface>,
    /// Active layer surfaces, sorted by z-order.
    layer_surfaces: Vec<LayerEntry>,
    display_width: u32,
    display_height: u32,
    display_on: bool,
    running: bool,
    /// XDG_RUNTIME_DIR for spawning children.
    xdg_runtime: String,
    /// True when any surface has new content or shell state changed since last render.
    damage: bool,
    /// Seconds of inactivity before display turns off (0 = disabled).
    display_timeout_secs: u64,
    ambient: bool,
    manual_off: bool,
    sleep_enabled: bool,
    sleep_generation: u64,
    activity_revision: u64,
    sleep_bridge: sleep::Bridge,
    interactive_inhibitor: Option<sleep_client::Client>,
    ambient_face: String,
    secondary_only: bool,
    placeholder: AmbientPlaceholder,
    ambient_failed: bool,
    /// Last input activity timestamp.
    last_activity: std::time::Instant,
    /// Current shell mode.
    shell_mode: ShellMode,
    /// Shell mode to restore after the authentication service unlocks.
    lock_return_mode: ShellMode,
    /// Whether a lock-screen renderer is configured.
    lock_enabled: bool,
    auth_monitor_active: Arc<std::sync::atomic::AtomicBool>,
    /// Last property state received from the current Auth1 service owner.
    auth_owner_available: bool,
    auth_property_locked: bool,
    auth_owner: Option<String>,
    /// Keep startup blank until the monitor has returned one Auth1 result.
    auth_state_resolved: bool,
    /// Fail-closed effective lock state. Renderer state never changes this.
    locked: bool,
    /// Selects the display-only primary watchface while ShellMode remains LockScreen.
    locked_watchface_selected: bool,
    /// Shell role to restore after an app launched by that role closes.
    app_return_mode: ShellMode,
    /// Display handle for Wayland client credential lookups.
    display_handle: DisplayHandle,
    /// Receiver for control socket messages.
    ctl_rx: mpsc::Receiver<CtlMessage>,
    ctl_tx: mpsc::Sender<CtlMessage>,
    wakeup: Arc<wakeup::Wakeup>,
    apps: Vec<Child>,
}

impl Compositor {
    fn new(
        display: &Display<Self>,
        ctl_tx: mpsc::Sender<CtlMessage>,
        ctl_rx: mpsc::Receiver<CtlMessage>,
        wakeup: Arc<wakeup::Wakeup>,
    ) -> Result<Self> {
        info!("Connecting to HWC proxy...");
        let mut proxy = proxy::ProxyClient::connect().context("HWC proxy connect")?;

        let width = proxy.display_width;
        let height = proxy.display_height;
        info!("Display: {}x{}", width, height);

        // Ensure display is on
        proxy.set_power(true).context("Initial power on")?;

        info!("Allocating framebuffers...");
        let fb0 = compose::MemfdBuffer::new(width, height).context("framebuffer 0")?;
        let fb1 = compose::MemfdBuffer::new(width, height).context("framebuffer 1")?;

        info!("Initializing input...");
        let input_mgr = if let Some(path) = std::env::var_os("HOKI_SIM_INPUT") {
            input::InputManager::simulated(std::path::Path::new(&path), width, height)
        } else {
            input::InputManager::new_from_udev("seat0", width, height)
        }.context("Input init")?;

        info!("Initializing Wayland globals...");
        let wayland = wayland::WaylandState::new(display, width, height);

        let gesture = gesture::GestureRecognizer::new(width, height);

        let config = read_config();
        let initial_mode = if config.watchface.is_empty() {
            ShellMode::Launcher
        } else {
            ShellMode::Watchface
        };
        let lock_enabled = !config.lock_screen.is_empty();

        let xdg_runtime =
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());

        Ok(Self {
            proxy,
            framebuffers: [fb0, fb1],
            write_buf_idx: 0,
            input_mgr,
            wayland,
            gesture,
            lock_screen: ManagedRole::new(RoleId::LockScreen, config.lock_screen),
            watchface: ManagedRole::new(RoleId::Watchface, config.watchface),
            launcher: ManagedRole::new(RoleId::Launcher, config.launcher),
            settings: ManagedRole::new(RoleId::Settings, config.settings),
            agent: ManagedRole::new(RoleId::Agent, config.agent),
            overlay: ManagedRole::new(RoleId::Overlay, config.overlay),
            agent_return: ShellMode::Watchface,
            crown_press: Default::default(),
            app_surfaces: Vec::new(),
            frame_callbacks: Vec::new(),
            last_callback: std::time::Instant::now(),
            touch_targets: std::collections::HashMap::new(),
            swallowed_touch_slots: std::collections::HashSet::new(),
            focused_surface: None,
            layer_surfaces: Vec::new(),
            display_width: width,
            display_height: height,
            display_on: true,
            running: true,
            xdg_runtime,
            damage: true,
            display_timeout_secs: config.display_timeout,
            ambient: false, manual_off:false, sleep_enabled:false, sleep_generation:0,activity_revision:0,
            sleep_bridge:sleep::Bridge::new(), interactive_inhibitor:None, ambient_face:String::new(), secondary_only:false, placeholder:AmbientPlaceholder::new(), ambient_failed:false,
            last_activity: std::time::Instant::now(),
            shell_mode: if lock_enabled { ShellMode::LockScreen } else { initial_mode },
            lock_return_mode: initial_mode,
            lock_enabled,
            auth_monitor_active: Arc::new(std::sync::atomic::AtomicBool::new(lock_enabled)),
            auth_owner_available: false,
            auth_property_locked: true,
            auth_owner: None,
            auth_state_resolved: !lock_enabled,
            locked: lock_enabled,
            locked_watchface_selected: false,
            app_return_mode: ShellMode::Launcher,
            display_handle: display.handle(),
            ctl_rx,
            ctl_tx,
            wakeup,
            apps: Vec::new(),
        })
    }

    /// Preserve applications and their Wayland surfaces across display changes.
    fn note_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
        self.activity_revision = self.activity_revision.wrapping_add(1);
    }

    fn set_display_power(&mut self, on: bool) {
        if self.manual_off == on {
            self.activity_revision = self.activity_revision.wrapping_add(1);
        }
        self.manual_off=!on;
        if on {self.ambient_failed=false;}
        self.change_display(if on {"interactive"} else {"off"});
    }

    fn change_display(&mut self, target:&str) {
        let on=target=="interactive";let ambient=target=="ambient";
        // Acquire before exposing foreground work; a queued suspend must finish
        // or cancel before powerd acknowledges this promise to remain awake.
        if on && self.sleep_enabled && self.interactive_inhibitor.is_none() {
            let grant=(|| -> std::io::Result<sleep_client::Client> {
                let mut client=sleep_client::Client::connect()?;
                client.inhibit(true,false,"interactive compositor")?;
                Ok(client)
            })();
            match grant {
                Ok(client)=>self.interactive_inhibitor=Some(client),
                Err(error)=>{
                    warn!(?error,"interactive wake could not acquire inhibitor");
                    self.running=false;
                    return;
                }
            }
        }
        if self.display_on==on && self.ambient==ambient {return;}
        if ambient && self.ambient_failed {return;}
        let mode=if ambient {3} else if on {2} else {0};
        if let Err(error)=self.proxy.set_display(mode,&self.ambient_face) {
            warn!(?error,"display transition failed; restoring interactive UI");
            if self.proxy.set_display(2,"").is_err(){self.running=false;}
            self.display_on=true;self.ambient=false;self.damage=true;self.ambient_failed=true;
            return;
        }
        self.cancel_touches();self.display_on=on;self.ambient=ambient;self.damage=on;
        // Release only after physical handoff. The subsequent UI report must
        // acknowledge that handoff before the coordinator can actually sleep.
        if !on {self.interactive_inhibitor=None;}
        let event=if ambient {"display-ambient"} else if on {"display-on"} else {"display-off"};
        std::thread::spawn(move || {notify_powerd(event);});
    }

    fn reconcile_sleep(&mut self) {
        let locked = self.is_locked();
        if locked {
            self.show_placeholder(false);
            if self.ambient {
                self.change_display("off");
            }
        }
        let reported=if self.display_on {"interactive"} else if self.ambient {"ambient"} else {"off"};
        let watchface_foreground = self.shell_mode == ShellMode::Watchface
            || (locked && self.locked_watchface_selected);
        let reply=self.sleep_bridge.exchange(serde_json::json!({"command":"ui",
            "activity_revision":self.activity_revision,"idle":self.last_activity.elapsed().as_secs_f64(),"foreground":!watchface_foreground,
            "display":reported,"generation":self.sleep_generation,"manual_off":self.manual_off,"handoff_failed":self.ambient_failed}));
        self.sleep_enabled=reply["config"]["enabled"]==true;
        if !self.sleep_enabled {
            self.show_placeholder(false);
            self.interactive_inhibitor=None;
            if self.ambient {self.change_display(if locked {"off"} else {"interactive"});}
            return;
        }
        let generation=reply["generation"].as_u64().unwrap_or(0);
        if generation!=self.sleep_generation {self.ambient_failed=false;}
        let face=reply["config"]["ambient_face"].as_str().unwrap_or("hoki-digital").to_string();
        self.secondary_only=reply["config"]["face_mode"]=="secondary";
        if self.ambient && face!=self.ambient_face {self.change_display("interactive");}
        self.ambient_face=face;
        self.sleep_generation=generation;
        let policy_target=reply["display"].as_str().unwrap_or("interactive");
        if locked && policy_target == "ambient" {
            // The ambient handoff bypasses compositor lock rendering.
            self.change_display("off");
            return;
        }
        let target=policy_target;
        let want_placeholder = !locked && !self.ambient && !self.ambient_failed && !self.manual_off
            && self.shell_mode == ShellMode::Watchface
            && (target == "ambient" || (self.secondary_only && reply["_stale"] == true));
        self.show_placeholder(want_placeholder);
        if want_placeholder {
            if !self.display_on { self.change_display("interactive"); }
            if !self.running { return; }
            if let Err(error) = self.prepare_placeholder() {
                warn!(%error, "Ambient companion unavailable; keeping interactive fallback");
                self.ambient_failed = true;
                self.show_placeholder(false);
                return;
            }
        }
        // Stale replies cannot start a handoff. A fresh secondary decision need
        // not wait one second, but must wait for the companion's submitted frame
        // and any pending crown short/long-press decision.
        let secondary_ready = self.secondary_only && target == "ambient" && reply["_stale"] != true;
        if target == "ambient" && !self.ambient {
            if self.ambient_failed || !self.placeholder.presented
                || self.crown_press.remaining_ms(std::time::Instant::now()).is_some() { return; }
        }
        if target!="interactive" && !secondary_ready && !self.manual_off && self.last_activity.elapsed().as_millis()<1000 {return;}
        self.change_display(target);
        if self.ambient || self.ambient_failed { self.show_placeholder(false); }
    }

    fn activate_agent(&mut self) {
        if self.agent.command.is_empty() { return; }
        if self.shell_mode != ShellMode::Agent { self.agent_return = self.shell_mode; }
        self.switch_mode(ShellMode::Agent);
        if self.running {self.agent.send("activate");}
    }

    fn dismiss_agent(&mut self) {
        if self.shell_mode != ShellMode::Agent { return; }
        let target = if self.agent_return == ShellMode::App && !self.has_live_toplevels() {
            self.app_return_target()
        } else { self.agent_return };
        self.switch_mode(target);
    }

    /// Switch shell mode, lazily spawning the target role if needed.
    fn switch_mode(&mut self, mode: ShellMode) {
        if self.is_locked() && mode != ShellMode::LockScreen {
            return;
        }
        if self.sleep_enabled && mode!=ShellMode::Watchface {
            self.set_display_power(true);
            if !self.running {return;}
        }
        if self.shell_mode != mode {
            self.activity_revision = self.activity_revision.wrapping_add(1);
            info!(from = ?self.shell_mode, to = ?mode, "Shell mode changed");
            self.cancel_touches();
            self.damage = true;
        }
        if self.shell_mode == ShellMode::Agent && mode != ShellMode::Agent {
            self.agent.send("cancel");
        }
        self.shell_mode = mode;
        let xdg = self.xdg_runtime.clone();
        match mode {
            ShellMode::LockScreen => {
                if self.auth_state_ready() {
                    ensure_role_running(&mut self.lock_screen, &xdg, &self.wakeup);
                }
            }
            ShellMode::Agent => {
                if self.auth_state_ready() {
                    ensure_role_running(&mut self.agent, &xdg, &self.wakeup);
                }
            }
            ShellMode::Launcher => {
                if self.auth_state_ready() {
                    ensure_role_running(&mut self.launcher, &xdg, &self.wakeup);
                }
            }
            ShellMode::Settings => {
                if self.auth_state_ready() {
                    ensure_role_running(&mut self.settings, &xdg, &self.wakeup);
                }
            }
            _ => {}
        }
    }

    fn is_locked(&self) -> bool {
        self.lock_enabled && self.locked
    }

    /// No user-facing child process starts until the lock monitor reports an
    /// initial result. Without a configured lock role, the legacy startup path
    /// remains immediate.
    fn auth_state_ready(&self) -> bool {
        !self.lock_enabled || self.auth_state_resolved
    }

    fn app_return_target(&self) -> ShellMode {
        if self.app_return_mode == ShellMode::Settings && !self.settings.command.is_empty() {
            ShellMode::Settings
        } else {
            ShellMode::Launcher
        }
    }

    /// Apply only state read from the authenticated system-bus owner. Loss of
    /// the owner or any failed state read is represented as locked=true.
    fn update_auth_state(&mut self, state: auth::AuthState) {
        if self.auth_owner != state.owner {
            info!(owner = ?state.owner, enrolled = state.enrolled, locked = state.locked, "Auth1 owner/state changed");
        }
        self.auth_owner_available = state.owner.is_some();
        self.auth_property_locked = state.locked || !self.auth_owner_available;
        self.auth_owner = state.owner;
        self.auth_state_resolved = true;
        if self.auth_owner_available && !state.enrolled && !state.locked {
            // A healthy, explicitly unconfigured Auth1 instance needs no lock
            // renderer. Clear any stale renderer left by an earlier state.
            self.lock_screen.kill();
        }
        self.refresh_lock_state();
    }

    fn refresh_lock_state(&mut self) {
        self.transition_lock(self.lock_enabled && self.auth_property_locked);
    }

    fn transition_lock(&mut self, locked: bool) {
        if self.locked == locked {
            return;
        }
        self.locked = locked;
        self.locked_watchface_selected = false;
        self.swallowed_touch_slots.clear();
        if locked {
            if self.shell_mode != ShellMode::LockScreen {
                self.lock_return_mode = self.shell_mode;
            }
            self.shell_mode = ShellMode::LockScreen;
            self.crown_press.release();
            self.cancel_touches();
            self.show_placeholder(false);
            if self.ambient {
                // A low-power renderer is outside the lock compositor path.
                self.change_display("off");
            }
            self.set_keyboard_focus(self.lock_screen.surface.clone());
        } else {
            let restore = if self.lock_return_mode == ShellMode::App && !self.has_live_toplevels() {
                self.app_return_target()
            } else {
                self.lock_return_mode
            };
            self.shell_mode = restore;
            self.cancel_touches();
            self.set_keyboard_focus(self.active_surface());
        }
        self.damage = true;
        // Watch output capture must not retain a pre-lock frame or expose even
        // the lock UI to unrelated clients while authentication is active.
        self.wayland.capture.set_active(self.display_on && !self.is_locked());
        self.wayland.desktop.capture.set_active(
            self.wayland.desktop.config.enabled && !self.is_locked(),
        );
    }

    fn select_locked_watchface(&mut self, selected: bool) {
        if !self.is_locked() || self.locked_watchface_selected == selected {
            return;
        }
        self.locked_watchface_selected = selected;
        self.cancel_touches();
        self.damage = true;
        self.set_keyboard_focus(self.active_surface());
    }

    fn set_lock_screen_command(&mut self, command: Vec<String>) {
        if !valid_lock_screen_command(&command) {
            warn!("Ignoring empty lock-screen renderer command; disable it in shell.conf and restart");
            return;
        }
        let was_enabled = self.lock_enabled;
        self.lock_screen.kill();
        self.lock_screen = ManagedRole::new(RoleId::LockScreen, command);
        self.lock_enabled = !self.lock_screen.command.is_empty();
        self.auth_monitor_active.store(self.lock_enabled, std::sync::atomic::Ordering::Release);
        if !was_enabled && self.lock_enabled {
            // The role becomes opt-in immediately. Until a trusted service
            // property says otherwise, missing service state remains locked.
            self.auth_owner_available = false;
            self.auth_owner = None;
            self.auth_property_locked = true;
            self.auth_state_resolved = false;
            self.refresh_lock_state();
        } else if was_enabled && !self.lock_enabled {
            self.auth_state_resolved = true;
            self.transition_lock(false);
        } else {
            self.damage = true;
            self.set_keyboard_focus(self.active_surface());
        }
        if self.is_locked() && self.auth_state_ready() {
            ensure_role_running(&mut self.lock_screen, &self.xdg_runtime, &self.wakeup);
        }
    }

    fn active_surface(&self) -> Option<WlSurface> {
        if self.is_locked() {
            return if self.locked_watchface_selected {
                None
            } else {
                self.lock_screen.surface.clone()
            };
        }
        match self.shell_mode {
            ShellMode::LockScreen => self.lock_screen.surface.clone(),
            ShellMode::Watchface => self.visible_watchface().surface.clone(),
            ShellMode::Launcher => self.launcher.surface.clone(),
            ShellMode::Settings => self.settings.surface.clone(),
            ShellMode::Agent => self.agent.surface.clone(),
            ShellMode::App => self.focused_surface.clone(),
        }
    }

    fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        if let Some(keyboard) = self.wayland.seat.get_keyboard() {
            keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
    }

    fn is_role_surface(&self, surface: &WlSurface) -> bool {
        self.lock_screen.is_surface(surface)
            || self.watchface.is_surface(surface)
            || self.placeholder.role.is_surface(surface)
            || self.launcher.is_surface(surface)
            || self.settings.is_surface(surface)
            || self.agent.is_surface(surface)
            || self.overlay.is_surface(surface)
    }

    /// Close the foreground app by sending xdg_toplevel.close (skips role surfaces).
    fn close_foreground_app(&mut self) {
        let toplevels: Vec<_> = self
            .wayland
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .cloned()
            .collect();
        for toplevel in toplevels {
            if self.is_role_surface(toplevel.wl_surface()) {
                continue;
            }
            toplevel.send_close();
        }
    }

    /// Check if any non-role toplevel surfaces are still alive.
    fn has_live_toplevels(&self) -> bool {
        self.app_surfaces
            .iter()
            .any(|entry| entry.surface.is_alive() && entry.buffer.is_some())
    }

    /// Spawn an app process (fire-and-forget, no stdin/stdout piping).
    fn spawn_app(&mut self, parts: &[String], return_mode: ShellMode) {
        if self.is_locked() {
            return;
        }
        if self.sleep_enabled {
            self.set_display_power(true);
            if !self.running {return;}
        }
        if parts.is_empty() {
            return;
        }
        let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
        match spawn_child(
            Command::new(&parts[0])
                .args(&parts[1..])
                .env("XDG_RUNTIME_DIR", &xdg)
                .env("WAYLAND_DISPLAY", "wayland-0")
                // Qt Wayland compatibility: these are harmless for non-Qt apps
                .env("QT_QPA_PLATFORM", "wayland")
                .env("QT_WAYLAND_DISABLE_WINDOWDECORATION", "1")
                .env("QT_WAYLAND_CLIENT_BUFFER_INTEGRATION", "none"),
        ) {
            Ok(child) => {
                info!(pid = child.id(), argv = ?parts, "App spawned");
                self.apps.push(child);
                self.app_return_mode = return_mode;
            }
            Err(e) => warn!(argv = ?parts, "Failed to spawn app: {}", e),
        }
    }

    /// Request a short CPU lease before spawning, without blocking the event loop.
    fn queue_app_launch(&mut self, parts: &[String], return_mode: ShellMode) {
        if parts.is_empty() || self.is_locked() { return; }
        let args = parts.to_vec();
        let tx = self.ctl_tx.clone();
        let wakeup = self.wakeup.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("app-launch-boost".into())
            .spawn(move || {
                request_launch_boost();
                if tx.send(CtlMessage::LaunchApp { args, return_mode }).is_ok() {
                    wakeup.notify();
                }
            })
        {
            warn!(%error, "Launch boost worker unavailable");
            self.spawn_app(parts, return_mode);
        }
    }

    /// Process a stdout message from any role.
    fn handle_role_message(&mut self, role: RoleId, msg: &str) {
        // Renderer output is never an authentication authority. While locked,
        // shell roles also cannot navigate, launch apps, or blank the display.
        if role == RoleId::LockScreen || self.is_locked() {
            return;
        }
        match msg {
            "dismiss" if role == RoleId::Agent => self.dismiss_agent(),
            "screen-off" => self.set_display_power(false),
            "go-watchface" => {
                info!(from = ?role, "Navigation: go-watchface");
                self.switch_mode(ShellMode::Watchface);
            }
            "go-settings" => {
                info!(from = ?role, "Navigation: go-settings");
                self.switch_mode(ShellMode::Settings);
            }
            "go-launcher" => {
                info!(from = ?role, "Navigation: go-launcher");
                self.switch_mode(ShellMode::Launcher);
            }
            _ if msg.starts_with("launch-argv:") => {
                match serde_json::from_str::<Vec<String>>(&msg[12..]) {
                    Ok(args)
                        if !args.is_empty()
                            && !args[0].is_empty()
                            && args.iter().all(|a| !a.contains('\0')) =>
                    {
                        self.queue_app_launch(
                            &args,
                            if role == RoleId::Settings { ShellMode::Settings } else { ShellMode::Launcher },
                        )
                    }
                    _ => warn!("Invalid launcher argument vector"),
                }
            }
            _ if msg.starts_with("launch:") => {
                // Compatibility with older roles. New launcher sends an argument vector.
                match shell_words::split(&msg[7..]) {
                    Ok(args) => self.queue_app_launch(
                        &args,
                        if role == RoleId::Settings { ShellMode::Settings } else { ShellMode::Launcher },
                    ),
                    Err(e) => warn!(%e, "Invalid legacy launch command"),
                }
            }
            _ => warn!(?role, msg, "Unknown role message"),
        }
    }

    /// Process stdout messages from all role processes.
    fn process_role_messages(&mut self) {
        for _ in self.lock_screen.drain_messages() {}
        for msg in self.overlay.drain_messages() { self.handle_role_message(RoleId::Overlay, &msg); }
        let locked = self.is_locked();
        let wf_msgs = self.watchface.drain_messages();
        for msg in self.placeholder.role.drain_messages() {
            if !self.placeholder.role.reply_to_lock_state_query(&msg, locked) {
                self.handle_role_message(RoleId::Watchface, &msg);
            }
        }
        let launcher_msgs = self.launcher.drain_messages();
        let settings_msgs = self.settings.drain_messages();
        for msg in self.agent.drain_messages() { self.handle_role_message(RoleId::Agent, &msg); }

        for msg in wf_msgs {
            if !self.watchface.reply_to_lock_state_query(&msg, locked) {
                self.handle_role_message(RoleId::Watchface, &msg);
            }
        }
        for msg in launcher_msgs {
            self.handle_role_message(RoleId::Launcher, &msg);
        }
        for msg in settings_msgs {
            self.handle_role_message(RoleId::Settings, &msg);
        }
    }
}

// --- Config file ---

fn config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/ceres".into());
    std::path::PathBuf::from(home).join(".config/hoki/shell.conf")
}

struct Config {
    lock_screen: Vec<String>,
    watchface: Vec<String>,
    launcher: Vec<String>,
    settings: Vec<String>,
    agent: Vec<String>,
    overlay: Vec<String>,
    display_timeout: u64,
}

fn read_config() -> Config {
    let default_wf: Vec<String> = vec!["/usr/lib/hoki-watchface".into(), "--watchface".into()];
    let default_launcher: Vec<String> = vec!["/usr/lib/hoki-launcher".into()];
    let default_settings: Vec<String> = vec!["/usr/lib/hoki-settings".into()];

    let path = config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            return Config {
                watchface: default_wf,
                lock_screen: Vec::new(),
                launcher: default_launcher,
                settings: default_settings,
                agent: Vec::new(),
                overlay: Vec::new(),
                display_timeout: 0,
            };
        }
    };

    let mut watchface = None;
    let mut lock_screen = None;
    let mut launcher = None;
    let mut settings = None;
    let mut agent = None;
    let mut overlay = None;
    let mut display_timeout: u64 = 0;

    for line in content.lines() {
        let line = line.trim();
        if let Some(cmd) = line.strip_prefix("lock-screen=") {
            // A malformed non-empty command must fail closed by enabling the
            // role with an unspawnable command, leaving the display black.
            lock_screen = Some(match shell_words::split(cmd) {
                Ok(command) => command,
                Err(_) if !cmd.trim().is_empty() => vec!["<invalid-lock-screen-command>".into()],
                Err(_) => Vec::new(),
            });
        } else if let Some(cmd) = line.strip_prefix("watchface=") {
            watchface = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("launcher=") {
            launcher = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("settings=") {
            settings = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("agent=") {
            agent = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("overlay=") {
            overlay = shell_words::split(cmd).ok();
        } else if let Some(val) = line.strip_prefix("display_timeout=") {
            display_timeout = val.trim().parse().unwrap_or(0);
        }
    }

    Config {
        lock_screen: lock_screen.unwrap_or_default(),
        watchface: watchface.unwrap_or(default_wf),
        launcher: launcher.unwrap_or(default_launcher),
        settings: settings.unwrap_or(default_settings),
        agent: agent.unwrap_or_default(),
        overlay: overlay.unwrap_or_default(),
        display_timeout,
    }
}

fn write_config(
    watchface: &[String],
    launcher: &[String],
    settings: &[String],
    agent: &[String],
    overlay: &[String],
    lock_screen: &[String],
) -> std::io::Result<()> {
    config_file::save_roles(&config_path(), watchface, launcher, settings, agent, overlay, lock_screen)
}

fn spawn_child(command: &mut Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGCHLD);
            if libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

// --- Role spawning ---

fn spawn_role(role: &mut ManagedRole, xdg_runtime: &str, wakeup: &Arc<wakeup::Wakeup>) {
    if role.command.is_empty() {
        return;
    }

    let binary = &role.command[0];
    let args = &role.command[1..];

    match spawn_child(
        Command::new(binary)
            .args(args)
            .env("XDG_RUNTIME_DIR", xdg_runtime)
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("HOKI_MANAGED_ROLE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped()),
    ) {
        Ok(mut child) => {
            let pid = child.id();
            let stdin =
                match role_input::RoleInput::new(child.stdin.take().expect("piped stdin").into()) {
                    Ok(stdin) => stdin,
                    Err(e) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        role.last_exit = Some(std::time::Instant::now());
                        warn!(role = ?role.id, %e, "Could not initialize role input");
                        return;
                    }
                };
            let stdout = child.stdout.take();
            info!(pid, role = ?role.id, cmd = ?role.command, "Role spawned");
            role.child_pid = Some(pid);
            role.stdin = Some(stdin);
            role.reported_visible = None;
            role.reported_locked = None;
            role.process = Some(child);
            role.surface = None;
            role.buffer = None;

            // Spawn a reader thread for the role's stdout
            if let Some(stdout) = stdout {
                let (tx, rx) = mpsc::channel();
                let wakeup = wakeup.clone();
                let name = format!("{:?}-stdout", role.id);
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(std::result::Result::ok) {
                            if tx.send(line).is_err() {
                                break;
                            }
                            wakeup.notify();
                        }
                        wakeup.notify();
                    })
                    .ok();
                role.rx = Some(rx);
            }
        }
        Err(e) => {
            role.last_exit = Some(std::time::Instant::now());
            warn!(role = ?role.id, cmd = ?role.command, "Failed to spawn: {}", e);
        }
    }
}

fn ensure_role_running(role: &mut ManagedRole, xdg_runtime: &str, wakeup: &Arc<wakeup::Wakeup>) {
    if role.command.is_empty() {
        return;
    }
    role.check_alive();
    if role.process.is_none() {
        // Backoff: wait at least 2 seconds between respawns to avoid crash loops
        if let Some(last) = role.last_exit {
            if last.elapsed() < std::time::Duration::from_secs(2) {
                return;
            }
        }
        spawn_role(role, xdg_runtime, wakeup);
    }
}

// --- Control socket ---

fn start_control_socket(tx: mpsc::Sender<CtlMessage>, wakeup: Arc<wakeup::Wakeup>) {
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let path = format!("{}/hoki-compositor.sock", xdg);

    // Remove stale socket
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind control socket {}: {}", path, e);
            return;
        }
    };

    info!(path, "Control socket bound");

    std::thread::Builder::new()
        .name("ctl-socket".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        handle_ctl_connection(stream, &tx);
                        wakeup.notify();
                    }
                    Err(e) => warn!("Control socket accept: {}", e),
                }
            }
        })
        .ok();
}

fn handle_ctl_connection(stream: std::os::unix::net::UnixStream, tx: &mpsc::Sender<CtlMessage>) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .ok();

    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };

    let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }

    let response = process_ctl_command(line.trim(), tx);
    writer.write_all(response.as_bytes()).ok();
}

fn process_ctl_command(cmd: &str, tx: &mpsc::Sender<CtlMessage>) -> String {
    if cmd == "screen-off" {
        return if tx.send(CtlMessage::ScreenOff).is_ok() {
            "ok\n".into()
        } else {
            "error: compositor unavailable\n".into()
        };
    }
    let mut config = read_config();
    let setter = [
        ("set-lock-screen ", RoleId::LockScreen),
        ("set-watchface ", RoleId::Watchface),
        ("set-launcher ", RoleId::Launcher),
        ("set-settings ", RoleId::Settings),
        ("set-agent ", RoleId::Agent),
        ("set-overlay ", RoleId::Overlay),
    ]
    .into_iter()
    .find_map(|(prefix, id)| cmd.strip_prefix(prefix).map(|rest| (id, rest)))
    .or_else(|| (cmd == "set-agent").then_some((RoleId::Agent, "")))
    .or_else(|| (cmd == "set-lock-screen").then_some((RoleId::LockScreen, "")))
    .or_else(|| (cmd == "set-overlay").then_some((RoleId::Overlay, "")));
    if let Some((id, text)) = setter {
        let command = match shell_words::split(text) {
            Ok(command) => command,
            Err(e) => return format!("error: invalid command: {e}\n"),
        };
        if id == RoleId::LockScreen && !valid_lock_screen_command(&command) {
            return "error: lock-screen requires a renderer command\n".into();
        }
        match id {
            RoleId::LockScreen => config.lock_screen = command.clone(),
            RoleId::Watchface => config.watchface = command.clone(),
            RoleId::Launcher => config.launcher = command.clone(),
            RoleId::Settings => config.settings = command.clone(),
            RoleId::Agent => config.agent = command.clone(),
            RoleId::Overlay => config.overlay = command.clone(),
        }
        if let Err(e) = write_config(&config.watchface, &config.launcher, &config.settings, &config.agent, &config.overlay, &config.lock_screen) {
            return format!("error: {e}\n");
        }
        return if tx.send(CtlMessage::SetRole { id, command }).is_ok() {
            "ok\n".into()
        } else {
            "error: compositor unavailable\n".into()
        };
    }
    match cmd {
        "get-watchface" => format!("{}\n", shell_words::join(&config.watchface)),
        "get-lock-screen" => format!("{}\n", shell_words::join(&config.lock_screen)),
        "get-launcher" => format!("{}\n", shell_words::join(&config.launcher)),
        "get-overlay" => format!("{}\n", shell_words::join(&config.overlay)),
        "get-agent" => format!("{}\n", shell_words::join(&config.agent)),
        "get-settings" => format!("{}\n", shell_words::join(&config.settings)),
        _ => "error: unknown command\n".into(),
    }
}

fn valid_lock_screen_command(command: &[String]) -> bool {
    command.first().is_some_and(|exe| !exe.is_empty())
}

fn start_initial_roles(
    compositor: &mut Compositor,
    xdg_runtime: &str,
    wakeup: &Arc<wakeup::Wakeup>,
) {
    if compositor.is_locked() && compositor.auth_state_ready() {
        compositor.wayland.capture.set_active(false);
        spawn_role(&mut compositor.lock_screen, xdg_runtime, wakeup);
    }
    if compositor.auth_state_ready() {
        spawn_role(&mut compositor.watchface, xdg_runtime, wakeup);
        spawn_role(&mut compositor.overlay, xdg_runtime, wakeup);
    }
}

// --- Main ---

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::filter::EnvFilter::new("info")),
        )
        .init();

    info!("nereid-compositor starting (HWC proxy mode)");

    let mut display = Display::<Compositor>::new().context("Failed to create Wayland display")?;

    let socket = ListeningSocket::bind("wayland-0").context("Failed to bind Wayland socket")?;
    let desktop_socket = ListeningSocket::bind("wayland-desktop").context("Failed to bind desktop socket")?;
    let socket_name = socket
        .socket_name()
        .map(|n| n.to_string_lossy().to_string());
    info!(socket = ?socket_name, "Wayland socket bound");

    if let Some(ref name) = socket_name {
        unsafe { std::env::set_var("WAYLAND_DISPLAY", name) };
    }

    // Block SIGCHLD before spawning any worker threads.
    let child_signals = wakeup::ChildSignals::new()?;
    let wakeup = Arc::new(wakeup::Wakeup::new()?);

    // Start control socket for runtime role changes
    let (ctl_tx, ctl_rx) = mpsc::channel();
    start_control_socket(ctl_tx.clone(), wakeup.clone());

    let mut compositor = Compositor::new(&display, ctl_tx.clone(), ctl_rx, wakeup.clone())?;
    if let Some(keyboard) = compositor.wayland.seat.get_keyboard() {
        keyboard.set_keymap_from_string(&mut compositor, wayland::WATCH_KEYMAP.into())
            .context("watch button keymap")?;
    }

    // Persistent roles start immediately; foreground roles start on navigation
    let xdg_runtime = compositor.xdg_runtime.clone();
    auth::start_monitor(ctl_tx.clone(), wakeup.clone(), compositor.auth_monitor_active.clone());
    start_initial_roles(&mut compositor, &xdg_runtime, &wakeup);

    // Set up epoll for interrupt-driven main loop
    const EPOLL_INPUT: u64 = 1;
    const EPOLL_WAYLAND: u64 = 2;
    const EPOLL_LISTEN: u64 = 3;

    let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC).context("epoll_create")?;
    epoll
        .add(
            compositor.input_mgr.as_fd(),
            EpollEvent::new(EpollFlags::EPOLLIN, EPOLL_INPUT),
        )
        .context("epoll add input")?;
    epoll
        .add(
            display.as_fd(),
            EpollEvent::new(EpollFlags::EPOLLIN, EPOLL_WAYLAND),
        )
        .context("epoll add wayland")?;
    epoll
        .add(
            socket.as_fd(),
            EpollEvent::new(EpollFlags::EPOLLIN, EPOLL_LISTEN),
        )
        .context("epoll add listen socket")?;

    epoll.add(wakeup.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 4))?;
    epoll.add(
        child_signals.as_fd(),
        EpollEvent::new(EpollFlags::EPOLLIN, 5),
    )?;
    epoll.add(
        compositor.proxy.as_fd(),
        EpollEvent::new(EpollFlags::EPOLLRDHUP, 6),
    )?;
    epoll.add(desktop_socket.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 7))?;
    info!("Entering epoll main loop");

    let mut prev_shell_mode = compositor.shell_mode;
    let mut epoll_events = [EpollEvent::empty(); 16];

    while compositor.running {
        compositor.wayland.desktop.reap_globals(
            &compositor.display_handle, std::time::Instant::now(),
        );
        compositor.reconcile_sleep();
        compositor.wayland.capture.set_active(compositor.display_on && !compositor.is_locked());
        compositor.wayland.desktop.capture.set_active(
            compositor.wayland.desktop.config.enabled && !compositor.is_locked(),
        );
        // Deliver stopped events before an indefinite screen-off epoll wait.
        display.flush_clients().context("Wayland capture state flush")?;
        // Compute epoll timeout
        let mut timeout = if !compositor.display_on {
            // Screen off: sleep indefinitely, only wake on input interrupt
            EpollTimeout::NONE
        } else if compositor.damage {
            // Pending damage: don't block, render immediately
            EpollTimeout::ZERO
        } else if !compositor.sleep_enabled && compositor.display_timeout_secs > 0 {
            // Compute remaining timeout until display blanks
            let elapsed_ms = compositor.last_activity.elapsed().as_millis() as u64;
            let timeout_total_ms = compositor.display_timeout_secs.saturating_mul(1000);
            if elapsed_ms >= timeout_total_ms {
                EpollTimeout::ZERO
            } else {
                let remaining = (timeout_total_ms - elapsed_ms).min(u16::MAX as u64) as u16;
                EpollTimeout::from(remaining)
            }
        } else {
            // No display timeout, wake on any fd activity
            EpollTimeout::NONE
        };

        if compositor.display_on {
            for role in [
                &compositor.lock_screen,
                &compositor.watchface,
                &compositor.placeholder.role,
                &compositor.launcher,
                &compositor.settings,
                &compositor.agent,
                &compositor.overlay,
            ] {
                let needed = (role.id == RoleId::LockScreen && compositor.is_locked())
                    || role.id == RoleId::Watchface || role.id == RoleId::Overlay
                    || (role.id == RoleId::Launcher
                        && compositor.shell_mode == ShellMode::Launcher)
                    || (role.id == RoleId::Settings
                        && compositor.shell_mode == ShellMode::Settings)
                    || (role.id == RoleId::Agent && compositor.shell_mode == ShellMode::Agent);
                let needed = needed && (!std::ptr::eq(role, &compositor.placeholder.role) || compositor.placeholder.visible);
                if needed && !role.command.is_empty() && role.process.is_none() {
                    let remaining = role
                        .last_exit
                        .map(|last| {
                            std::time::Duration::from_secs(2).saturating_sub(last.elapsed())
                        })
                        .unwrap_or_default();
                    let ms = remaining.as_millis().saturating_add(1).min(2001) as u16;
                    let candidate = EpollTimeout::from(ms);
                    if timeout == EpollTimeout::NONE || timeout.as_millis().unwrap_or(0) > ms as u32
                    {
                        timeout = candidate;
                    }
                }
            }
        }
        if let Some(remaining) = compositor.wayland.desktop.deadline(std::time::Instant::now()) {
            let ms = remaining.as_millis()
                .saturating_add(u128::from(!remaining.is_zero()))
                .min(u16::MAX as u128) as u16;
            if timeout == EpollTimeout::NONE || timeout.as_millis().unwrap_or(0) > ms as u32 {
                timeout = EpollTimeout::from(ms);
            }
        }
        if compositor.display_on && compositor.has_visible_callbacks() {
            let remaining = std::time::Duration::from_millis(22)
                .saturating_sub(compositor.last_callback.elapsed());
            let ms = remaining.as_millis() as u32 + 1;
            if timeout == EpollTimeout::NONE || timeout.as_millis().unwrap_or(0) > ms {
                timeout = EpollTimeout::from(ms as u16);
            }
        }
        if let Some(ms) = compositor.crown_press.remaining_ms(std::time::Instant::now()) {
            if timeout == EpollTimeout::NONE || timeout.as_millis().unwrap_or(0) > ms as u32 {
                timeout = EpollTimeout::from(ms);
            }
        }
        // These borrowed descriptors stay alive until the registrations are removed,
        // before dispatch can kill or replace a role. No EPOLLOUT interest when idle.
        let pending_inputs: Vec<_> = [
            &compositor.lock_screen,
            &compositor.watchface,
                &compositor.placeholder.role,
            &compositor.launcher,
            &compositor.settings,
            &compositor.agent,
            &compositor.overlay,
        ]
        .into_iter()
        .filter_map(|r| r.stdin.as_ref())
        .filter(|s| s.pending())
        .collect();
        for (index, stdin) in pending_inputs.iter().enumerate() {
            epoll.add(
                stdin.as_fd(),
                EpollEvent::new(EpollFlags::EPOLLOUT, 7 + index as u64),
            )?;
        }
        let wait_result = epoll.wait(&mut epoll_events, if timeout == EpollTimeout::NONE {EpollTimeout::from(500u16)} else {EpollTimeout::from(timeout.as_millis().unwrap_or(500).min(500) as u16)});
        for stdin in pending_inputs {
            epoll.delete(stdin.as_fd())?;
        }
        let count = match wait_result {
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e).context("epoll_wait"),
        };
        if epoll_events[..count].iter().any(|event| event.data() == 6) {
            anyhow::bail!("HWC proxy disconnected; restarting compositor");
        }
        wakeup.drain();
        child_signals.drain();
        compositor.apps.retain_mut(|child| match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    info!(pid = child.id(), %status, "App process exited");
                } else {
                    warn!(pid = child.id(), %status, "App process exited unsuccessfully");
                }
                false
            }
            _ => true,
        });
        for role in [
            &mut compositor.lock_screen,
            &mut compositor.watchface,
            &mut compositor.placeholder.role,
            &mut compositor.launcher,
            &mut compositor.settings,
            &mut compositor.agent,
            &mut compositor.overlay,
        ] {
            let was_running = role.process.is_some();
            role.flush_input();
            if !role.check_alive() && was_running {
                compositor.damage = true;
            }
        }

        // Accept new client connections
        if let Some(stream) = socket.accept().context("Socket accept")? {
            let client = display
                .handle()
                .insert_client(
                    stream,
                    std::sync::Arc::new(wayland::ClientState {
                        desktop: false,
                        compositor: Default::default(),
                    }),
                )
                .context("Failed to insert client")?;
            info!("New Wayland client connected: {:?}", client.id());
        }

        while let Some(stream) = desktop_socket.accept().context("desktop accept")? {
            display.handle().insert_client(stream, std::sync::Arc::new(wayland::ClientState {
                desktop: true, compositor: Default::default(),
            })).context("insert desktop client")?;
        }
        // Process Wayland client events
        display
            .dispatch_clients(&mut compositor)
            .context("Wayland dispatch")?;
        display.flush_clients().context("Wayland flush")?;

        // Detect toplevel lifecycle changes
        if compositor.shell_mode == ShellMode::App && !compositor.has_live_toplevels() {
            let target = compositor.app_return_target();
            info!(?target, "All toplevels closed, returning to shell role");
            compositor.switch_mode(target);
            compositor.focused_surface = None;
            if target == ShellMode::Launcher {
                compositor.launcher.send("app-closed");
            }
        }

        compositor
            .frame_callbacks
            .retain(|(surface, cb)| surface.is_alive() && cb.is_alive());

        // Process role messages (screen-off requests, etc.)
        compositor.process_role_messages();

        // Overlay lifetime is independent of foreground mode and display power.
        if compositor.auth_state_ready() {
            ensure_role_running(&mut compositor.overlay, &xdg_runtime, &wakeup);
        }
        if compositor.is_locked() && compositor.display_on && compositor.auth_state_ready() {
            ensure_role_running(&mut compositor.lock_screen, &xdg_runtime, &wakeup);
        }

        // Respawn watchface if it crashed while display is on
        if compositor.display_on && compositor.auth_state_ready() {
            ensure_role_running(&mut compositor.watchface, &xdg_runtime, &wakeup);
            match compositor.shell_mode {
                ShellMode::Agent => { ensure_role_running(&mut compositor.agent, &xdg_runtime, &wakeup); }
                ShellMode::Launcher => {
                    ensure_role_running(&mut compositor.launcher, &xdg_runtime, &wakeup)
                }
                ShellMode::Settings => {
                    ensure_role_running(&mut compositor.settings, &xdg_runtime, &wakeup)
                }
                _ => {}
            }
        }

        // Process control socket messages on the compositor thread.
        while let Ok(msg) = compositor.ctl_rx.try_recv() {
            match msg {
                CtlMessage::ScreenOff => compositor.set_display_power(false),
                CtlMessage::LaunchApp { args, return_mode } => compositor.spawn_app(&args, return_mode),
                CtlMessage::AuthState(state) => compositor.update_auth_state(state),
                CtlMessage::SetRole { id, command } => {
                    info!(role = ?id, cmd = ?command, "Setting role via control socket");
                    compositor.cancel_touches();
                    compositor.damage = true;
                    match id {
                        RoleId::LockScreen => compositor.set_lock_screen_command(command),
                        RoleId::Overlay => {
                            compositor.overlay.kill();
                            compositor.overlay = ManagedRole::new(RoleId::Overlay, command);
                            if compositor.auth_state_ready() {
                                ensure_role_running(&mut compositor.overlay, &xdg_runtime, &wakeup);
                            }
                        }
                        RoleId::Agent => {
                            compositor.dismiss_agent();
                            compositor.agent.kill();
                            compositor.agent = ManagedRole::new(RoleId::Agent, command);
                        }
                        RoleId::Watchface => {
                            compositor.watchface.kill();
                            compositor.watchface = ManagedRole::new(RoleId::Watchface, command);
                            if compositor.display_on && compositor.auth_state_ready() {
                                spawn_role(&mut compositor.watchface, &xdg_runtime, &wakeup);
                            }
                        }
                        RoleId::Launcher => {
                            compositor.launcher.kill();
                            compositor.launcher = ManagedRole::new(RoleId::Launcher, command);
                            if compositor.display_on && compositor.auth_state_ready() && compositor.shell_mode == ShellMode::Launcher
                            {
                                spawn_role(&mut compositor.launcher, &xdg_runtime, &wakeup);
                            }
                        }
                        RoleId::Settings => {
                            compositor.settings.kill();
                            compositor.settings = ManagedRole::new(RoleId::Settings, command);
                            if compositor.display_on && compositor.auth_state_ready() && compositor.shell_mode == ShellMode::Settings
                            {
                                spawn_role(&mut compositor.settings, &xdg_runtime, &wakeup);
                            }
                        }
                    }
                }
            }
        }

        // Process input — always dispatch, even when display is off (for wake-up)
        let time_ms = {
            let dur = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            dur.as_millis() as u32
        };

        match compositor.input_mgr.dispatch() {
            Ok(events) => {
                for ev in events {
                    if let input::InputEvent::Desktop(event) = ev {
                        if !compositor.is_locked() {
                            compositor.handle_desktop_input(event, time_ms);
                        }
                        continue;
                    }
                    if !compositor.display_on {
                        // Buttons distinguish a visible ambient face from a dark display.
                        if let input::InputEvent::Button(ref b) = ev {
                            handle_button(&mut compositor, b, time_ms);
                        }
                        continue;
                    }

                    // Any input resets the display timeout
                    compositor.note_activity();

                    // Display is on — normal input handling
                    match ev {
                        input::InputEvent::Desktop(_) => unreachable!(),
                        input::InputEvent::Touch(t) => {
                            handle_touch(&mut compositor, &t, time_ms);
                        }
                        input::InputEvent::Button(b) => {
                            handle_button(&mut compositor, &b, time_ms);
                        }
                        input::InputEvent::Scroll(s) => {
                            handle_scroll(&mut compositor, &s, time_ms);
                        }
                    }
                }
            }
            Err(e) => warn!("Input dispatch error: {}", e),
        }

        if compositor.crown_press.tick(std::time::Instant::now()) {
            compositor.activate_agent();
        }
        // Flush input events to clients immediately
        display
            .flush_clients()
            .context("Wayland flush after input")?;

        // Display timeout
        if !compositor.sleep_enabled && compositor.display_on && compositor.display_timeout_secs > 0 {
            if compositor.last_activity.elapsed().as_secs() >= compositor.display_timeout_secs {
                info!(timeout = compositor.display_timeout_secs, "Display timeout");
                compositor.set_display_power(false);
            }
        }

        if !compositor.running {
            anyhow::bail!("display connection failed");
        }

        // Input may just have returned to the watchface. Publish that state and
        // reconcile before submitting any primary-face frame. Present the LP
        // companion before the synchronous Sidekick upload begins.
        compositor.reconcile_sleep();
        if !compositor.running {
            anyhow::bail!("display connection failed");
        }
        let locked = compositor.is_locked();
        let locked_watchface = locked && compositor.locked_watchface_selected;
        compositor.watchface.report_lock_state(locked);
        compositor.placeholder.role.report_lock_state(locked);
        compositor.lock_screen.report_visibility(compositor.display_on && locked && !locked_watchface);
        compositor.watchface.report_visibility(
            compositor.display_on && !compositor.placeholder.visible
                && ((!locked && compositor.shell_mode == ShellMode::Watchface) || locked_watchface),
        );
        compositor.placeholder.role.report_visibility(
            compositor.display_on && !locked && compositor.placeholder.visible,
        );
        compositor.launcher.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Launcher,
        );
        compositor.settings.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Settings,
        );

        compositor.overlay.report_visibility(compositor.display_on && !compositor.is_locked());
        compositor.agent.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Agent,
        );
        compositor.wayland.desktop.render(std::time::Instant::now(), time_ms);
        // Skip rendering when display is off
        compositor.wayland.capture.set_active(compositor.display_on && !compositor.is_locked());
        compositor.wayland.desktop.capture.set_active(
            compositor.wayland.desktop.config.enabled && !compositor.is_locked(),
        );
        if !compositor.display_on {
            display.flush_clients().context("Wayland capture stop flush")?;
            continue;
        }

        // Detect shell mode changes as damage
        if compositor.shell_mode != prev_shell_mode {
            compositor.damage = true;
            prev_shell_mode = compositor.shell_mode;
        }

        let rendered = compositor.damage;
        if rendered {
            let idx = compositor.write_buf_idx;
            let locked = compositor.is_locked();
            let watchface_buf = if !locked && compositor.placeholder.visible {
                compositor.placeholder.role.buffer.as_ref()
            } else {
                compositor.watchface.buffer.as_ref()
            };
            let locked_watchface = locked && compositor.locked_watchface_selected;
            let app_buf = compositor
                .app_surfaces
                .iter()
                .find(|entry| compositor.focused_surface.as_ref() == Some(&entry.surface))
                .and_then(|entry| entry.buffer.as_ref());
            compose::composite_frame(
                compositor.framebuffers[idx].as_mut_slice(),
                compositor.display_width,
                compositor.display_height,
                compositor.lock_screen.buffer.as_ref(),
                watchface_buf,
                locked_watchface,
                compositor.launcher.buffer.as_ref(),
                compositor.settings.buffer.as_ref(),
                compositor.agent.buffer.as_ref(),
                app_buf,
                &compositor.layer_surfaces,
                compositor.shell_mode,
            );
            compositor.present_composited_frame(idx)?;
            compositor.wayland.capture.presented();
            compositor.write_buf_idx = 1 - idx;
            compositor.damage = false;
        }
        compositor.wayland.capture.copy_pending(
            compositor.framebuffers[1 - compositor.write_buf_idx].as_mut_slice(),
        );
        display.flush_clients().context("Wayland capture flush")?;
        if compositor.has_visible_callbacks()
            && (rendered
                || compositor.last_callback.elapsed() >= std::time::Duration::from_millis(22))
        {
            compositor.complete_visible_callbacks(time_ms);
            display
                .flush_clients()
                .context("Wayland flush after frame")?;
        }
    }

    info!("Compositor shutting down");
    Ok(())
}

// --- Button handling ---

fn handle_button(compositor: &mut Compositor, button: &input::ButtonEvent, time_ms: u32) {
    if !compositor.display_on {
        if !button.pressed { return; }
        let locked = compositor.is_locked();
        let visible_watchface = !locked && compositor.ambient && compositor.shell_mode == ShellMode::Watchface;
        compositor.set_display_power(true);
        if !compositor.running { return; }
        compositor.note_activity();
        if !visible_watchface {
            // A dark display consumes the wake tap; a held crown can still activate the agent.
            if !locked && button.code == KEY_POWER && !compositor.agent.command.is_empty() {
                compositor.crown_press.press(std::time::Instant::now(), true);
            }
            if !locked && compositor.secondary_only && compositor.shell_mode == ShellMode::Watchface {
                compositor.switch_mode(ShellMode::Launcher);
            }
            return;
        }
        // A visible ambient face uses exactly the same actions as the primary face.
    }
    if compositor.is_locked() {
        if !button.pressed { return; }
        match button.code {
            KEY_POWER => {
                compositor.select_locked_watchface(!compositor.locked_watchface_selected);
            }
            KEY_VOLUMEUP if !compositor.locked_watchface_selected => {
                if let Some(surface) = compositor.lock_screen.surface.clone() {
                    forward_key_to_surface(compositor, &surface, KEY_F13, time_ms);
                }
            }
            KEY_VOLUMEDOWN => compositor.set_display_power(false),
            _ => {}
        }
        return;
    }
    // Raw input can reveal credentials, including in ordinary Settings apps.
    // Never log key codes or touch coordinates, even outside the lock role.
    if button.code == KEY_POWER && !compositor.agent.command.is_empty() {
        let now = std::time::Instant::now();
        if button.pressed { compositor.crown_press.press(now, false); return; }
        // Account for delayed event dispatch at the threshold.
        if compositor.crown_press.tick(now) { compositor.activate_agent(); }
        if !compositor.crown_press.release() { return; }
    } else if !button.pressed { return; }

    match button.code {
        KEY_POWER => {
            // Crown button — global mode switch
            match compositor.shell_mode {
                ShellMode::LockScreen => unreachable!("locked input is routed above"),
                ShellMode::Agent => compositor.dismiss_agent(),
                ShellMode::App => {
                    let target = compositor.app_return_target();
                    info!(?target, "Crown in App mode: closing app and returning to shell role");
                    compositor.close_foreground_app();
                    compositor.switch_mode(target);
                    if target == ShellMode::Launcher {
                        compositor.launcher.send("app-closed");
                    }
                }
                ShellMode::Watchface => {
                    info!("Crown: Watchface -> Launcher");
                    compositor.switch_mode(ShellMode::Launcher);
                }
                ShellMode::Launcher => {
                    info!("Crown: Launcher -> Watchface");
                    compositor.switch_mode(ShellMode::Watchface);
                }
                ShellMode::Settings => {
                    info!("Crown: Settings -> Watchface");
                    compositor.switch_mode(ShellMode::Watchface);
                }
            }
        }
        KEY_VOLUMEUP => {
            // Top button — remap to F13 for Slint compatibility
            match compositor.shell_mode {
                ShellMode::LockScreen => unreachable!("locked input is routed above"),
                ShellMode::Watchface => {
                    info!("Top: Watchface -> Settings");
                    compositor.switch_mode(ShellMode::Settings);
                }
                ShellMode::Agent => {
                    if let Some(surface) = compositor.agent.surface.clone() { forward_key_to_surface(compositor, &surface, KEY_F13, time_ms); }
                }
                ShellMode::Settings => {
                    if let Some(surface) = compositor.settings.surface.clone() {
                        forward_key_to_surface(compositor, &surface, KEY_F13, time_ms);
                    }
                }
                ShellMode::Launcher => {
                    if let Some(surface) = compositor.launcher.surface.clone() {
                        forward_key_to_surface(compositor, &surface, KEY_F13, time_ms);
                    }
                }
                ShellMode::App => {
                    forward_key_to_app(compositor, KEY_F13, time_ms);
                }
            }
        }
        KEY_VOLUMEDOWN => {
            // Bottom button — remap to F14 for Slint compatibility
            match compositor.shell_mode {
                ShellMode::LockScreen => unreachable!("locked input is routed above"),
                ShellMode::Watchface => {
                    // Screen off
                    compositor.set_display_power(false);
                }
                ShellMode::Agent => {
                    if let Some(surface) = compositor.agent.surface.clone() { forward_key_to_surface(compositor, &surface, KEY_F14, time_ms); }
                }
                ShellMode::Settings => {
                    if let Some(surface) = compositor.settings.surface.clone() {
                        forward_key_to_surface(compositor, &surface, KEY_F14, time_ms);
                    }
                }
                ShellMode::Launcher => {
                    if let Some(surface) = compositor.launcher.surface.clone() {
                        forward_key_to_surface(compositor, &surface, KEY_F14, time_ms);
                    }
                }
                ShellMode::App => {
                    forward_key_to_app(compositor, KEY_F14, time_ms);
                }
            }
        }
        _ => {}
    }
}

/// Forward a physical button press to a Wayland surface as a keyboard event.
fn forward_key_to_surface(
    compositor: &mut Compositor,
    surface: &WlSurface,
    evdev_code: u32,
    time_ms: u32,
) {
    if let Some(keyboard) = compositor.wayland.seat.get_keyboard() {
        let serial = SERIAL_COUNTER.next_serial();
        // XKB keycode = evdev code + 8
        let keycode = Keycode::new(evdev_code + 8);

        // Set focus so the key goes to the right client
        keyboard.set_focus(compositor, Some(surface.clone()), serial);

        // Press
        keyboard.input::<(), _>(
            compositor,
            keycode,
            KeyState::Pressed,
            serial,
            time_ms,
            |_, _, _| FilterResult::Forward,
        );
        // Release
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.input::<(), _>(
            compositor,
            keycode,
            KeyState::Released,
            serial,
            time_ms,
            |_, _, _| FilterResult::Forward,
        );
    }
}

/// Forward a physical button press to the focused app as a Wayland keyboard event.
fn forward_key_to_app(compositor: &mut Compositor, evdev_code: u32, time_ms: u32) {
    if let Some(surface) = compositor.focused_surface.clone() {
        forward_key_to_surface(compositor, &surface, evdev_code, time_ms);
    }
}

// --- Scroll handling ---

fn handle_scroll(compositor: &mut Compositor, scroll: &input::ScrollEvent, time_ms: u32) {
    let ticks = scroll.v120 / 120;
    if ticks == 0 {
        return;
    }
    if compositor.is_locked() {
        if !compositor.locked_watchface_selected {
            compositor.lock_screen.send(&format!("scroll:{}", ticks));
        }
        return;
    }

    match compositor.shell_mode {
        ShellMode::LockScreen => unreachable!("locked scroll is routed above"),
        ShellMode::Watchface => {
            compositor.watchface.send(&format!("scroll:{}", ticks));
        }
        ShellMode::Launcher => {
            compositor.launcher.send(&format!("scroll:{}", ticks));
        }
        ShellMode::Agent => compositor.agent.send(&format!("scroll:{}", ticks)),
        ShellMode::Settings => {
            compositor.settings.send(&format!("scroll:{}", ticks));
        }
        ShellMode::App => {
            // Forward scroll to the focused app as pointer axis events
            if let Some(ref surface) = compositor.focused_surface {
                if let Some(pointer) = compositor.wayland.seat.get_pointer() {
                    let serial = SERIAL_COUNTER.next_serial();
                    // Ensure pointer focus is on the surface
                    pointer.motion(
                        compositor,
                        Some((surface.clone(), (0.0, 0.0).into())),
                        &PointerMotionEvent {
                            location: (208.0, 208.0).into(), // center of screen
                            serial,
                            time: time_ms,
                        },
                    );
                    pointer.frame(compositor);

                    let frame = AxisFrame::new(time_ms)
                        .v120(smithay::backend::input::Axis::Vertical, scroll.v120);
                    pointer.axis(compositor, frame);
                    pointer.frame(compositor);
                }
            }
        }
    }
}

// --- Touch handling ---

/// Forward touch events directly to the focused client surface.
/// No gesture interception — crown button replaces edge swipes.
fn handle_touch(compositor: &mut Compositor, touch: &input::TouchEvent, time_ms: u32) {
    if touch.state == input::TouchState::Cancel {
        compositor.swallowed_touch_slots.clear();
        compositor.cancel_touches();
        return;
    }
    if compositor.swallowed_touch_slots.contains(&touch.slot) {
        if touch.state == input::TouchState::Up {
            compositor.swallowed_touch_slots.remove(&touch.slot);
        }
        return;
    }
    if compositor.is_locked() && compositor.locked_watchface_selected {
        if touch.state == input::TouchState::Down {
            compositor.swallowed_touch_slots.insert(touch.slot);
            compositor.select_locked_watchface(false);
        }
        return;
    }
    forward_touch(compositor, touch, time_ms);
}

/// Forward a touch event to the appropriate surface.
fn forward_touch(compositor: &mut Compositor, touch: &input::TouchEvent, time_ms: u32) {
    if touch.state == input::TouchState::Cancel {
        compositor.cancel_touches();
        return;
    }
    let Some(handle) = compositor.wayland.seat.get_touch() else {
        return;
    };
    let serial = SERIAL_COUNTER.next_serial();
    let slot = TouchSlot::from(Some(touch.slot));
    match touch.state {
        input::TouchState::Down => {
            let Some(surface) = find_touch_target(compositor, touch.x, touch.y) else {
                return;
            };
            compositor.touch_targets.insert(
                touch.slot,
                (
                    surface.clone(),
                    MotionEvent {
                        slot,
                        location: (touch.x, touch.y).into(),
                        time: time_ms,
                    },
                ),
            );
            handle.down(
                compositor,
                Some((surface, (0.0, 0.0).into())),
                &DownEvent {
                    slot,
                    location: (touch.x, touch.y).into(),
                    serial,
                    time: time_ms,
                },
            );
        }
        input::TouchState::Motion => {
            let Some((_, last_motion)) = compositor.touch_targets.get_mut(&touch.slot) else {
                return;
            };
            *last_motion = MotionEvent {
                slot,
                location: (touch.x, touch.y).into(),
                time: time_ms,
            };
            handle.motion(
                compositor,
                None,
                &MotionEvent {
                    slot,
                    location: (touch.x, touch.y).into(),
                    time: time_ms,
                },
            );
        }
        input::TouchState::Up => {
            if compositor.touch_targets.remove(&touch.slot).is_none() {
                return;
            }
            handle.up(
                compositor,
                &UpEvent {
                    slot,
                    serial,
                    time: time_ms,
                },
            );
        }
        input::TouchState::Cancel => unreachable!(),
    }
    handle.frame(compositor);
}

/// Find the surface that should receive touch input.
/// Priority: Overlay/Top layers > active role or app > Background/Bottom layers.
fn find_touch_target(compositor: &Compositor, x: f64, y: f64) -> Option<WlSurface> {
    let accepts = |surface: &WlSurface, buf: Option<&wayland::SurfaceBuffer>| {
        let Some(buf) = buf else {
            return false;
        };
        if x < 0.0
            || y < 0.0
            || x >= buf.width as f64
            || y >= buf.height as f64
            || !surface.is_alive()
        {
            return false;
        }
        smithay::wayland::compositor::with_states(surface, |states| {
            states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .input_region
                .as_ref()
                .map_or(true, |region| region.contains((x as i32, y as i32)))
        })
    };
    if compositor.is_locked() {
        if compositor.locked_watchface_selected {
            return None;
        }
        return compositor
            .lock_screen
            .surface
            .as_ref()
            .filter(|surface| accepts(surface, compositor.lock_screen.buffer.as_ref()))
            .cloned();
    }
    for entry in compositor.layer_surfaces.iter().rev() {
        if entry.visible
            && matches!(entry.layer, Layer::Top | Layer::Overlay)
            && accepts(entry.surface.wl_surface(), entry.pending_buffer.as_ref())
        {
            return Some(entry.surface.wl_surface().clone());
        }
    }
    let main = match compositor.shell_mode {
        ShellMode::Watchface => compositor
            .visible_watchface()
            .surface
            .as_ref()
            .map(|s| (s, compositor.visible_watchface().buffer.as_ref())),
        ShellMode::Launcher => compositor
            .launcher
            .surface
            .as_ref()
            .map(|s| (s, compositor.launcher.buffer.as_ref())),
        ShellMode::Settings => compositor
            .settings
            .surface
            .as_ref()
            .map(|s| (s, compositor.settings.buffer.as_ref())),
        ShellMode::Agent => compositor.agent.surface.as_ref().map(|s| (s, compositor.agent.buffer.as_ref())),
        ShellMode::LockScreen => compositor.lock_screen.surface.as_ref().map(|s| (s, compositor.lock_screen.buffer.as_ref())),
        ShellMode::App => compositor
            .app_surfaces
            .iter()
            .find(|entry| compositor.focused_surface.as_ref() == Some(&entry.surface))
            .map(|entry| (&entry.surface, entry.buffer.as_ref())),
    };
    if let Some((surface, buf)) = main {
        if accepts(surface, buf) {
            return Some(surface.clone());
        }
    }
    for entry in compositor.layer_surfaces.iter().rev() {
        if entry.visible
            && matches!(entry.layer, Layer::Background | Layer::Bottom)
            && accepts(entry.surface.wl_surface(), entry.pending_buffer.as_ref())
        {
            return Some(entry.surface.wl_surface().clone());
        }
    }
    None
}

/// Notify powerd of an event for battery logging (blocking D-Bus call).
fn notify_powerd(event: &str) {
    // Use std::process::Command to call dbus-send — avoids zbus blocking runtime issues
    let _ = std::process::Command::new("dbus-send")
        .args([
            "--system",
            "--type=method_call",
            "--dest=org.hoki.power",
            "/org/hoki/power",
            "org.hoki.power.Manager.NotifyEvent",
            &format!("string:{}", event),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Powerd grants one extra core for three seconds; app launch proceeds on errors.
fn request_launch_boost() {
    let request = (|| -> zbus::Result<String> {
        let conn = zbus::blocking::connection::Builder::system()?
            .method_timeout(std::time::Duration::from_millis(150))
            .build()?;
        PowerManagerProxy::new(&conn)?.request_cores(
            1,
            3,
            "nereid-compositor-app-launch",
        )
    })();
    match request {
        Ok(reply) if reply.starts_with("error:") => warn!(%reply, "Launch boost rejected"),
        Err(error) => warn!(%error, "Launch boost request failed"),
        Ok(_) => {},
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.watchface.kill();
        self.lock_screen.kill();
        self.placeholder.role.kill();
        self.launcher.kill();
        self.settings.kill();
        self.agent.kill();
        self.overlay.kill();
    }
}

impl Compositor {
    fn cancel_surface_touches(&mut self, surface: &WlSurface) {
        if self.touch_targets.values().any(|(s, _)| s == surface) {
            self.cancel_touches();
        }
    }

    fn cancel_touches(&mut self) {
        if !self.touch_targets.is_empty() {
            if let Some(handle) = self.wayland.seat.get_touch() {
                // Smithay 0.7 skips cancellation after an already-flushed frame
                // (current >= pending). Reassert the last position to mark each
                // active slot pending before cancelling the whole touch sequence.
                let motions: Vec<_> = self
                    .touch_targets
                    .values()
                    .map(|(_, m)| m.clone())
                    .collect();
                for motion in motions {
                    handle.motion(self, None, &motion);
                }
                handle.cancel(self);
            }
            self.touch_targets.clear();
        }
    }

    fn surface_visible(&self, surface: &WlSurface) -> bool {
        if !self.display_on || !surface.is_alive() {
            return false;
        }
        if self.is_locked() {
            let selected_role = if self.locked_watchface_selected {
                &self.watchface
            } else {
                &self.lock_screen
            };
            return selected_role.is_surface(surface) && selected_role.buffer.is_some();
        }
        let role = match self.shell_mode {
            ShellMode::LockScreen => &self.lock_screen,
            ShellMode::Watchface => self.visible_watchface(),
            ShellMode::Launcher => &self.launcher,
            ShellMode::Settings => &self.settings,
            ShellMode::Agent => &self.agent,
            ShellMode::App => {
                return self.focused_surface.as_ref() == Some(surface)
                    || self
                        .layer_surfaces
                        .iter()
                        .any(|e| e.visible && e.has_content && e.surface.wl_surface() == surface);
            }
        };
        (role.is_surface(surface) && role.buffer.is_some())
            || self
                .layer_surfaces
                .iter()
                .any(|e| e.visible && e.has_content && e.surface.wl_surface() == surface)
    }

    fn complete_visible_callbacks(&mut self, time_ms: u32) {
        let callbacks = std::mem::take(&mut self.frame_callbacks);
        for (surface, cb) in callbacks {
            if !surface.is_alive() || !cb.is_alive() {
                continue;
            }
            if self.surface_visible(&surface) {
                cb.done(time_ms);
            } else {
                self.frame_callbacks.push((surface, cb));
            }
        }
        self.last_callback = std::time::Instant::now();
    }

    fn has_visible_callbacks(&self) -> bool {
        self.frame_callbacks
            .iter()
            .any(|(surface, _)| self.surface_visible(surface))
    }

    fn app_unmapped(&mut self, surface: &WlSurface) {
        if self.focused_surface.as_ref() == Some(surface) {
            self.cancel_touches();
            self.focused_surface = self
                .app_surfaces
                .iter()
                .rev()
                .find(|e| e.buffer.is_some() && e.surface.is_alive())
                .map(|e| e.surface.clone());
            if self.shell_mode == ShellMode::App {
                if self.app_return_mode == ShellMode::Settings || self.focused_surface.is_none() {
                    let target = self.app_return_target();
                    self.switch_mode(target);
                }
                self.damage = true;
            }
        }
    }
}

#[cfg(test)]
mod tests;
