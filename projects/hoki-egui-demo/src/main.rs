use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use egui::{Color32, Context, Rect, TextureId, ViewportId};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const WIDTH: u32 = 416;
const HEIGHT: u32 = 416;
const SCALE: f32 = 3.0;

// Logical dimensions at 3x zoom
const LW: f32 = WIDTH as f32 / SCALE; // ~138.7
const LH: f32 = HEIGHT as f32 / SCALE;
const LR: f32 = LW / 2.0; // logical radius ~69.3
const LC: f32 = LR; // logical center x and y

// Colors
const BG: Color32 = Color32::from_rgb(0x0d, 0x0d, 0x1a);
const MID_BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x28);
const DEC_BG: Color32 = Color32::from_rgb(0x2a, 0x15, 0x20);
const DEC_HOVER: Color32 = Color32::from_rgb(0x3a, 0x20, 0x2a);
const INC_BG: Color32 = Color32::from_rgb(0x15, 0x2a, 0x20);
const INC_HOVER: Color32 = Color32::from_rgb(0x20, 0x3a, 0x2a);
const DIVIDER: Color32 = Color32::from_rgb(0x33, 0x33, 0x55);
const TITLE_COLOR: Color32 = Color32::from_rgb(0x77, 0x88, 0xaa);
const VALUE_COLOR: Color32 = Color32::WHITE;
const DEC_TEXT: Color32 = Color32::from_rgb(0xcc, 0x66, 0x66);
const INC_TEXT: Color32 = Color32::from_rgb(0x66, 0xcc, 0xaa);

// Segment boundaries (thirds)
const SEG_GAP: f32 = 0.5;
const SEG1_Y: f32 = LH / 3.0; // ~46.2
const SEG2_Y: f32 = LH * 2.0 / 3.0; // ~92.4

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}

struct TextureData {
    pixels: Vec<Color32>,
    width: usize,
    height: usize,
}

struct App {
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    ctx: Context,
    egui_state: Option<egui_winit::State>,
    textures: HashMap<TextureId, TextureData>,
    counter: i32,
}

