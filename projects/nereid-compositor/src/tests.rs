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
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    sync: usize,
    frames: usize,
    layers: Option<layer_shell::ZwlrLayerShellV1>,
    touches: Vec<&'static str>,
    clicks: usize,
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
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

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

impl Harness {
    fn new() -> Self { Self::with_size(4, 4) }
    fn with_size(width: u32, height: u32) -> Self {
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
            watchface: ManagedRole::new(RoleId::Watchface, vec![]),
            launcher: ManagedRole::new(RoleId::Launcher, vec![]),
            settings: ManagedRole::new(RoleId::Settings, vec![]),
            agent: ManagedRole::new(RoleId::Agent, vec![]),
            agent_return: ShellMode::Watchface,
            crown_press: Default::default(),
            app_surfaces: vec![],
            frame_callbacks: vec![],
            last_callback: std::time::Instant::now(),
            touch_targets: Default::default(),
            focused_surface: None,
            layer_surfaces: vec![],
            display_width: width,
            display_height: height,
            display_on: true,
            running: true,
            xdg_runtime: String::new(),
            damage: false,
            display_timeout_secs: 0,
            ambient:false,manual_off:false,sleep_enabled:false,sleep_generation:0,activity_revision:0,
            sleep_bridge:sleep::Bridge::new(),interactive_inhibitor:None,ambient_face:String::new(),secondary_only:false,ambient_failed:false,
            last_activity: std::time::Instant::now(),
            shell_mode: ShellMode::Launcher,
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
    h.compositor.switch_mode(ShellMode::Launcher);
    h.map(&a, 66);
    assert_eq!(h.compositor.shell_mode, ShellMode::Launcher);
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
        _: &mut Self,
        s: &layer_surface::ZwlrLayerSurfaceV1,
        event: layer_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let layer_surface::Event::Configure { serial, .. } = event {
            s.ack_configure(serial);
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
    for id in [RoleId::Watchface, RoleId::Launcher, RoleId::Settings, RoleId::Agent] {
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
fn launcher_argument_vector_reaches_child_without_shell_reparsing() {
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
        RoleId::Launcher,
        &format!("launch-argv:{}", serde_json::to_string(&args).unwrap()),
    );
    let launch = h.compositor.ctl_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    match launch {
        CtlMessage::LaunchApp(args) => h.compositor.spawn_app(&args),
        CtlMessage::SetRole { .. } => panic!("unexpected control message"),
    }
    assert_eq!(h.compositor.apps.len(), 1);
    assert!(h.compositor.apps[0].wait().unwrap().success());
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        args[4..].iter().map(String::as_str).collect::<Vec<_>>()
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn role_visibility_is_sent_once_per_transition_and_after_respawn() {
    let wake = Arc::new(wakeup::Wakeup::new().unwrap());
    let mut role = ManagedRole::new(RoleId::Settings, vec!["/bin/sh".into(), "-c".into(),
        "test \"$HOKI_MANAGED_ROLE\" = 1 || exit 2; while IFS= read -r line; do printf '%s\\n' \"$line\"; done".into()]);
    for _ in 0..2 {
        spawn_role(&mut role, "/tmp", &wake);
        role.report_visibility(false);
        for _ in 0..100 {
            role.report_visibility(false);
        }
        role.report_visibility(true);
        for _ in 0..100 {
            role.report_visibility(true);
        }
        let rx = role.rx.as_ref().unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "visibility:hidden"
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
