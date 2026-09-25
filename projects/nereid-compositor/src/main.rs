mod compose;
mod config_file;
mod gesture;
mod input;
mod pixels;
mod proxy;
mod role_input;
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
    /// Watchface role is visible.
    Watchface,
    /// Launcher role is visible.
    Launcher,
    /// Settings role is visible.
    Settings,
    /// A toplevel app is in the foreground.
    App,
}

/// Identifies a managed role slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleId {
    Watchface,
    Launcher,
    Settings,
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
        self.rx = None;
        self.surface = None;
        self.buffer = None;
        self.last_exit = None; // intentional kill, no backoff needed
    }
}

/// Control message from the control socket thread.
enum CtlMessage {
    SetRole { id: RoleId, command: Vec<String> },
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
    /// Launcher role slot.
    launcher: ManagedRole,
    /// Settings role slot.
    settings: ManagedRole,
    /// Current toplevel surface buffer (persists until replaced or removed).
    app_surfaces: Vec<AppSurface>,
    /// Frame callbacks to fire after rendering.
    frame_callbacks: Vec<(WlSurface, WlCallback)>,
    last_callback: std::time::Instant,
    touch_targets: std::collections::HashMap<u32, (WlSurface, MotionEvent)>,
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
    /// Last input activity timestamp.
    last_activity: std::time::Instant,
    /// Current shell mode.
    shell_mode: ShellMode,
    /// Display handle for Wayland client credential lookups.
    display_handle: DisplayHandle,
    /// Receiver for control socket messages.
    ctl_rx: mpsc::Receiver<CtlMessage>,
    wakeup: Arc<wakeup::Wakeup>,
    apps: Vec<Child>,
}

impl Compositor {
    fn new(
        display: &Display<Self>,
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

        let xdg_runtime =
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());