impl App {
    fn new() -> Self {
        let ctx = Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        Self {
            window: None,
            surface: None,
            ctx,
            egui_state: None,
            textures: HashMap::new(),
            counter: 0,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("egui demo")
            .with_inner_size(winit::dpi::PhysicalSize::new(WIDTH, HEIGHT))
            .with_resizable(false);

        let window = Arc::new(event_loop.create_window(attrs).unwrap());

        let context = softbuffer::Context::new(window.clone()).unwrap();
        let mut surface = softbuffer::Surface::new(&context, window.clone()).unwrap();
        surface
            .resize(
                NonZeroU32::new(WIDTH).unwrap(),
                NonZeroU32::new(HEIGHT).unwrap(),
            )
            .unwrap();

        let egui_state = egui_winit::State::new(
            self.ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(1.0),
            None,
            None,
        );

        self.ctx.set_zoom_factor(SCALE);
        self.window = Some(window.clone());
        self.surface = Some(surface);
        self.egui_state = Some(egui_state);

        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = &self.window else { return };
        let Some(egui_state) = &mut self.egui_state else {
            return;
        };

        let response = egui_state.on_window_event(window, &event);
        if response.repaint {
            window.request_redraw();
        }
        if response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

impl App {
    fn render(&mut self) {
        let window = self.window.clone().unwrap();
        let egui_state = self.egui_state.as_mut().unwrap();
        let raw_input = egui_state.take_egui_input(&window);

        let counter = &mut self.counter;

        let ctx = self.ctx.clone();
        let full_output = ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    // Bottom button rects
                    let dec_rect = Rect::from_min_max(
                        egui::pos2(0.0, SEG2_Y + SEG_GAP),
                        egui::pos2(LC - SEG_GAP, LH),
                    );
                    let inc_rect = Rect::from_min_max(
                        egui::pos2(LC + SEG_GAP, SEG2_Y + SEG_GAP),
                        egui::pos2(LW, LH),
                    );

                    // Allocate interactive areas (needs &mut ui)
                    let dec_resp = ui.allocate_rect(dec_rect, egui::Sense::click());
                    let inc_resp = ui.allocate_rect(inc_rect, egui::Sense::click());

                    // Now paint everything (borrows ui immutably via painter)
                    let painter = ui.painter();

                    // --- Backgrounds ---
                    painter.rect_filled(
                        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(LW, LH)),
                        0.0,
                        BG,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(
                            egui::pos2(0.0, SEG1_Y + SEG_GAP),
                            egui::pos2(LW, SEG2_Y - SEG_GAP),
                        ),
                        0.0,
                        MID_BG,
                    );
                    painter.rect_filled(
                        dec_rect,
                        0.0,
                        if dec_resp.hovered() { DEC_HOVER } else { DEC_BG },
                    );
                    painter.rect_filled(
                        inc_rect,
                        0.0,
                        if inc_resp.hovered() { INC_HOVER } else { INC_BG },
                    );

                    // --- Divider lines ---
                    painter.line_segment(
                        [egui::pos2(4.0, SEG1_Y), egui::pos2(LW - 4.0, SEG1_Y)],
                        egui::Stroke::new(0.5, DIVIDER),
                    );
                    painter.line_segment(
                        [egui::pos2(4.0, SEG2_Y), egui::pos2(LW - 4.0, SEG2_Y)],
                        egui::Stroke::new(0.5, DIVIDER),
                    );
                    painter.line_segment(
                        [egui::pos2(LC, SEG2_Y + 4.0), egui::pos2(LC, LH - 4.0)],
                        egui::Stroke::new(0.5, DIVIDER),
                    );

                    // --- Text ---
                    painter.text(
                        egui::pos2(LC, SEG1_Y / 2.0),
                        egui::Align2::CENTER_CENTER,
                        "Counter",
                        egui::FontId::proportional(11.0),
                        TITLE_COLOR,
                    );
                    painter.text(
                        egui::pos2(LC, (SEG1_Y + SEG2_Y) / 2.0),
                        egui::Align2::CENTER_CENTER,
                        format!("{}", *counter),
                        egui::FontId::proportional(24.0),
                        VALUE_COLOR,
                    );
                    painter.text(
                        egui::pos2(dec_rect.center().x, dec_rect.center().y),
                        egui::Align2::CENTER_CENTER,
                        "\u{2212}",
                        egui::FontId::proportional(18.0),
                        DEC_TEXT,
                    );
                    painter.text(
                        egui::pos2(inc_rect.center().x, inc_rect.center().y),
                        egui::Align2::CENTER_CENTER,
                        "+",
                        egui::FontId::proportional(18.0),
                        INC_TEXT,
                    );

                    // --- Interaction ---
                    if dec_resp.clicked() {
                        *counter -= 1;
                    }
                    if inc_resp.clicked() {
                        *counter += 1;
                    }
                });
        });

        // Update textures
        for (id, delta) in &full_output.textures_delta.set {
            apply_texture(&mut self.textures, *id, delta);
        }

        let mut prims = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);

        // Scale tessellated output from logical points to physical pixels
        let ppp = full_output.pixels_per_point;
        for prim in &mut prims {
            prim.clip_rect = Rect::from_min_max(
                egui::pos2(prim.clip_rect.min.x * ppp, prim.clip_rect.min.y * ppp),
                egui::pos2(prim.clip_rect.max.x * ppp, prim.clip_rect.max.y * ppp),
            );
            if let egui::epaint::Primitive::Mesh(mesh) = &mut prim.primitive {
                for v in &mut mesh.vertices {
                    v.pos.x *= ppp;
                    v.pos.y *= ppp;
                }
            }
        }

        // Render to software buffer
        let surface = self.surface.as_mut().unwrap();
        let mut buffer = surface.buffer_mut().unwrap();

        // Clear to black (outside circle)
        buffer.fill(0x00000000);

        software_render(
            &mut buffer,
            WIDTH as usize,
            HEIGHT as usize,
            &prims,
            &self.textures,
        );
        buffer.present().unwrap();

        for id in &full_output.textures_delta.free {
            self.textures.remove(id);
        }

        let egui_state = self.egui_state.as_mut().unwrap();
        egui_state.handle_platform_output(&window, full_output.platform_output);

        if ctx.has_requested_repaint() {
            window.request_redraw();
        }
    }
}

// --- Texture management ---

fn apply_texture(
    textures: &mut HashMap<TextureId, TextureData>,
    id: TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let pixels: Vec<Color32> = match &delta.image {
        egui::epaint::ImageData::Color(img) => img.pixels.clone(),
        egui::epaint::ImageData::Font(img) => img.srgba_pixels(None).collect(),
    };

    if let Some(pos) = delta.pos {
        let tex = textures.get_mut(&id).unwrap();
        let w = delta.image.width();
        for (i, pixel) in pixels.iter().enumerate() {
            let x = pos[0] + i % w;
            let y = pos[1] + i / w;
            if x < tex.width && y < tex.height {
                tex.pixels[y * tex.width + x] = *pixel;
            }
        }
    } else {
        textures.insert(
            id,
            TextureData {
                width: delta.image.width(),
                height: delta.image.height(),
                pixels,
            },
        );
    }
}

