//! Real protocol requests against a headless compositor; no input devices or GPU.
use super::*;
use std::os::unix::net::UnixStream;
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use wayland_client::protocol::{wl_pointer, wl_region, wl_seat, wl_touch};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as layer_shell, zwlr_layer_surface_v1 as layer_surface,
};

#[derive(Default)]
struct Client {
    capture: capture_protocol::ClientCapture,
    outputs: desktop_protocol::ClientOutputs,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    sync: usize,
    frames: usize,
    layers: Option<layer_shell::ZwlrLayerShellV1>,
    touches: Vec<&'static str>,
    clicks: usize,
    closed_layers: usize,
    closed_toplevels: usize,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_output_manager_v1" => state.outputs.manager = Some(registry.bind(name, 1, qh, ())),
                "wl_output" => state.capture.output = Some(registry.bind(name, 3, qh, ())),
                "ext_output_image_capture_source_manager_v1" => state.capture.source = Some(registry.bind(name, 1, qh, ())),
                "ext_image_copy_capture_manager_v1" => state.capture.manager = Some(registry.bind(name, 1, qh, ())),
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.shell = Some(registry.bind(name, version.min(3), qh, ())),
                "zwlr_layer_shell_v1" => {
                    state.layers = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = registry.bind(name, version.min(7), qh, ());
                    seat.get_touch(qh, ());
                    seat.get_pointer(qh, ());
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}
impl Dispatch<wl_callback::WlCallback, bool> for Client {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        frame: &bool,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *frame {
            state.frames += 1;
        } else {
            state.sync += 1;
        }
    }
}
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(state: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Close = event { state.closed_toplevels += 1; }
    }
}

struct Harness {
    display: Display<Compositor>,
    compositor: Compositor,
    conn: Connection,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    _proxy: UnixStream,
}

#[test]
fn ambient_and_screen_off_preserve_app_surface_focus_and_buffer() {
    use std::io::{Read,Write};
    let mut h=Harness::new();
    let(surface,_,_)=h.window();h.map(&surface,17);
    let focused=h.compositor.focused_surface.clone();
    let mut peer=h._proxy.try_clone().unwrap();
    let hardware=std::thread::spawn(move || {
        for mode in [3,2,0,2] {
            let mut header=[0u8;5];peer.read_exact(&mut header).unwrap();
            assert_eq!(header[4],0x04);
            let mut payload=vec![0;u32::from_le_bytes(header[..4].try_into().unwrap()) as usize-5];
            peer.read_exact(&mut payload).unwrap();assert_eq!(payload[0],mode);
            peer.write_all(&[6,0,0,0,0x84,0]).unwrap();
        }
    });
    for target in ["ambient","interactive","off","interactive"] {
        h.compositor.change_display(target);
        assert_eq!(h.compositor.focused_surface,focused);
        assert_eq!(h.compositor.shell_mode,ShellMode::App);
        assert!(h.compositor.has_live_toplevels());
        assert_eq!(h.compositor.app_surfaces[0].buffer.as_ref().unwrap().data[0],17);
    }
    hardware.join().unwrap();
}
fn expect_display_modes(h: &Harness, modes: Vec<u8>) -> std::thread::JoinHandle<()> {
    use std::io::{Read, Write};
    let mut peer = h._proxy.try_clone().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
    std::thread::spawn(move || {
        for mode in modes {
            let mut header = [0u8; 5];
            peer.read_exact(&mut header).unwrap();
            assert_eq!(header[4], 0x04);
            let mut payload = vec![0; u32::from_le_bytes(header[..4].try_into().unwrap()) as usize - 5];
            peer.read_exact(&mut payload).unwrap();
            assert_eq!(payload[0], mode);
            peer.write_all(&[6, 0, 0, 0, 0x84, 0]).unwrap();
        }
    })
}

#[test]
fn primary_and_ambient_watchfaces_share_button_actions() {
    for ambient in [false, true] {
        for secondary_only in [false, true] {
            for with_agent in [false, true] {
                for (code, expected_mode, expected_on) in [
                    (KEY_VOLUMEUP, ShellMode::Settings, true),
                    (KEY_POWER, ShellMode::Launcher, true),
                    (KEY_VOLUMEDOWN, ShellMode::Watchface, false),
                ] {
                    let mut h = Harness::new();
                    h.compositor.shell_mode = ShellMode::Watchface;
                    h.compositor.ambient = ambient;
                    h.compositor.display_on = !ambient;
                    h.compositor.secondary_only = secondary_only;
                    if with_agent {
                        h.compositor.agent.command = vec!["/unused-test-agent".into()];
                    }
                    let mut modes = if ambient { vec![2] } else { vec![] };
                    if !expected_on { modes.push(0); }
                    let hardware = expect_display_modes(&h, modes);
                    handle_button(&mut h.compositor, &input::ButtonEvent { code, pressed: true }, 1);
                    handle_button(&mut h.compositor, &input::ButtonEvent { code, pressed: false }, 2);
                    assert_eq!(h.compositor.shell_mode, expected_mode,
                        "ambient={ambient}, secondary_only={secondary_only}, agent={with_agent}, key={code}");
                    assert_eq!(h.compositor.display_on, expected_on);
                    assert!(!h.compositor.ambient);
                    assert_eq!(h.compositor.manual_off, !expected_on);
                    hardware.join().unwrap();
                }
            }
        }
    }
}

#[test]
fn dark_display_consumes_wake_buttons_and_releases() {
    for secondary_only in [false, true] {
        for code in [KEY_VOLUMEUP, KEY_POWER, KEY_VOLUMEDOWN] {
            let mut h = Harness::new();
            h.compositor.shell_mode = ShellMode::Watchface;
            h.compositor.display_on = false;
            h.compositor.manual_off = true;
            h.compositor.secondary_only = secondary_only;
            h.compositor.agent.command = vec!["/unused-test-agent".into()];
            let hardware = expect_display_modes(&h, vec![2]);
            // A release by itself must not wake the dark display.
            handle_button(&mut h.compositor, &input::ButtonEvent { code, pressed: false }, 0);
            assert!(!h.compositor.display_on);
            handle_button(&mut h.compositor, &input::ButtonEvent { code, pressed: true }, 1);
            handle_button(&mut h.compositor, &input::ButtonEvent { code, pressed: false }, 2);
            assert_eq!(h.compositor.shell_mode,
                if secondary_only { ShellMode::Launcher } else { ShellMode::Watchface });
            assert!(h.compositor.display_on);
            assert!(!h.compositor.manual_off);
            hardware.join().unwrap();
        }
    }
}

