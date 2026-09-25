//! WASM guest app — renders a touch-reactive demo to a pixel buffer.

use std::sync::Mutex;

static STATE: Mutex<AppState> = Mutex::new(AppState {
    width: 0,
    height: 0,
    buffer: Vec::new(),
    touch_x: 0.0,
    touch_y: 0.0,
    touched: false,
    frame: 0,
    ripples: Vec::new(),
});

struct Ripple {
    x: f32,
    y: f32,
    radius_sq: f32, // squared radius for cheap comparison
    radius: f32,
    hue: f32,
}

struct AppState {
    width: u32,
    height: u32,
    buffer: Vec<u8>,
    touch_x: f32,
    touch_y: f32,
    touched: bool,
    frame: u32,
    ripples: Vec<Ripple>,
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if hp < 1.0 {
        (c, x, 0.0)
    } else if hp < 2.0 {
        (x, c, 0.0)
    } else if hp < 3.0 {
        (0.0, c, x)
    } else if hp < 4.0 {
        (0.0, x, c)
    } else if hp < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn sin_approx(mut x: f32) -> f32 {
    use core::f32::consts::PI;
    x = x % (2.0 * PI);
    if x > PI {
        x -= 2.0 * PI;
    } else if x < -PI {
        x += 2.0 * PI;
    }
    let num = 16.0 * x * (PI - x.abs());
    let den = 5.0 * PI * PI - 4.0 * x * (PI - x.abs());
    num / den
}

fn cos_approx(x: f32) -> f32 {
    sin_approx(x + core::f32::consts::FRAC_PI_2)
}

// Fast inverse sqrt (Quake-style) — avoids sqrt entirely
fn fast_inv_sqrt(x: f32) -> f32 {
    let half = 0.5 * x;
    let i = f32::from_bits(0x5f3759df - (x.to_bits() >> 1));
    i * (1.5 - half * i * i)
}

#[no_mangle]
pub extern "C" fn init(width: u32, height: u32) {
    {
        let mut state = STATE.lock().unwrap();
        state.width = width;
        state.height = height;
        state.buffer = vec![0u8; (width * height * 4) as usize];
        state.touch_x = width as f32 / 2.0;
        state.touch_y = height as f32 / 2.0;
    }
}

#[no_mangle]
pub extern "C" fn render() -> u32 {
    render_state(&mut STATE.lock().unwrap())
}

fn render_state(s: &mut AppState) -> u32 {
    let w = s.width;
    let h = s.height;
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let screen_r_sq = cx * cx;
    let t = s.frame as f32 * 0.03;
    s.frame += 1;

    // Update ripples
    for ripple in s.ripples.iter_mut() {
        ripple.radius += 3.0;
        ripple.radius_sq = ripple.radius * ripple.radius;
    }
    s.ripples.retain(|r| r.radius < 200.0);

    // 3 orbiting blobs (reduced from 5)
    let mut blob_x = [0.0f32; 3];
    let mut blob_y = [0.0f32; 3];
    let mut blob_hue = [0.0f32; 3];
    let blob_r_sq = 50.0 * 50.0; // blob influence radius squared
    for i in 0..3 {
        let angle = t * (0.4 + i as f32 * 0.2) + i as f32 * 2.094;
        let orbit_r = 40.0 + 15.0 * sin_approx(t * 0.3 + i as f32);
        blob_x[i] = cx + cos_approx(angle) * orbit_r;
        blob_y[i] = cy + sin_approx(angle) * orbit_r;
        blob_hue[i] = (i as f32 * 120.0 + t * 10.0) % 360.0;
    }

    let touch_r_sq = 50.0 * 50.0;

    for y in 0..h {
        let py = y as f32 + 0.5;
        let dy = py - cy;
        let dy2 = dy * dy;

        for x in 0..w {
            let px = x as f32 + 0.5;
            let dx = px - cx;
            let dist_sq = dx * dx + dy2;

            let idx = ((y * w + x) * 4) as usize;

            // Circular mask (compare squared distances — no sqrt)
            if dist_sq > screen_r_sq {
                s.buffer[idx] = 0;
                s.buffer[idx + 1] = 0;
                s.buffer[idx + 2] = 0;
                s.buffer[idx + 3] = 255;
                continue;
            }

            // Background
            let bg_hue = (t * 8.0 + dx * 0.8) % 360.0;
            let inv_r = fast_inv_sqrt(dist_sq + 1.0);
            let dist_norm = 1.0 / (inv_r * cx + 0.001);
            let bg_v = 0.06 + 0.04 * (1.0 - dist_norm.min(1.0));
            let (mut r, mut g, mut b) = hsv_to_rgb(bg_hue.abs(), 0.5, bg_v);

            // Blobs
            for i in 0..3 {
                let bdx = px - blob_x[i];
                let bdy = py - blob_y[i];
                let bd_sq = bdx * bdx + bdy * bdy;
                if bd_sq < blob_r_sq {
                    let falloff = 1.0 - bd_sq / blob_r_sq;
                    let (br, bg2, bb) = hsv_to_rgb(blob_hue[i], 0.8, falloff * 0.7);
                    r = r.saturating_add(br);
                    g = g.saturating_add(bg2);
                    b = b.saturating_add(bb);
                }
            }

            // Touch glow
            if s.touched {
                let tdx = px - s.touch_x;
                let tdy = py - s.touch_y;
                let td_sq = tdx * tdx + tdy * tdy;
                if td_sq < touch_r_sq {
                    let falloff = 1.0 - td_sq / touch_r_sq;
                    let (tr, tg, tb) = hsv_to_rgb((t * 30.0) % 360.0, 0.5, falloff);
                    r = r.saturating_add(tr);
                    g = g.saturating_add(tg);
                    b = b.saturating_add(tb);
                }
            }

            // Ripple rings — use sqrt only for active ripples near this pixel
            for ripple in s.ripples.iter() {
                let rdx = px - ripple.x;
                let rdy = py - ripple.y;
                let rd_sq = rdx * rdx + rdy * rdy;
                // Quick bounds check: skip if way outside or inside the ring
                let inner = (ripple.radius - 8.0).max(0.0);
                let outer = ripple.radius + 8.0;
                if rd_sq < inner * inner || rd_sq > outer * outer {
                    continue;
                }
                // Only sqrt for pixels near the ring
                let rd = rd_sq * fast_inv_sqrt(rd_sq); // rd ≈ sqrt(rd_sq)
                let ring_dist = (rd - ripple.radius).abs();
                if ring_dist < 6.0 {
                    let fade = 1.0 - ripple.radius / 200.0;
                    let ring_t = (1.0 - ring_dist / 6.0) * fade * fade;
                    let (rr, rg, rb) = hsv_to_rgb(ripple.hue, 0.9, ring_t);
                    r = r.saturating_add(rr);
                    g = g.saturating_add(rg);
                    b = b.saturating_add(rb);
                }
            }

            s.buffer[idx] = r;
            s.buffer[idx + 1] = g;
            s.buffer[idx + 2] = b;
            s.buffer[idx + 3] = 255;
        }
    }

    s.buffer.as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn on_touch(x: u32, y: u32, pressed: u32) {
    {
        let mut state = STATE.lock().unwrap();
        state.touch_x = x as f32;
        state.touch_y = y as f32;
        let was = state.touched;
        state.touched = pressed != 0;

        if state.touched && !was {
            let hue = (state.frame as f32 * 3.0) % 360.0;
            state.ripples.push(Ripple {
                x: x as f32,
                y: y as f32,
                radius: 0.0,
                radius_sq: 0.0,
                hue,
            });
        }
    }
}

#[no_mangle]
pub extern "C" fn buffer_len() -> u32 {
    STATE.lock().unwrap().buffer.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> AppState {
        AppState {
            width: 128,
            height: 128,
            buffer: vec![0; 128 * 128 * 4],
            touch_x: 64.0,
            touch_y: 64.0,
            touched: false,
            frame: 0,
            ripples: Vec::new(),
        }
    }

    #[test]
    fn ripple_lights_pixels_at_its_radius() {
        let mut background = scene();
        let mut ripple = scene();
        ripple.ripples.push(Ripple {
            x: 64.0,
            y: 64.0,
            radius: 43.0,
            radius_sq: 43.0 * 43.0,
            hue: 0.0,
        });
        render_state(&mut background);
        render_state(&mut ripple); // Radius advances to 46 pixels.
        for (x, y) in [(110, 64), (17, 64), (64, 110), (64, 17)] {
            let offset = (y * 128 + x) * 4;
            assert!(
                ripple.buffer[offset] > background.buffer[offset],
                "missing red ring at {x},{y}"
            );
        }
        // The center and circular mask remain unaffected by a distant ring.
        for (x, y) in [(64, 64), (0, 0)] {
            let offset = (y * 128 + x) * 4;
            assert_eq!(
                &ripple.buffer[offset..offset + 4],
                &background.buffer[offset..offset + 4]
            );
        }
    }
}