// --- Software triangle rasterizer for egui meshes ---

fn software_render(
    buf: &mut [u32],
    w: usize,
    h: usize,
    primitives: &[egui::epaint::ClippedPrimitive],
    textures: &HashMap<TextureId, TextureData>,
) {
    for prim in primitives {
        match &prim.primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                let Some(tex) = textures.get(&mesh.texture_id) else {
                    continue;
                };
                for tri in mesh.indices.chunks_exact(3) {
                    rasterize_tri(
                        buf,
                        w,
                        h,
                        &prim.clip_rect,
                        &mesh.vertices[tri[0] as usize],
                        &mesh.vertices[tri[1] as usize],
                        &mesh.vertices[tri[2] as usize],
                        tex,
                    );
                }
            }
            egui::epaint::Primitive::Callback(_) => {}
        }
    }
}

#[inline(always)]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

fn rasterize_tri(
    buf: &mut [u32],
    w: usize,
    h: usize,
    clip: &Rect,
    v0: &egui::epaint::Vertex,
    v1: &egui::epaint::Vertex,
    v2: &egui::epaint::Vertex,
    tex: &TextureData,
) {
    let min_x = v0.pos.x.min(v1.pos.x).min(v2.pos.x).max(clip.min.x).max(0.0) as usize;
    let min_y = v0.pos.y.min(v1.pos.y).min(v2.pos.y).max(clip.min.y).max(0.0) as usize;
    let max_x =
        (v0.pos.x.max(v1.pos.x).max(v2.pos.x).min(clip.max.x).ceil() as usize).min(w - 1);
    let max_y =
        (v0.pos.y.max(v1.pos.y).max(v2.pos.y).min(clip.max.y).ceil() as usize).min(h - 1);

    let area = edge(v0.pos.x, v0.pos.y, v1.pos.x, v1.pos.y, v2.pos.x, v2.pos.y);
    if area.abs() < 0.001 {
        return;
    }
    let inv_area = 1.0 / area;

    let tw = tex.width.max(1) as f32;
    let th = tex.height.max(1) as f32;
    let tw_max = (tex.width - 1) as usize;
    let th_max = (tex.height - 1) as usize;

    // Circular display clipping
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let r_sq = cx * cx;

    for y in min_y..=max_y {
        let py = y as f32 + 0.5;
        let dy = py - cy;
        let dy_sq = dy * dy;
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let dx = px - cx;
            if dx * dx + dy_sq > r_sq {
                continue;
            }

            let b0 = edge(v1.pos.x, v1.pos.y, v2.pos.x, v2.pos.y, px, py) * inv_area;
            let b1 = edge(v2.pos.x, v2.pos.y, v0.pos.x, v0.pos.y, px, py) * inv_area;
            let b2 = 1.0 - b0 - b1;

            if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                continue;
            }

            let u = b0 * v0.uv.x + b1 * v1.uv.x + b2 * v2.uv.x;
            let v = b0 * v0.uv.y + b1 * v1.uv.y + b2 * v2.uv.y;

            let tx = ((u * tw) as usize).min(tw_max);
            let ty = ((v * th) as usize).min(th_max);
            let tc = tex.pixels[ty * tex.width + tx];

            let cr =
                b0 * v0.color.r() as f32 + b1 * v1.color.r() as f32 + b2 * v2.color.r() as f32;
            let cg =
                b0 * v0.color.g() as f32 + b1 * v1.color.g() as f32 + b2 * v2.color.g() as f32;
            let cb =
                b0 * v0.color.b() as f32 + b1 * v1.color.b() as f32 + b2 * v2.color.b() as f32;
            let ca =
                b0 * v0.color.a() as f32 + b1 * v1.color.a() as f32 + b2 * v2.color.a() as f32;

            let sr = (tc.r() as f32 * cr / 255.0) as u32;
            let sg = (tc.g() as f32 * cg / 255.0) as u32;
            let sb = (tc.b() as f32 * cb / 255.0) as u32;
            let sa = (tc.a() as f32 * ca / 255.0) as u32;

            if sa == 0 {
                continue;
            }

            let idx = y * w + x;
            let dst = buf[idx];
            let dr = (dst >> 16) & 0xFF;
            let dg = (dst >> 8) & 0xFF;
            let db = dst & 0xFF;

            let inv_a = 255 - sa;
            let or = (sr * sa + dr * inv_a) / 255;
            let og = (sg * sa + dg * inv_a) / 255;
            let ob = (sb * sa + db * inv_a) / 255;

            buf[idx] = (or.min(255) << 16) | (og.min(255) << 8) | ob.min(255);
        }
    }
}