#[test]
fn manual_screen_off_stays_dark_while_coordinator_reply_catches_up() {
    let mut h = Harness::new();
    h.compositor.shell_mode = ShellMode::Watchface;
    h.compositor.sleep_bridge.set_reply_for_test(serde_json::json!({
        "display":"interactive", "config":{"enabled":true}, "generation":1,
        "_request":{"idle":30.,"foreground":false,"manual_off":false}
    }));
    let hardware = expect_display_modes(&h, vec![0]);
    handle_button(&mut h.compositor, &input::ButtonEvent { code: KEY_VOLUMEDOWN, pressed: true }, 1);
    for _ in 0..3 {
        h.compositor.reconcile_sleep();
        assert!(h.compositor.running, "stale reply must not attempt to reacquire the wake inhibitor");
        assert!(!h.compositor.display_on);
        assert!(h.compositor.manual_off);
    }
    h.compositor.sleep_bridge.set_reply_for_test(serde_json::json!({
        "display":"off", "config":{"enabled":true}, "generation":1,
        "_request":{"idle":0.,"foreground":false,"manual_off":true}
    }));
    h.compositor.reconcile_sleep();
    assert!(h.compositor.running);
    assert!(!h.compositor.display_on);
    hardware.join().unwrap();
}

#[test]
fn secondary_return_presents_companion_before_handoff_without_wake_delay() {
    let mut h = Harness::new();
    h.compositor.placeholder.face = "hoki-digital".into();
    h.compositor.placeholder.role.child_pid = Some(std::process::id());
    let (surface, _, _) = h.window();
    h.map(&surface, 71);
    assert!(h.compositor.app_surfaces.is_empty(), "companion must be claimed as a role");
    // Represent the already-held interactive inhibitor without a live powerd.
    let path = std::env::temp_dir().join(format!("hoki-secondary-test-{}", std::process::id()));
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    h.compositor.interactive_inhibitor = Some(sleep_client::Client::connect_to(path.to_str().unwrap()).unwrap());
    let (_inhibitor_peer, _) = listener.accept().unwrap();
    std::fs::remove_file(path).unwrap();
    h.compositor.shell_mode = ShellMode::Launcher;
    h.compositor.note_activity();
    h.compositor.display_config.face_mode="secondary".into();
    let mut reply = serde_json::json!({"ok":true, "inhibitors":[]});
    h.compositor.sleep_bridge.set_reply_for_test(reply.clone());
    h.compositor.switch_mode(ShellMode::Watchface);
    h.compositor.reconcile_sleep();
    assert!(h.compositor.placeholder.visible, "show companion before physical handoff");
    assert_eq!(h.compositor.visible_watchface().buffer.as_ref().unwrap().data[0], 71);
    assert!(h.compositor.display_on);
    assert!(h.compositor.running);

    h.compositor.switch_mode(ShellMode::Launcher);
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.placeholder.visible, "navigation cancels the placeholder");
    assert!(!h.compositor.placeholder.presented);
    h.compositor.switch_mode(ShellMode::Watchface);
    h.compositor.reconcile_sleep();
    assert!(h.compositor.placeholder.visible);

    // A display inhibitor must still allow the primary face.
    reply["inhibitors"]=serde_json::json!([{"display":true}]);
    h.compositor.sleep_bridge.set_reply_for_test(reply.clone());
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.placeholder.visible);
    assert!(h.compositor.display_on);

    reply["inhibitors"] = serde_json::json!([]);
    h.compositor.sleep_bridge.set_reply_for_test(reply);
    h.compositor.crown_press.press(std::time::Instant::now(), false);
    h.compositor.reconcile_sleep();
    assert!(h.compositor.placeholder.visible, "show companion until the crown gesture resolves");
    assert!(h.compositor.display_on);
    assert!(h.compositor.crown_press.release());
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.ambient, "a committed Wayland buffer alone is not a presented frame");
    use std::os::fd::AsRawFd;
    use crate::proxy::protocol::*;
    let peer = h._proxy.try_clone().unwrap();
    let hardware = std::thread::spawn(move || {
        let (kind, _, frame) = recv_fd(peer.as_raw_fd()).unwrap();
        assert_eq!(kind, MSG_FRAME);
        assert!(frame.is_some());
        send_raw(peer.as_raw_fd(), MSG_SYNC, &[]).unwrap();
        let (kind, payload, _) = recv_fd(peer.as_raw_fd()).unwrap();
        assert_eq!(kind, MSG_DISPLAY);
        assert_eq!(payload[0], 3);
        send_raw(peer.as_raw_fd(), MSG_DISPLAY_RESULT, &[0]).unwrap();
    });
    h.compositor.present_composited_frame(0).unwrap();
    assert!(h.compositor.placeholder.presented);
    h.compositor.last_activity = std::time::Instant::now();
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.placeholder.visible);
    assert!(h.compositor.ambient, "secondary selection hands off immediately after presentation");
    assert!(!h.compositor.display_on);
    hardware.join().unwrap();
}

#[test]
fn missing_companion_times_out_to_interactive_without_uploading() {
    for mode in ["secondary", "automatic"] {
        let mut h = Harness::new();
        h.compositor.shell_mode = ShellMode::Watchface;
        h.compositor.display_config.face_mode=mode.into();
        h.compositor.last_activity=std::time::Instant::now()-std::time::Duration::from_secs(40);
        h.compositor.placeholder.face = "hoki-digital".into();
        h.compositor.show_placeholder(true);
        h.compositor.placeholder.since = Some(std::time::Instant::now() - std::time::Duration::from_secs(4));
        h.compositor.reconcile_sleep();
        assert!(h.compositor.ambient_failed);
        assert!(!h.compositor.placeholder.visible);
        assert!(h.compositor.display_on);
        assert!(!h.compositor.ambient);
        assert!(std::ptr::eq(h.compositor.visible_watchface(), &h.compositor.watchface));
    }
}

