// SPDX-License-Identifier: GPL-3.0-only
use serde_json::Value;
const W: usize = 276;
const H: usize = 157;
/// Render accepted points only, preserving breaks instead of joining pauses.
pub fn render(track: &Value) -> (slint::Image, bool) {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(W as u32, H as u32);
    let points: Vec<_> = track
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let lat = p["lat"].as_f64()?;
            let lon = p["lon"].as_f64()?;
            (lat.is_finite() && lon.is_finite()).then_some((
                lat,
                lon,
                p["segment"].as_i64().unwrap_or(-1),
            ))
        })
        .collect();
    if points.len() < 2 {
        return (slint::Image::from_rgb8(buffer), false);
    }
    let (lat0, lon0, _) = points[0];
    let factor = lat0.to_radians().cos();
    let xy: Vec<_> = points
        .iter()
        .map(|&(lat, lon, s)| {
            (
                ((lon - lon0 + 180.).rem_euclid(360.) - 180.) * factor,
                -(lat - lat0),
                s,
            )
        })
        .collect();
    let minx = xy.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let maxx = xy.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let miny = xy.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let maxy = xy.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let scale = ((W - 24) as f64 / (maxx - minx).max(0.00001))
        .min((H - 24) as f64 / (maxy - miny).max(0.00001));
    let screen: Vec<_> = xy
        .iter()
        .map(|&(x, y, s)| {
            (
                (W as f64 / 2. + (x - (minx + maxx) / 2.) * scale).round() as i32,
                (H as f64 / 2. + (y - (miny + maxy) / 2.) * scale).round() as i32,
                s,
            )
        })
        .collect();
    let pixels = buffer.make_mut_slice();
    let mut line_count = 0;
    for pair in screen.windows(2) {
        if pair[0].2 != pair[1].2 {
            continue;
        }
        line_count += 1;
        let (x0, y0, _) = pair[0];
        let (x1, y1, _) = pair[1];
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let x = x0 + (x1 - x0) * i / steps;
            let y = y0 + (y1 - y0) * i / steps;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (x, y) = (x + dx, y + dy);
                    if x >= 0 && y >= 0 && x < W as i32 && y < H as i32 {
                        pixels[y as usize * W + x as usize] = slint::Rgb8Pixel::new(168, 239, 206);
                    }
                }
            }
        }
    }
    (slint::Image::from_rgb8(buffer), line_count > 0)
}