        Ok(Self {
            proxy,
            framebuffers: [fb0, fb1],
            write_buf_idx: 0,
            input_mgr,
            wayland,
            gesture,
            watchface: ManagedRole::new(RoleId::Watchface, config.watchface),
            launcher: ManagedRole::new(RoleId::Launcher, config.launcher),
            settings: ManagedRole::new(RoleId::Settings, config.settings),
            app_surfaces: Vec::new(),
            frame_callbacks: Vec::new(),
            last_callback: std::time::Instant::now(),
            touch_targets: std::collections::HashMap::new(),
            focused_surface: None,
            layer_surfaces: Vec::new(),
            display_width: width,
            display_height: height,
            display_on: true,
            running: true,
            xdg_runtime,
            damage: true,
            display_timeout_secs: config.display_timeout,
            last_activity: std::time::Instant::now(),
            shell_mode: initial_mode,
            display_handle: display.handle(),
            ctl_rx,
            wakeup,
            apps: Vec::new(),
        })
    }

    /// Set display power state via HWC proxy.
    /// When turning off: kills all children (watchface, launcher, settings, apps).
    /// When turning on: respawns watchface only (launcher/settings spawn on demand).
    fn set_display_power(&mut self, on: bool) {
        if on == self.display_on {
            return;
        }
        if let Err(e) = self.proxy.set_power(on) {
            warn!("Failed to set display power: {}", e);
            self.running = false;
            return;
        }
        self.cancel_touches();
        self.display_on = on;
        if on {
            info!("Display on — respawning watchface");
            let xdg = self.xdg_runtime.clone();
            spawn_role(&mut self.watchface, &xdg, &self.wakeup);
            self.switch_mode(if self.watchface.command.is_empty() {
                ShellMode::Launcher
            } else {
                ShellMode::Watchface
            });
            self.damage = true;
        } else {
            info!("Display off — killing all children");
            self.close_foreground_app();
            self.watchface.kill();
            self.launcher.kill();
            self.settings.kill();

            self.focused_surface = None;
            self.shell_mode = ShellMode::Watchface;
        }
        info!(on, "Display power changed");

        // Notify powerd for battery logging (fire-and-forget)
        let event: &str = if on { "display-on" } else { "display-off" };
        let event = event.to_string();
        std::thread::spawn(move || {
            notify_powerd(&event);
        });
    }

    /// Switch shell mode, lazily spawning the target role if needed.
    fn switch_mode(&mut self, mode: ShellMode) {
        if self.shell_mode != mode {
            info!(from = ?self.shell_mode, to = ?mode, "Shell mode changed");
            self.cancel_touches();
            self.damage = true;
        }
        self.shell_mode = mode;
        let xdg = self.xdg_runtime.clone();
        match mode {
            ShellMode::Launcher => {
                ensure_role_running(&mut self.launcher, &xdg, &self.wakeup);
            }
            ShellMode::Settings => {
                ensure_role_running(&mut self.settings, &xdg, &self.wakeup);
            }
            _ => {}
        }
    }

    fn is_role_surface(&self, surface: &WlSurface) -> bool {
        self.watchface.is_surface(surface)
            || self.launcher.is_surface(surface)
            || self.settings.is_surface(surface)
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
    fn spawn_app(&mut self, parts: &[String]) {
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
            }
            Err(e) => warn!(argv = ?parts, "Failed to spawn app: {}", e),
        }
    }

    /// Process a stdout message from any role.
    fn handle_role_message(&mut self, role: RoleId, msg: &str) {
        match msg {
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
                        self.spawn_app(&args)
                    }
                    _ => warn!("Invalid launcher argument vector"),
                }
            }
            _ if msg.starts_with("launch:") => {
                // Compatibility with older roles. New launcher sends an argument vector.
                match shell_words::split(&msg[7..]) {
                    Ok(args) => self.spawn_app(&args),
                    Err(e) => warn!(%e, "Invalid legacy launch command"),
                }
            }
            _ => warn!(?role, msg, "Unknown role message"),
        }
    }

    /// Process stdout messages from all role processes.
    fn process_role_messages(&mut self) {
        let wf_msgs = self.watchface.drain_messages();
        let launcher_msgs = self.launcher.drain_messages();
        let settings_msgs = self.settings.drain_messages();

        for msg in wf_msgs {
            self.handle_role_message(RoleId::Watchface, &msg);
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
    watchface: Vec<String>,
    launcher: Vec<String>,
    settings: Vec<String>,
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
                launcher: default_launcher,
                settings: default_settings,
                display_timeout: 0,
            };
        }
    };

    let mut watchface = None;
    let mut launcher = None;
    let mut settings = None;
    let mut display_timeout: u64 = 0;

    for line in content.lines() {
        let line = line.trim();
        if let Some(cmd) = line.strip_prefix("watchface=") {
            watchface = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("launcher=") {
            launcher = shell_words::split(cmd).ok();
        } else if let Some(cmd) = line.strip_prefix("settings=") {
            settings = shell_words::split(cmd).ok();
        } else if let Some(val) = line.strip_prefix("display_timeout=") {
            display_timeout = val.trim().parse().unwrap_or(0);
        }
    }

    Config {
        watchface: watchface.unwrap_or(default_wf),
        launcher: launcher.unwrap_or(default_launcher),
        settings: settings.unwrap_or(default_settings),
        display_timeout,
    }
}