impl Harness {
    fn new() -> Self { Self::with_size(4, 4) }
    fn with_size(width: u32, height: u32) -> Self { Self::with_domain(width, height, false) }
    fn with_domain(width: u32, height: u32, desktop: bool) -> Self {
        let display = Display::new().unwrap();
        let (proxy, peer) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        let compositor = Compositor {
            proxy: proxy::ProxyClient::for_test(proxy),
            framebuffers: [
                compose::MemfdBuffer::new(width, height).unwrap(),
                compose::MemfdBuffer::new(width, height).unwrap(),
            ],
            write_buf_idx: 0,
            input_mgr: input::InputManager::for_test(),
            wayland: wayland::WaylandState::new(&display, width, height),
            gesture: gesture::GestureRecognizer::new(width, height),
            lock_screen: ManagedRole::new(RoleId::LockScreen, vec![]),
            watchface: ManagedRole::new(RoleId::Watchface, vec![]),
            launcher: ManagedRole::new(RoleId::Launcher, vec![]),
            settings: ManagedRole::new(RoleId::Settings, vec![]),
            agent: ManagedRole::new(RoleId::Agent, vec![]),
            overlay: ManagedRole::new(RoleId::Overlay, vec![]),
            agent_return: ShellMode::Watchface,
            crown_press: Default::default(),
            app_surfaces: vec![],
            frame_callbacks: vec![],
            last_callback: std::time::Instant::now(),
            touch_targets: Default::default(),
            swallowed_touch_slots: Default::default(),
            focused_surface: None,
            layer_surfaces: vec![],
            display_width: width,
            display_height: height,
            display_on: true,
            running: true,
            xdg_runtime: String::new(),
            damage: false,
            ambient:false,manual_off:false,power_coordination:false,display_config:display_policy::Config::default(),activity_revision:0,
            sleep_bridge:sleep::Bridge::new(),interactive_inhibitor:None,ambient_face:String::new(),secondary_only:false,placeholder:AmbientPlaceholder::new(),ambient_failed:false,
            last_activity: std::time::Instant::now(),
            shell_mode: ShellMode::Launcher,
            lock_return_mode: ShellMode::Launcher,
            lock_enabled: false,
            auth_monitor_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            auth_owner_available: false,
            auth_property_locked: true,
            auth_owner: None,
            auth_state_resolved: true,
            locked: false,
            locked_watchface_selected: false,
            app_return_mode: ShellMode::Launcher,
            display_handle: display.handle(),
            ctl_rx: rx,
            ctl_tx: tx,
            wakeup: Arc::new(wakeup::Wakeup::new().unwrap()),
            apps: vec![],
        };
        let (server, client) = UnixStream::pair().unwrap();
        display
            .handle()
            .insert_client(
                server,
                Arc::new(wayland::ClientState {
                    desktop,
                    compositor: Default::default(),
                }),
            )
            .unwrap();
        let conn = Connection::from_socket(client).unwrap();
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let mut h = Self {
            display,
            compositor,
            conn,
            queue,
            client: Client::default(),
            _proxy: peer,
        };
        h.pump();
        h.pump();
        h
    }
    fn pump(&mut self) {
        let next = self.client.sync + 1;
        self.conn.display().sync(&self.queue.handle(), false);
        self.conn.flush().unwrap();
        self.display.dispatch_clients(&mut self.compositor).unwrap();
        self.display.flush_clients().unwrap();
        while self.client.sync < next {
            self.queue.blocking_dispatch(&mut self.client).unwrap();
        }
    }
    fn window(
        &mut self,
    ) -> (
        wl_surface::WlSurface,
        xdg_surface::XdgSurface,
        xdg_toplevel::XdgToplevel,
    ) {
        let qh = self.queue.handle();
        let s = self
            .client
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let x = self
            .client
            .shell
            .as_ref()
            .unwrap()
            .get_xdg_surface(&s, &qh, ());
        let t = x.get_toplevel(&qh, ());
        s.commit();
        self.pump();
        self.pump();
        (s, x, t)
    }
    fn map(&mut self, s: &wl_surface::WlSurface, pixel: u8) {
        use std::os::fd::BorrowedFd;
        let mut mem = compose::MemfdBuffer::new(4, 4).unwrap();
        mem.as_mut_slice().fill(pixel);
        let qh = self.queue.handle();
        let pool = self.client.shm.as_ref().unwrap().create_pool(
            unsafe { BorrowedFd::borrow_raw(mem.fd) },
            64,
            &qh,
            (),
        );
        let buffer = pool.create_buffer(0, 4, 4, 16, wl_shm::Format::Xrgb8888, &qh, ());
        s.attach(Some(&buffer), 0, 0);
        s.damage(0, 0, 4, 4);
        s.commit();
        self.pump();
        buffer.destroy();
        pool.destroy();
        self.pump();
    }
}

#[test]
fn app_mapping_focus_and_background_redraw_are_independent() {
    let mut h = Harness::new();
    let (a, _, _) = h.window();
    assert_eq!(
        h.compositor.shell_mode,
        ShellMode::Launcher,
        "empty commits must not steal focus"
    );
    h.map(&a, 17);
    let a_server = h.compositor.focused_surface.clone().unwrap();
    let (b, _, _) = h.window();
    assert_eq!(h.compositor.focused_surface.as_ref(), Some(&a_server));
    h.map(&b, 33);
    let b_server = h.compositor.focused_surface.clone().unwrap();
    assert_ne!(a_server, b_server);
    h.compositor.damage = false;
    h.map(&a, 55);
    assert_eq!(h.compositor.focused_surface.as_ref(), Some(&b_server));
    assert!(
        !h.compositor.damage,
        "hidden app redraw must not repaint display"
    );
    assert_eq!(
        h.compositor.app_surfaces[0].buffer.as_ref().unwrap().data[0],
        55
    );
    b.attach(None, 0, 0);
    b.commit();
    h.pump();
    assert_eq!(h.compositor.focused_surface.as_ref(), Some(&a_server));
    assert_eq!(h.compositor.shell_mode, ShellMode::App);
    h.compositor.switch_mode(ShellMode::Launcher);
    h.map(&a, 66);
    assert_eq!(h.compositor.shell_mode, ShellMode::Launcher);
}

#[test]
fn final_app_surface_returns_to_settings_when_settings_launched_it() {
    let mut h = Harness::new();
    h.compositor.settings.command = vec!["/bin/true".into()];
    h.compositor.app_return_mode = ShellMode::Settings;
    let (surface, _, _) = h.window();
    h.map(&surface, 42);
    assert_eq!(h.compositor.shell_mode, ShellMode::App);

    surface.attach(None, 0, 0);
    surface.commit();
    h.pump();

    assert_eq!(h.compositor.shell_mode, ShellMode::Settings);
}

#[test]
fn settings_origin_app_close_returns_to_settings_over_older_buffered_app() {
    let mut h = Harness::new();
    h.compositor.settings.command = vec!["/bin/true".into()];

    let (older, _, _) = h.window();
    h.map(&older, 21);
    let older_server = h.compositor.focused_surface.clone().unwrap();
    h.compositor.switch_mode(ShellMode::Settings);

    let (settings_app, _, _) = h.window();
    h.compositor.app_return_mode = ShellMode::Settings;
    h.map(&settings_app, 42);
    assert_eq!(h.compositor.shell_mode, ShellMode::App);
    assert_eq!(h.compositor.app_surfaces.len(), 2);

    settings_app.attach(None, 0, 0);
    settings_app.commit();
    h.pump();

    assert_eq!(h.compositor.shell_mode, ShellMode::Settings);
    assert_eq!(h.compositor.focused_surface, Some(older_server));
    assert!(h.compositor.app_surfaces.iter().any(|entry| {
        entry.surface.is_alive() && entry.buffer.is_some()
    }));
}

#[test]
fn callback_only_commit_is_kept_and_surface_removal_cancels_touch() {
    let mut h = Harness::new();
    let (s, _, top) = h.window();
    h.map(&s, 255);
    h.compositor.damage = false;
    s.frame(&h.queue.handle(), true);
    s.commit();
    h.pump();
    assert!(!h.compositor.damage);
    assert!(h.compositor.has_visible_callbacks());
    h.compositor.complete_visible_callbacks(1);
    h.pump();
    assert_eq!(
        h.client.frames, 1,
        "callback-only commit completes without repainting"
    );
    s.frame(&h.queue.handle(), true);
    s.commit();
    h.pump();
    let surface = h.compositor.focused_surface.clone().unwrap();
    h.compositor.touch_targets.insert(
        0,
        (
            surface,
            MotionEvent {
                slot: TouchSlot::from(Some(0)),
                location: (1.0, 1.0).into(),
                time: 1,
            },
        ),
    );
    top.destroy();
    h.pump();
    assert!(h.compositor.touch_targets.is_empty());
    assert!(h.compositor.frame_callbacks.is_empty());
    assert!(h.compositor.damage);
}

