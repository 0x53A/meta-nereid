//! Minimal wlr-layer-shell test client.
//! Creates a semi-transparent overlay surface and fills it with a color.
//! Usage: layer-test [namespace] [r] [g] [b] [a]
//!   namespace: "quick-panel", "notifications", etc. (default: "test-overlay")
//!   r/g/b/a: 0-255 color values (default: 0 100 200 180)

use std::os::unix::io::AsFd;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

const WIDTH: u32 = 416;
const HEIGHT: u32 = 416;

struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    configured: bool,
    color: [u8; 4], // BGRA
    namespace: String,
    running: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let namespace = args.get(1).cloned().unwrap_or_else(|| "test-overlay".to_string());
    let r: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let g: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
    let b: u8 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(200);
    let a: u8 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(180);

    eprintln!("layer-test: namespace={namespace}, color=rgba({r},{g},{b},{a})");

    let conn = Connection::connect_to_env().expect("Failed to connect to Wayland");
    let display = conn.display();
    let mut event_queue: EventQueue<State> = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut state = State {
        compositor: None,
        shm: None,
        layer_shell: None,
        surface: None,
        layer_surface: None,
        configured: false,
        color: [((b as u16 * a as u16) / 255) as u8, ((g as u16 * a as u16) / 255) as u8, ((r as u16 * a as u16) / 255) as u8, a], // BGRA for ARGB8888 on LE
        namespace,
        running: true,
    };

    // Trigger registry enumeration
    display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state).expect("roundtrip failed");

    // Create surface + layer surface
    let compositor = state.compositor.as_ref().expect("no wl_compositor");
    let layer_shell = state.layer_shell.as_ref().expect("no zwlr_layer_shell_v1");

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None, // default output
        zwlr_layer_shell_v1::Layer::Overlay,
        state.namespace.clone(),
        &qh,
        (),
    );

    // Request fullscreen
    layer_surface.set_size(WIDTH, HEIGHT);
    layer_surface.set_anchor(
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Bottom
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
    );

    surface.commit();
    state.surface = Some(surface);
    state.layer_surface = Some(layer_surface);

    eprintln!("layer-test: waiting for configure...");

    while state.running {
        event_queue.blocking_dispatch(&mut state).expect("dispatch failed");

        if state.configured {
            draw_frame(&mut state, &qh);
            state.configured = false;
        }
    }
}

fn draw_frame(state: &mut State, qh: &QueueHandle<State>) {
    let shm = state.shm.as_ref().expect("no wl_shm");
    let surface = state.surface.as_ref().expect("no surface");

    let stride = WIDTH * 4;
    let size = (stride * HEIGHT) as usize;

    // Create shared memory buffer
    let file = create_shm_file(size);
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        WIDTH as i32,
        HEIGHT as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );

    // Fill with color
    let mapping = unsafe {
        std::slice::from_raw_parts_mut(
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                std::os::unix::io::AsRawFd::as_raw_fd(&file),
                0,
            ) as *mut u8,
            size,
        )
    };

    for pixel in mapping.chunks_exact_mut(4) {
        pixel.copy_from_slice(&state.color);
    }

    unsafe { libc::munmap(mapping.as_ptr() as *mut _, size); }

    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, WIDTH as i32, HEIGHT as i32);

    // Request frame callback for animation
    surface.frame(qh, ());
    surface.commit();

    eprintln!("layer-test: frame drawn");

    pool.destroy();
}

fn create_shm_file(size: usize) -> std::fs::File {
    use std::os::unix::io::FromRawFd;
    let name = std::ffi::CString::new("layer-test-shm").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create failed");
    unsafe { libc::ftruncate(fd, size as libc::off_t) };
    unsafe { std::fs::File::from_raw_fd(fd) }
}

// --- Wayland dispatch implementations ---

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            // Buffer released by compositor
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Frame callback — redraw
        state.configured = true;
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for State {
    fn event(_: &mut Self, _: &zwlr_layer_shell_v1::ZwlrLayerShellV1, _: zwlr_layer_shell_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                eprintln!("layer-test: configured {}x{}", width, height);
                layer_surface.ack_configure(serial);
                state.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                eprintln!("layer-test: closed");
                state.running = false;
            }
            _ => {}
        }
    }
}