fn write_config(
    watchface: &[String],
    launcher: &[String],
    settings: &[String],
) -> std::io::Result<()> {
    config_file::save_roles(&config_path(), watchface, launcher, settings)
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
    let mut config = read_config();
    let setter = [
        ("set-watchface ", RoleId::Watchface),
        ("set-launcher ", RoleId::Launcher),
        ("set-settings ", RoleId::Settings),
    ]
    .into_iter()
    .find_map(|(prefix, id)| cmd.strip_prefix(prefix).map(|rest| (id, rest)));
    if let Some((id, text)) = setter {
        let command = match shell_words::split(text) {
            Ok(command) => command,
            Err(e) => return format!("error: invalid command: {e}\n"),
        };
        match id {
            RoleId::Watchface => config.watchface = command.clone(),
            RoleId::Launcher => config.launcher = command.clone(),
            RoleId::Settings => config.settings = command.clone(),
        }
        if let Err(e) = write_config(&config.watchface, &config.launcher, &config.settings) {
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
        "get-launcher" => format!("{}\n", shell_words::join(&config.launcher)),
        "get-settings" => format!("{}\n", shell_words::join(&config.settings)),
        _ => "error: unknown command\n".into(),
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
    start_control_socket(ctl_tx, wakeup.clone());

    let mut compositor = Compositor::new(&display, ctl_rx, wakeup.clone())?;
    if let Some(keyboard) = compositor.wayland.seat.get_keyboard() {
        keyboard.set_keymap_from_string(&mut compositor, wayland::WATCH_KEYMAP.into())
            .context("watch button keymap")?;
    }

    // Spawn watchface only — launcher/settings spawn lazily on navigation
    let xdg_runtime = compositor.xdg_runtime.clone();
    spawn_role(&mut compositor.watchface, &xdg_runtime, &wakeup);

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
    info!("Entering epoll main loop");

    let mut prev_shell_mode = compositor.shell_mode;
    let mut epoll_events = [EpollEvent::empty(); 16];

    while compositor.running {
        // Compute epoll timeout
        let mut timeout = if !compositor.display_on {
            // Screen off: sleep indefinitely, only wake on input interrupt
            EpollTimeout::NONE
        } else if compositor.damage {
            // Pending damage: don't block, render immediately
            EpollTimeout::ZERO
        } else if compositor.display_timeout_secs > 0 {
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
                &compositor.watchface,
                &compositor.launcher,
                &compositor.settings,
            ] {
                let needed = role.id == RoleId::Watchface
                    || (role.id == RoleId::Launcher
                        && compositor.shell_mode == ShellMode::Launcher)
                    || (role.id == RoleId::Settings
                        && compositor.shell_mode == ShellMode::Settings);
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
        if compositor.display_on && compositor.has_visible_callbacks() {
            let remaining = std::time::Duration::from_millis(22)
                .saturating_sub(compositor.last_callback.elapsed());
            let ms = remaining.as_millis() as u32 + 1;
            if timeout == EpollTimeout::NONE || timeout.as_millis().unwrap_or(0) > ms {
                timeout = EpollTimeout::from(ms as u16);
            }
        }
        // These borrowed descriptors stay alive until the registrations are removed,
        // before dispatch can kill or replace a role. No EPOLLOUT interest when idle.
        let pending_inputs: Vec<_> = [
            &compositor.watchface,
            &compositor.launcher,
            &compositor.settings,
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
        let wait_result = epoll.wait(&mut epoll_events, timeout);
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
            &mut compositor.watchface,
            &mut compositor.launcher,
            &mut compositor.settings,
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
                        compositor: Default::default(),
                    }),
                )
                .context("Failed to insert client")?;
            info!("New Wayland client connected: {:?}", client.id());
        }

        // Process Wayland client events
        display
            .dispatch_clients(&mut compositor)
            .context("Wayland dispatch")?;
        display.flush_clients().context("Wayland flush")?;

        // Detect toplevel lifecycle changes
        if compositor.shell_mode == ShellMode::App && !compositor.has_live_toplevels() {
            info!("All toplevels closed, switching to Launcher mode");
            compositor.switch_mode(ShellMode::Launcher);
            compositor.focused_surface = None;
            compositor.launcher.send("app-closed");
        }

        compositor
            .frame_callbacks
            .retain(|(surface, cb)| surface.is_alive() && cb.is_alive());

        // Process role messages (screen-off requests, etc.)
        compositor.process_role_messages();

        // Respawn watchface if it crashed while display is on
        if compositor.display_on {
            ensure_role_running(&mut compositor.watchface, &xdg_runtime, &wakeup);
            match compositor.shell_mode {
                ShellMode::Launcher => {
                    ensure_role_running(&mut compositor.launcher, &xdg_runtime, &wakeup)
                }
                ShellMode::Settings => {
                    ensure_role_running(&mut compositor.settings, &xdg_runtime, &wakeup)
                }
                _ => {}
            }
        }

        // Process control socket messages (set-watchface, set-launcher)
        while let Ok(msg) = compositor.ctl_rx.try_recv() {
            match msg {
                CtlMessage::SetRole { id, command } => {
                    info!(role = ?id, cmd = ?command, "Setting role via control socket");
                    compositor.cancel_touches();
                    compositor.damage = true;
                    match id {
                        RoleId::Watchface => {
                            compositor.watchface.kill();
                            compositor.watchface = ManagedRole::new(RoleId::Watchface, command);
                            if compositor.display_on {
                                spawn_role(&mut compositor.watchface, &xdg_runtime, &wakeup);
                            }
                        }
                        RoleId::Launcher => {
                            compositor.launcher.kill();
                            compositor.launcher = ManagedRole::new(RoleId::Launcher, command);
                            if compositor.display_on && compositor.shell_mode == ShellMode::Launcher
                            {
                                spawn_role(&mut compositor.launcher, &xdg_runtime, &wakeup);
                            }
                        }
                        RoleId::Settings => {
                            compositor.settings.kill();
                            compositor.settings = ManagedRole::new(RoleId::Settings, command);
                            if compositor.display_on && compositor.shell_mode == ShellMode::Settings
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
                    if !compositor.display_on {
                        // Display is off — any button press wakes it (consumed, not forwarded)
                        if let input::InputEvent::Button(ref b) = ev {
                            if b.pressed {
                                info!(code = b.code, "Wake-up button press");
                                compositor.set_display_power(true);
                                compositor.last_activity = std::time::Instant::now();
                            }
                        }
                        continue;
                    }

                    // Any input resets the display timeout
                    compositor.last_activity = std::time::Instant::now();

                    // Display is on — normal input handling
                    match ev {
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

        // Flush input events to clients immediately
        display
            .flush_clients()
            .context("Wayland flush after input")?;

        // Display timeout
        if compositor.display_on && compositor.display_timeout_secs > 0 {
            if compositor.last_activity.elapsed().as_secs() >= compositor.display_timeout_secs {
                info!(timeout = compositor.display_timeout_secs, "Display timeout");
                compositor.set_display_power(false);
            }
        }

        if !compositor.running {
            anyhow::bail!("display connection failed");
        }

        compositor.watchface.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Watchface,
        );
        compositor.launcher.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Launcher,
        );
        compositor.settings.report_visibility(
            compositor.display_on && compositor.shell_mode == ShellMode::Settings,
        );

        // Skip rendering when display is off
        if !compositor.display_on {
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
            let app_buf = compositor
                .app_surfaces
                .iter()
                .find(|entry| compositor.focused_surface.as_ref() == Some(&entry.surface))
                .and_then(|entry| entry.buffer.as_ref());
            compose::composite_frame(
                compositor.framebuffers[idx].as_mut_slice(),
                compositor.display_width,
                compositor.display_height,
                compositor.watchface.buffer.as_ref(),
                compositor.launcher.buffer.as_ref(),
                compositor.settings.buffer.as_ref(),
                app_buf,
                &compositor.layer_surfaces,
                compositor.shell_mode,
            );
            let w = compositor.display_width;
            compositor
                .proxy
                .send_frame(
                    compositor.framebuffers[idx].fd,
                    w,
                    compositor.display_height,
                    w * 4,
                )
                .context("display submission failed; restarting compositor")?;
            compositor.write_buf_idx = 1 - idx;
            compositor.damage = false;
        }
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
    info!(code = button.code, pressed = button.pressed, mode = ?compositor.shell_mode, "Button event");
    if !button.pressed {
        return; // Only handle press, not release
    }

    match button.code {
        KEY_POWER => {
            // Crown button — global mode switch
            match compositor.shell_mode {
                ShellMode::App => {
                    info!("Crown in App mode: closing app, switching to Launcher");
                    compositor.close_foreground_app();
                    compositor.switch_mode(ShellMode::Launcher);
                    compositor.launcher.send("app-closed");
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
                ShellMode::Watchface => {
                    info!("Top: Watchface -> Settings");
                    compositor.switch_mode(ShellMode::Settings);
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
                ShellMode::Watchface => {
                    // Screen off
                    compositor.set_display_power(false);
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
    info!(v120 = scroll.v120, ticks, mode = ?compositor.shell_mode, "Scroll event");
    if ticks == 0 {
        return;
    }

    match compositor.shell_mode {
        ShellMode::Watchface => {
            compositor.watchface.send(&format!("scroll:{}", ticks));
        }
        ShellMode::Launcher => {
            compositor.launcher.send(&format!("scroll:{}", ticks));
        }
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
    if touch.state == input::TouchState::Down {
        info!(x = touch.x, y = touch.y, mode = ?compositor.shell_mode, "Touch down");
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
            .watchface
            .surface
            .as_ref()
            .map(|s| (s, compositor.watchface.buffer.as_ref())),
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

impl Drop for Compositor {
    fn drop(&mut self) {
        self.watchface.kill();
        self.launcher.kill();
        self.settings.kill();
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
        let role = match self.shell_mode {
            ShellMode::Watchface => &self.watchface,
            ShellMode::Launcher => &self.launcher,
            ShellMode::Settings => &self.settings,
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
                if self.focused_surface.is_none() {
                    self.switch_mode(ShellMode::Launcher);
                }
                self.damage = true;
            }
        }
    }
}

#[cfg(test)]
mod tests;