impl Dispatch<wl_touch::WlTouch, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { .. } => state.touches.push("down"),
            wl_touch::Event::Up { .. } => state.touches.push("up"),
            wl_touch::Event::Motion { .. } => state.touches.push("motion"),
            wl_touch::Event::Cancel => state.touches.push("cancel"),
            _ => {}
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { .. } = event {
            state.clicks += 1;
        }
    }
}
impl Dispatch<layer_surface::ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        s: &layer_surface::ZwlrLayerSurfaceV1,
        event: layer_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let layer_surface::Event::Configure { serial, .. } = event {
            s.ack_configure(serial);
        } else if let layer_surface::Event::Closed = event {
            state.closed_layers += 1;
        }
    }
}
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore wl_region::WlRegion);
delegate_noop!(Client: ignore layer_shell::ZwlrLayerShellV1);

#[test]
fn native_touch_is_delivered_once_and_cancelled_on_navigation() {
    let mut h = Harness::new();
    let (s, _, _) = h.window();
    h.map(&s, 255);
    for state in [
        input::TouchState::Down,
        input::TouchState::Motion,
        input::TouchState::Up,
    ] {
        forward_touch(
            &mut h.compositor,
            &input::TouchEvent {
                slot: 0,
                x: 1.0,
                y: 1.0,
                state,
            },
            1,
        );
    }
    h.pump();
    assert_eq!(h.client.touches, ["down", "motion", "up"]);
    assert_eq!(h.client.clicks, 0);
    forward_touch(
        &mut h.compositor,
        &input::TouchEvent {
            slot: 0,
            x: 1.0,
            y: 1.0,
            state: input::TouchState::Down,
        },
        2,
    );
    h.compositor.switch_mode(ShellMode::Launcher);
    h.pump();
    assert_eq!(&h.client.touches[3..], &["down", "motion", "cancel"]);
}

#[test]
fn transparent_input_layer_passes_through_and_removal_preserves_app_callbacks() {
    let mut h = Harness::new();
    let (s, _, _) = h.window();
    h.map(&s, 255);
    let app = h.compositor.focused_surface.clone().unwrap();
    let qh = h.queue.handle();
    let overlay = h
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let layer = h.client.layers.as_ref().unwrap().get_layer_surface(
        &overlay,
        None,
        layer_shell::Layer::Overlay,
        "test".into(),
        &qh,
        (),
    );
    layer.set_size(4, 4);
    overlay.commit();
    h.pump();
    h.pump();
    h.map(&overlay, 128);
    assert_ne!(
        find_touch_target(&h.compositor, 1.0, 1.0),
        Some(app.clone())
    );
    let region = h.client.compositor.as_ref().unwrap().create_region(&qh, ());
    overlay.set_input_region(Some(&region));
    overlay.commit();
    h.pump();
    assert_eq!(
        find_touch_target(&h.compositor, 1.0, 1.0),
        Some(app.clone())
    );
    s.frame(&qh, true);
    s.commit();
    overlay.frame(&qh, true);
    overlay.commit();
    h.pump();
    h.compositor.damage = false;
    layer.destroy();
    h.pump();
    assert!(h.compositor.damage);
    assert_eq!(h.compositor.frame_callbacks.len(), 1);
    assert_eq!(h.compositor.frame_callbacks[0].0, app);
}

#[test]
fn managed_roles_deliver_stdout_and_reap_exits() {
    use std::os::fd::AsRawFd;
    for id in [RoleId::Watchface, RoleId::Launcher, RoleId::Settings, RoleId::Agent, RoleId::Overlay] {
        let wake = Arc::new(wakeup::Wakeup::new().unwrap());
        let mut role = ManagedRole::new(
            id,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'go-launcher\n'".into(),
            ],
        );
        spawn_role(&mut role, "/tmp", &wake);
        let mut poll = libc::pollfd {
            fd: wake.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 1000) }, 1);
        role.process.as_mut().unwrap().wait().unwrap();
        assert!(!role.check_alive());
        assert_eq!(
            role.drain_messages(),
            ["go-launcher"],
            "reaping must retain final commands"
        );
        assert!(role.process.is_none() && role.child_pid.is_none());
        ensure_role_running(&mut role, "/tmp", &wake);
        assert!(role.process.is_none(), "respect restart backoff");
    }
}

#[test]
fn settings_argument_vector_reaches_child_and_keeps_its_return_mode() {
    let mut h = Harness::new();
    let path = std::env::temp_dir().join(format!("hoki-argv-{}", std::process::id()));
    let args: Vec<String> = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s\\n' \"$@\" > \"$1\"".into(),
        "probe".into(),
        path.to_string_lossy().into_owned(),
        "two words".into(),
        "".into(),
        "%literal".into(),
        "quote'and\"double".into(),
        "$(literal)".into(),
    ];
    h.compositor.handle_role_message(
        RoleId::Settings,
        &format!("launch-argv:{}", serde_json::to_string(&args).unwrap()),
    );
    let launch = h.compositor.ctl_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    match launch {
        CtlMessage::LaunchApp { args, return_mode } => {
            assert_eq!(return_mode, ShellMode::Settings);
            h.compositor.spawn_app(&args, return_mode);
        }
        CtlMessage::SetRole { .. } | CtlMessage::ScreenOff | CtlMessage::AuthState(_) | CtlMessage::DisplayRequest { .. } => {
            panic!("unexpected control message")
        }
    }
    assert_eq!(h.compositor.apps.len(), 1);
    assert!(h.compositor.apps[0].wait().unwrap().success());
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        args[4..].iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(h.compositor.app_return_mode, ShellMode::Settings);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn role_visibility_is_sent_once_per_transition_and_after_respawn() {
    let wake = Arc::new(wakeup::Wakeup::new().unwrap());
    let mut role = ManagedRole::new(RoleId::Settings, vec!["/bin/sh".into(), "-c".into(),
        "test \"$HOKI_MANAGED_ROLE\" = 1 || exit 2; while IFS= read -r line; do printf '%s\\n' \"$line\"; done".into()]);
    for _ in 0..2 {
        spawn_role(&mut role, "/tmp", &wake);
        role.report_lock_state(true);
        for _ in 0..100 {
            role.report_lock_state(true);
        }
        role.report_visibility(false);
        for _ in 0..100 {
            role.report_visibility(false);
        }
        role.report_lock_state(false);
        for _ in 0..100 {
            role.report_lock_state(false);
        }
        role.report_visibility(true);
        for _ in 0..100 {
            role.report_visibility(true);
        }
        let rx = role.rx.as_ref().unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "lock-state:locked"
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "visibility:hidden"
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "lock-state:unlocked"
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "visibility:visible"
        );
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(25))
            .is_err());
        role.kill();
    }
}

#[test]
fn assistant_dismiss_restores_previous_app_and_does_not_close_it() {
    let mut h = Harness::new();
    let (surface, _, _top) = h.window();
    h.map(&surface, 17);
    h.pump();
    assert_eq!(h.compositor.shell_mode, ShellMode::App);
    h.compositor.agent.command = vec!["/nonexistent-test-assistant".into()];
    h.compositor.activate_agent();
    assert_eq!(h.compositor.shell_mode, ShellMode::Agent);
    assert!(h.compositor.has_live_toplevels());
    h.compositor.handle_role_message(RoleId::Agent, "dismiss");
    assert_eq!(h.compositor.shell_mode, ShellMode::App);
    assert!(h.compositor.has_live_toplevels());
}

#[test]
fn assistant_owns_surface_and_hidden_app_redraw_does_not_dismiss_it() {
    let mut h = Harness::new();
    let (app, _, _) = h.window(); h.map(&app, 17);
    let app_focus=h.compositor.focused_surface.clone();
    h.compositor.agent.child_pid=Some(std::process::id());
    let (agent, _, _) = h.window(); h.map(&agent, 33);
    assert!(h.compositor.agent.surface.is_some());
    assert_eq!(h.compositor.app_surfaces.len(),1);
    h.compositor.agent_return=ShellMode::App;
    h.compositor.shell_mode=ShellMode::Agent;
    h.compositor.damage=false;
    h.map(&app,55);
    assert_eq!(h.compositor.shell_mode,ShellMode::Agent);
    assert!(!h.compositor.damage);
    h.compositor.dismiss_agent();
    assert_eq!(h.compositor.focused_surface,app_focus);
}

#[path = "capture/protocol_tests.rs"]
mod capture_protocol;

#[test]
fn remote_screen_off_uses_manual_display_transition() {
    let mut h = Harness::new();
    let hardware = expect_display_modes(&h, vec![0]);
    assert_eq!(process_ctl_command("screen-off", &h.compositor.ctl_tx), "ok\n");
    match h.compositor.ctl_rx.try_recv().unwrap() {
        CtlMessage::ScreenOff => h.compositor.set_display_power(false),
        _ => panic!("unexpected control message"),
    }
    assert!(!h.compositor.display_on);
    assert!(h.compositor.manual_off);
    // Repeated requests remain off and do not send another hardware transition.
    h.compositor.set_display_power(false);
    hardware.join().unwrap();
}

#[test]
fn remote_screen_off_reports_disconnected_compositor() {
    let (tx, rx) = mpsc::channel();
    drop(rx);
    assert_eq!(process_ctl_command("screen-off", &tx), "error: compositor unavailable\n");
}

#[test]
fn configured_lock_role_cannot_be_cleared_by_an_empty_control_command() {
    assert!(!valid_lock_screen_command(&[]));
    assert!(!valid_lock_screen_command(&[String::new()]));
    assert!(valid_lock_screen_command(&["/usr/bin/lock-renderer".into()]));
}

#[path = "desktop/protocol_tests.rs"]
mod desktop_protocol;


#[test]
fn overlay_role_is_single_layer_survives_home_and_does_not_take_focus() {
    let mut h = Harness::new();
    let (app, _, _) = h.window();
    h.map(&app, 255);
    let focused = h.compositor.focused_surface.clone();
    h.compositor.overlay.child_pid = Some(std::process::id());
    let qh = h.queue.handle();
    let make_layer = |h: &Harness| {
        let surface = h.client.compositor.as_ref().unwrap().create_surface(&qh, ());
        let layer = h.client.layers.as_ref().unwrap().get_layer_surface(
            &surface, None, layer_shell::Layer::Overlay, "hoki-overlay".into(), &qh, ());
        layer.set_size(4, 4);
        let region = h.client.compositor.as_ref().unwrap().create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        surface.commit();
        (surface, layer)
    };
    let (surface, layer) = make_layer(&h);
    h.pump(); h.pump(); h.map(&surface, 128);
    assert!(h.compositor.overlay.surface.is_some());
    assert_eq!(h.compositor.app_surfaces.len(), 1);
    assert_eq!(h.compositor.focused_surface, focused);
    assert_eq!(find_touch_target(&h.compositor, 1., 1.), focused);
    let (_duplicate_surface, _duplicate_layer) = make_layer(&h);
    h.pump(); h.pump();
    assert_eq!(h.client.closed_layers, 1);
    assert_eq!(h.compositor.layer_surfaces.len(), 1);
    handle_button(&mut h.compositor, &input::ButtonEvent {code: KEY_POWER, pressed: true}, 1);
    h.pump();
    assert_eq!(h.compositor.shell_mode, ShellMode::Launcher);
    assert_eq!(h.client.closed_toplevels, 1, "Home must close app");
    assert_eq!(h.client.closed_layers, 1, "Home must not close overlay");
    assert!(h.compositor.layer_surfaces[0].has_content);
    layer.destroy(); h.pump();
    assert!(h.compositor.overlay.surface.is_none());
    let (_replacement_surface, _replacement_layer) = make_layer(&h);
    h.pump(); h.pump();
    assert!(h.compositor.overlay.surface.is_some());
    assert_eq!(h.client.closed_layers, 1, "replacement after destroy must be accepted");
}

#[test]
fn auth_owner_loss_locks_and_shell_shortcuts_cannot_leave_lock_mode() {
    let mut h = Harness::new();
    h.compositor.lock_screen.command = vec!["/usr/lib/lock-screen".into()];
    h.compositor.lock_enabled = true;
    h.compositor.auth_monitor_active.store(true, std::sync::atomic::Ordering::Release);

    h.compositor.update_auth_state(auth::AuthState {
        owner: Some(":1.40".into()),
        enrolled: true,
        locked: false,
    });
    assert!(!h.compositor.is_locked());
    h.compositor.switch_mode(ShellMode::Settings);
    assert_eq!(h.compositor.shell_mode, ShellMode::Settings);

    h.compositor.update_auth_state(auth::AuthState {
        owner: None,
        enrolled: true,
        locked: true,
    });
    assert!(h.compositor.auth_state_ready());
    assert!(h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    h.compositor.switch_mode(ShellMode::Launcher);
    h.compositor.handle_role_message(RoleId::Overlay, "go-launcher");
    h.compositor.handle_role_message(RoleId::LockScreen, "go-launcher");
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        1,
    );
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);

    h.compositor.update_auth_state(auth::AuthState {
        owner: Some(":1.41".into()),
        enrolled: true,
        locked: false,
    });
    assert!(!h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::Settings);
}

#[test]
fn known_no_pin_unlocks_without_starting_lock_renderer() {
    let mut h = Harness::new();
    h.compositor.lock_screen.command = vec!["/bin/sleep".into(), "5".into()];
    h.compositor.lock_enabled = true;
    h.compositor.locked = true;
    h.compositor.shell_mode = ShellMode::LockScreen;
    h.compositor.lock_return_mode = ShellMode::Watchface;
    h.compositor.auth_state_resolved = false;

    let wake = h.compositor.wakeup.clone();
    start_initial_roles(&mut h.compositor, "/tmp", &wake);
    assert!(!h.compositor.auth_state_ready());
    assert!(h.compositor.is_locked());
    assert!(h.compositor.lock_screen.process.is_none());
    h.compositor.update_auth_state(auth::AuthState {
        owner: Some(":1.50".into()),
        enrolled: false,
        locked: false,
    });
    start_initial_roles(&mut h.compositor, "/tmp", &wake);

    assert!(h.compositor.auth_state_ready());
    assert!(!h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::Watchface);
    assert!(h.compositor.lock_screen.process.is_none());
    assert!(h.compositor.lock_screen.surface.is_none());
}

#[test]
fn disabling_lock_role_keeps_auth_errors_from_locking_compositor() {
    let mut h = Harness::new();
    h.compositor.lock_enabled = false;
    h.compositor.locked = false;
    h.compositor.shell_mode = ShellMode::Settings;

    h.compositor.update_auth_state(auth::AuthState {
        owner: None,
        enrolled: true,
        locked: true,
    });

    assert!(h.compositor.auth_state_ready());
    assert!(!h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::Settings);
}

#[test]
fn lock_renderer_exit_keeps_compositor_locked() {
    let mut h = Harness::new();
    h.compositor.lock_screen.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "exit 0".into(),
    ];
    h.compositor.lock_enabled = true;
    h.compositor.refresh_lock_state();
    assert!(h.compositor.is_locked());
    let wake = h.compositor.wakeup.clone();
    spawn_role(&mut h.compositor.lock_screen, "/tmp", &wake);
    h.compositor.lock_screen.process.as_mut().unwrap().wait().unwrap();
    assert!(!h.compositor.lock_screen.check_alive());
    assert!(h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
}

#[test]
fn locked_touch_never_falls_through_to_a_focused_app() {
    let mut h = Harness::new();
    let (app, _, _) = h.window();
    h.map(&app, 255);
    assert!(h.compositor.focused_surface.is_some());

    h.compositor.lock_screen.command = vec!["/usr/lib/lock-screen".into()];
    h.compositor.lock_enabled = true;
    h.compositor.refresh_lock_state();

    assert!(h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    assert_eq!(find_touch_target(&h.compositor, 1.0, 1.0), None);
}

#[test]
fn locked_home_and_lower_buttons_change_only_view_or_display() {
    let mut h = Harness::new();
    h.compositor.lock_enabled = true;
    h.compositor.shell_mode = ShellMode::Settings;
    h.compositor.transition_lock(true);
    let locked_return = h.compositor.lock_return_mode;
    assert_eq!(locked_return, ShellMode::Settings);

    let hardware = expect_display_modes(&h, vec![0, 2, 0, 2]);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        1,
    );
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: false },
        2,
    );
    assert!(h.compositor.locked_watchface_selected);
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    assert!(h.compositor.is_locked());

    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_VOLUMEDOWN, pressed: true },
        3,
    );
    assert!(!h.compositor.display_on);
    assert!(h.compositor.manual_off);
    assert!(h.compositor.locked_watchface_selected);
    assert_eq!(h.compositor.lock_return_mode, locked_return);

    // Home and lower presses in the dark wake only; they select PIN without forwarding the wake press.
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        4,
    );
    assert!(h.compositor.display_on);
    assert!(!h.compositor.manual_off);
    assert!(!h.compositor.locked_watchface_selected);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_VOLUMEDOWN, pressed: true },
        5,
    );
    assert!(!h.compositor.display_on);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_VOLUMEDOWN, pressed: true },
        6,
    );
    assert!(h.compositor.display_on);
    assert!(!h.compositor.locked_watchface_selected);

    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        7,
    );
    assert!(h.compositor.locked_watchface_selected);
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    assert!(h.compositor.is_locked());
    assert_eq!(h.compositor.lock_return_mode, locked_return);
    hardware.join().unwrap();
}

#[test]
fn locked_watchface_is_unfocused_read_only_and_does_not_expose_other_surfaces() {
    let mut h = Harness::new();
    let (app, _, _) = h.window();
    h.map(&app, 51);
    let app_server = h.compositor.focused_surface.clone().unwrap();
    h.compositor.lock_enabled = true;
    h.compositor.shell_mode = ShellMode::Settings;
    h.compositor.transition_lock(true);

    let pid = std::process::id();
    h.compositor.watchface.child_pid = Some(pid);
    let (watchface, _, _) = h.window();
    h.map(&watchface, 77);
    let watchface_server = h.compositor.watchface.surface.clone().unwrap();

    h.compositor.lock_screen.child_pid = Some(pid);
    let (lockscreen, _, _) = h.window();
    h.map(&lockscreen, 22);
    let lockscreen_server = h.compositor.lock_screen.surface.clone().unwrap();

    let qh = h.queue.handle();
    let overlay = h.client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = h.client.layers.as_ref().unwrap().get_layer_surface(
        &overlay,
        None,
        layer_shell::Layer::Overlay,
        "locked-test-overlay".into(),
        &qh,
        (),
    );
    layer.set_size(4, 4);
    overlay.commit();
    h.pump();
    h.pump();
    h.map(&overlay, 211);
    let overlay_server = h.compositor.layer_surfaces[0].surface.wl_surface().clone();

    let keyboard = h.compositor.wayland.seat.get_keyboard().unwrap();
    assert_eq!(keyboard.current_focus(), Some(lockscreen_server.clone()));
    assert!(h.compositor.surface_visible(&lockscreen_server));
    assert!(!h.compositor.surface_visible(&watchface_server));

    watchface.frame(&h.queue.handle(), true);
    watchface.commit();
    h.pump();
    assert!(!h.compositor.has_visible_callbacks());
    h.compositor.complete_visible_callbacks(19);
    h.pump();
    assert_eq!(h.client.frames, 0);

    // The PIN screen retains the top-button editing route.
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_VOLUMEUP, pressed: true },
        8,
    );
    assert_eq!(keyboard.current_focus(), Some(lockscreen_server.clone()));

    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        9,
    );
    assert!(h.compositor.locked_watchface_selected);
    assert!(h.compositor.is_locked());
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    assert_eq!(h.compositor.active_surface(), None);
    assert_eq!(keyboard.current_focus(), None);
    assert!(h.compositor.surface_visible(&watchface_server));
    assert!(!h.compositor.surface_visible(&lockscreen_server));
    assert!(!h.compositor.surface_visible(&app_server));
    assert!(!h.compositor.surface_visible(&overlay_server));
    assert_eq!(find_touch_target(&h.compositor, 1.0, 1.0), None);

    let mut frame = vec![0; 4 * 4 * 4];
    compose::composite_frame(
        &mut frame,
        4,
        4,
        h.compositor.lock_screen.buffer.as_ref(),
        h.compositor.watchface.buffer.as_ref(),
        true,
        h.compositor.launcher.buffer.as_ref(),
        h.compositor.settings.buffer.as_ref(),
        h.compositor.agent.buffer.as_ref(),
        h.compositor.app_surfaces.iter().find(|entry| entry.surface == app_server)
            .and_then(|entry| entry.buffer.as_ref()),
        &h.compositor.layer_surfaces,
        ShellMode::LockScreen,
    );
    assert_eq!(&frame[..4], &[77, 77, 77, 255]);

    // Frame callbacks follow the selected locked renderer.
    assert!(h.compositor.has_visible_callbacks());
    h.compositor.complete_visible_callbacks(20);
    h.pump();
    assert_eq!(h.client.frames, 1);
    lockscreen.frame(&qh, true);
    lockscreen.commit();
    h.pump();
    assert!(!h.compositor.has_visible_callbacks());
    h.compositor.complete_visible_callbacks(21);
    h.pump();
    assert_eq!(h.client.frames, 1);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        22,
    );
    assert!(h.compositor.has_visible_callbacks());
    h.compositor.complete_visible_callbacks(23);
    h.pump();
    assert_eq!(h.client.frames, 2);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        24,
    );
    assert_eq!(keyboard.current_focus(), None);

    // A watchface touch returns to PIN entry and its complete sequence is swallowed.
    handle_touch(
        &mut h.compositor,
        &input::TouchEvent { slot: 0, x: 1.0, y: 1.0, state: input::TouchState::Down },
        10,
    );
    handle_touch(
        &mut h.compositor,
        &input::TouchEvent { slot: 0, x: 1.0, y: 1.0, state: input::TouchState::Motion },
        11,
    );
    handle_touch(
        &mut h.compositor,
        &input::TouchEvent { slot: 0, x: 1.0, y: 1.0, state: input::TouchState::Up },
        12,
    );
    h.pump();
    assert!(!h.compositor.locked_watchface_selected);
    assert!(h.client.touches.is_empty());
    assert_eq!(keyboard.current_focus(), Some(lockscreen_server.clone()));

    // Fresh PIN-screen touches still reach only the trusted lock renderer.
    handle_touch(
        &mut h.compositor,
        &input::TouchEvent { slot: 0, x: 1.0, y: 1.0, state: input::TouchState::Down },
        13,
    );
    handle_touch(
        &mut h.compositor,
        &input::TouchEvent { slot: 0, x: 1.0, y: 1.0, state: input::TouchState::Up },
        14,
    );
    h.pump();
    assert_eq!(h.client.touches, ["down", "up"]);

    // Returning to the watchface clears focus; lockscreen restarts cannot steal it.
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_POWER, pressed: true },
        15,
    );
    assert_eq!(keyboard.current_focus(), None);
    h.compositor.lock_screen.surface = None;
    let (replacement, _, _) = h.window();
    h.map(&replacement, 23);
    assert!(h.compositor.locked_watchface_selected);
    assert_eq!(keyboard.current_focus(), None);
}

fn role_test_channel(role: &mut ManagedRole) -> (UnixStream, mpsc::Sender<String>) {
    let (reader, writer) = UnixStream::pair().unwrap();
    role.stdin = Some(role_input::RoleInput::new(writer.into()).unwrap());
    let (tx, rx) = mpsc::channel();
    role.rx = Some(rx);
    (reader, tx)
}

fn read_role_line(reader: &mut std::io::BufReader<UnixStream>) -> String {
    use std::io::BufRead;
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line.trim_end().to_string()
}

#[test]
fn watchfaces_receive_and_query_lock_state_while_locked() {
    let mut h = Harness::new();
    h.compositor.lock_enabled = true;
    h.compositor.locked = true;
    h.compositor.shell_mode = ShellMode::LockScreen;

    let (primary_peer, primary_tx) = role_test_channel(&mut h.compositor.watchface);
    let (placeholder_peer, placeholder_tx) = role_test_channel(&mut h.compositor.placeholder.role);
    let mut primary_reader = std::io::BufReader::new(primary_peer);
    let mut placeholder_reader = std::io::BufReader::new(placeholder_peer);

    // Initial push and visibility order are stable and de-duplicated.
    h.compositor.watchface.report_lock_state(true);
    h.compositor.watchface.report_lock_state(true);
    h.compositor.watchface.report_visibility(false);
    h.compositor.placeholder.role.report_lock_state(true);
    h.compositor.placeholder.role.report_visibility(false);
    assert_eq!(read_role_line(&mut primary_reader), "lock-state:locked");
    assert_eq!(read_role_line(&mut primary_reader), "visibility:hidden");
    assert_eq!(read_role_line(&mut placeholder_reader), "lock-state:locked");
    assert_eq!(read_role_line(&mut placeholder_reader), "visibility:hidden");

    primary_tx.send("get-lock-state".into()).unwrap();
    placeholder_tx.send("get-lock-state".into()).unwrap();
    h.compositor.process_role_messages();
    assert_eq!(read_role_line(&mut primary_reader), "lock-state:locked");
    assert_eq!(read_role_line(&mut placeholder_reader), "lock-state:locked");

    primary_tx.send("go-launcher".into()).unwrap();
    h.compositor.process_role_messages();
    assert_eq!(h.compositor.shell_mode, ShellMode::LockScreen);
    assert!(h.compositor.is_locked());

    h.compositor.locked = false;
    primary_tx.send("get-lock-state".into()).unwrap();
    h.compositor.process_role_messages();
    assert_eq!(read_role_line(&mut primary_reader), "lock-state:unlocked");
}

#[test]
fn locked_lower_off_survives_reconciliation_and_locked_idle_blanks() {
    let mut h = Harness::new();
    h.compositor.lock_enabled = true;
    h.compositor.locked = true;
    h.compositor.shell_mode = ShellMode::LockScreen;
    h.compositor.locked_watchface_selected = true;
    let hardware = expect_display_modes(&h, vec![0]);
    handle_button(
        &mut h.compositor,
        &input::ButtonEvent { code: KEY_VOLUMEDOWN, pressed: true },
        1,
    );
    h.compositor.reconcile_sleep();
    assert!(h.compositor.running);
    assert!(!h.compositor.display_on);
    assert!(h.compositor.manual_off);
    assert!(h.compositor.locked_watchface_selected);
    assert!(h.compositor.is_locked());
    hardware.join().unwrap();

    let mut h = Harness::new();
    h.compositor.lock_enabled = true;
    h.compositor.locked = true;
    h.compositor.shell_mode = ShellMode::LockScreen;
    h.compositor.locked_watchface_selected = true;
    h.compositor.show_placeholder(true);
    h.compositor.last_activity = std::time::Instant::now() - std::time::Duration::from_secs(40);
    let hardware = expect_display_modes(&h, vec![0]);
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.display_on);
    assert!(!h.compositor.ambient);
    assert!(!h.compositor.placeholder.visible);
    assert!(h.compositor.is_locked());
    hardware.join().unwrap();
}

#[test]
fn locked_secondary_face_stays_visible_until_idle_and_crown_wakes_pin() {
    let mut h=Harness::new();
    h.compositor.lock_enabled=true;
    h.compositor.transition_lock(true);
    h.compositor.display_config.face_mode="secondary".into();
    h.compositor.select_locked_watchface(true);
    // Powerd cannot tell the compositor to blank, even via a stale old-style reply.
    h.compositor.sleep_bridge.set_reply_for_test(serde_json::json!({"ok":true,"display":"ambient"}));
    h.compositor.reconcile_sleep();
    assert!(h.compositor.display_on);
    assert!(h.compositor.locked_watchface_selected);
    let hardware=expect_display_modes(&h,vec![0,2]);
    h.compositor.last_activity=std::time::Instant::now()-std::time::Duration::from_secs(31);
    h.compositor.reconcile_sleep();
    assert!(!h.compositor.display_on);
    handle_button(&mut h.compositor,&input::ButtonEvent{code:KEY_POWER,pressed:true},1);
    handle_button(&mut h.compositor,&input::ButtonEvent{code:KEY_POWER,pressed:false},2);
    h.compositor.reconcile_sleep();
    assert!(h.compositor.display_on);
    assert!(!h.compositor.locked_watchface_selected);
    assert!(h.compositor.is_locked());
    hardware.join().unwrap();
}

#[test]
fn completed_handoff_keeps_awake_lease_until_matching_acknowledgement() {
    let mut h=Harness::new();
    let path=std::env::temp_dir().join(format!("hoki-ready-test-{}",std::process::id()));
    let listener=UnixListener::bind(&path).unwrap();
    h.compositor.interactive_inhibitor=Some(sleep_client::Client::connect_to(path.to_str().unwrap()).unwrap());
    let (_peer,_)=listener.accept().unwrap();
    std::fs::remove_file(path).unwrap();
    let hardware=expect_display_modes(&h,vec![0]);
    h.compositor.set_display_power(false);
    let revision=h.compositor.activity_revision;
    h.compositor.sleep_bridge.set_reply_for_test(serde_json::json!({"ok":true,
        "_request":{"command":"ui","revision":revision-1,"display":"off","ready":true}}));
    h.compositor.reconcile_sleep();
    assert!(h.compositor.interactive_inhibitor.is_some());
    h.compositor.sleep_bridge.set_reply_for_test(serde_json::json!({"ok":true,
        "_request":{"command":"ui","revision":revision,"display":"off","ready":true}}));
    h.compositor.reconcile_sleep();
    assert!(h.compositor.interactive_inhibitor.is_none());
    hardware.join().unwrap();
}

#[test]
fn failed_manual_off_keeps_interactive_fallback_without_retrying() {
    use std::io::Read;
    let mut h = Harness::new();
    let mut peer = h._proxy.try_clone().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
    let hardware = std::thread::spawn(move || {
        for (mode, status) in [(0, 1), (2, 0)] {
            let mut header = [0u8; 5];
            peer.read_exact(&mut header).unwrap();
            assert_eq!(header[4], 0x04);
            let mut payload = vec![0; u32::from_le_bytes(header[..4].try_into().unwrap()) as usize - 5];
            peer.read_exact(&mut payload).unwrap();
            assert_eq!(payload[0], mode);
            peer.write_all(&[6, 0, 0, 0, 0x84, status]).unwrap();
        }
    });
    h.compositor.set_display_power(false);
    hardware.join().unwrap();
    assert!(h.compositor.display_on);
    assert!(h.compositor.ambient_failed);
    let revision = h.compositor.activity_revision;
    h.compositor.reconcile_sleep();
    h.compositor.reconcile_sleep();
    assert!(h.compositor.running);
    assert!(h.compositor.display_on);
    assert_eq!(h.compositor.activity_revision, revision);
    // An explicit wake clears the failure and permits a later off attempt.
    h.compositor.set_display_power(true);
    assert!(!h.compositor.ambient_failed);
    let hardware = expect_display_modes(&h, vec![0]);
    h.compositor.set_display_power(false);
    assert!(!h.compositor.display_on);
    hardware.join().unwrap();
}

#[test]
fn display_status_control_reports_main_thread_state_while_locked() {
    let mut h=Harness::new();
    h.compositor.lock_enabled=true;
    h.compositor.transition_lock(true);
    let (server,mut client)=UnixStream::pair().unwrap();
    let tx=h.compositor.ctl_tx.clone();
    let wakeup=h.compositor.wakeup.clone();
    let worker=std::thread::spawn(move ||handle_ctl_connection(server,&tx,&wakeup));
    client.write_all(b"display-status\n").unwrap();
    let request=h.compositor.ctl_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    if let CtlMessage::DisplayRequest {patch,reply}=request {
        reply.send(h.compositor.display_request(patch)).unwrap();
    } else {panic!("wrong control request");}
    let mut line=String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let status:serde_json::Value=serde_json::from_str(&line).unwrap();
    assert_eq!(status["locked"],true);
    assert_eq!(status["config"]["idle_seconds"],30);
    worker.join().unwrap();
}

#[test]
fn physical_wake_and_off_both_wait_for_powerd_grants() {
    let mut h=Harness::new();
    h.compositor.power_coordination=true;
    h.compositor.display_on=false;
    h.compositor.manual_off=true;
    h.compositor.lock_enabled=true;
    h.compositor.transition_lock(true);
    let path=std::env::temp_dir().join(format!("hoki-grant-test-{}",std::process::id()));
    let listener=UnixListener::bind(&path).unwrap();
    h.compositor.interactive_inhibitor=Some(sleep_client::Client::connect_to(path.to_str().unwrap()).unwrap());
    let (peer,_)=listener.accept().unwrap();
    std::fs::remove_file(path).unwrap();
    let powerd=std::thread::spawn(move || {
        let mut reader=BufReader::new(peer.try_clone().unwrap());
        let mut writer=peer;
        for _ in 0..2 {
            let mut line=String::new();
            reader.read_line(&mut line).unwrap();
            let request:serde_json::Value=serde_json::from_str(&line).unwrap();
            assert_eq!(request["command"],"inhibit");
            assert_eq!(request["cpu"],true);
            writer.write_all(b"{\"ok\":true}\n").unwrap();
        }
    });
    let hardware=expect_display_modes(&h,vec![2,0]);
    handle_button(&mut h.compositor,&input::ButtonEvent{code:KEY_POWER,pressed:true},1);
    assert!(h.compositor.display_on);
    assert!(!h.compositor.locked_watchface_selected);
    h.compositor.set_display_power(false);
    assert!(h.compositor.interactive_inhibitor.is_some());
    hardware.join().unwrap();
    powerd.join().unwrap();
}
