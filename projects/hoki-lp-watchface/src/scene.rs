//! Bounded scenes using previously accepted stock descriptors. Experimental
//! motion/layout still needs physical verification; no resource delete/replace.
use super::*;
fn word(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn float(b: &mut [u8], o: usize, v: f32) {
    word(b, o, v.to_bits());
}

pub fn bitmap_descriptor(id: u32, size: u32, x: f32, y: f32, kind: &str) -> [u8; 96] {
    let mut d = resources::drawable(id, size, size, x, y, resources::DrawableKind::Bitmap);
    word(
        &mut d,
        88,
        if matches!(kind, "dial" | "backing") {
            0
        } else {
            2
        },
    );
    match kind {
        "hand" => {
            d[24] = 1;
            float(&mut d, 28, 32.);
            float(&mut d, 32, 32.);
            float(&mut d, 36, 518400.);
            float(&mut d, 40, 6.);
            word(&mut d, 64, 3);
        }
        "blink" => {
            d[68] = 1;
            float(&mut d, 72, 500.);
            float(&mut d, 76, 500.);
        }
        "flip-x" | "flip-y" | "flip45" => {
            d[52] = 1;
            d[match kind {
                "flip-x" => 53,
                "flip-y" => 54,
                _ => 55,
            }] = 1;
            float(&mut d, 56, 1.);
            float(&mut d, 60, 1.);
        }
        _ => {}
    }
    d
}
impl Session<'_> {
    fn scene_bitmap(
        &self,
        id: u32,
        size: u32,
        x: f32,
        y: f32,
        kind: &str,
        png: &[u8],
    ) -> Result<(), String> {
        let d = bitmap_descriptor(id, size, x, y, kind);
        let reply = self.call(V1, 16, kind, |api, w| unsafe {
            (api.gbinder_writer_append_buffer_object)(w, d.as_ptr().cast(), d.len());
            (api.gbinder_writer_append_hidl_vec)(w, png.as_ptr().cast(), png.len() as u32, 1);
        })?;
        if reply.len() != 1 {
            return Err("unexpected scene asset response".into());
        }
        Ok(())
    }
    pub fn decorations(&self, bundle: &bundle::Bundle) -> Result<(), String> {
        match bundle.kind.as_str() {
            "orbit-v1" => {
                self.scene_bitmap(
                    14400,
                    96,
                    158.,
                    258.,
                    "dial",
                    include_bytes!("../assets/dial.png"),
                )?;
                self.scene_bitmap(
                    14401,
                    64,
                    174.,
                    274.,
                    "hand",
                    include_bytes!("../assets/hand.png"),
                )?;
            }
            "instrument-v1" => {
                for (id, x, kind) in [
                    (14402, 136., "flip-x"),
                    (14403, 194., "flip-y"),
                    (14404, 252., "flip45"),
                ] {
                    self.scene_bitmap(
                        id,
                        24,
                        x,
                        110.,
                        kind,
                        include_bytes!("../assets/chevron.png"),
                    )?;
                }
                self.scene_bitmap(
                    14405,
                    12,
                    200.,
                    252.,
                    "blink",
                    include_bytes!("../assets/dot.png"),
                )?;
                self.scene_bitmap(
                    14406,
                    96,
                    90.,
                    278.,
                    "backing",
                    include_bytes!("../assets/black.png"),
                )?;
                self.scene_bitmap(
                    14407,
                    96,
                    226.,
                    278.,
                    "backing",
                    include_bytes!("../assets/black.png"),
                )?;
                self.numeric(false, 14500, 90.)?;
                self.numeric(true, 14501, 226.)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn numeric(&self, color: bool, id: u32, x: f32) -> Result<(), String> {
        // Two autonomous 0..59 counters illustrate the legacy and colored
        // numeric APIs. These are clock-derived modulo counters, not live health values.
        let font_id = 14510;
        if !color {
            let mut font = [0u8; 16];
            for (o, v) in [(0, 48), (4, 64), (8, 10), (12, font_id)] {
                word(&mut font, o, v);
            }
            let png = include_bytes!("../digits.png");
            self.call(V1, 17, "numeric font", |api, w| unsafe {
                (api.gbinder_writer_append_buffer_object)(w, font.as_ptr().cast(), font.len());
                (api.gbinder_writer_append_hidl_vec)(w, png.as_ptr().cast(), png.len() as u32, 1);
            })?;
        }
        let mut d = resources::drawable(id, 96, 64, x, 278., resources::DrawableKind::DateTime);
        word(&mut d, 64, 2);
        word(&mut d, 88, 1);
        let mut n = [0u8; 56];
        for (o, v) in [
            (8, 0),
            (12, 0),
            (16, 59),
            (20, 1),
            (28, font_id),
            (32, 2),
            (36, 1),
            (40, u32::MAX),
            (48, 0xffa0e6dc),
            (52, 0xff000000),
        ] {
            word(&mut n, o, v);
        }
        float(&mut n, 24, 1000.);
        self.call(
            if color {
                "vendor.google_clockwork.sidekickgraphics@1.2::ISidekickGraphics"
            } else {
                V1
            },
            if color { 28 } else { 18 },
            "autonomous counter",
            |api, w| unsafe {
                (api.gbinder_writer_append_buffer_object)(w, d.as_ptr().cast(), d.len());
                (api.gbinder_writer_append_buffer_object)(
                    w,
                    n.as_ptr().cast(),
                    if color { 56 } else { 48 },
                );
            },
        )?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transforms_are_bounded_and_use_independent_flags() {
        let d = bitmap_descriptor(1, 64, 174., 274., "hand");
        assert_eq!(d[24], 1);
        assert_eq!(d[68], 0);
        assert_eq!(f32::from_le_bytes(d[40..44].try_into().unwrap()), 6.);
        let b = bitmap_descriptor(2, 12, 200., 252., "blink");
        assert_eq!(b[68], 1);
        assert_eq!(b[24], 0);
        for (kind, offset) in [("flip-x", 53), ("flip-y", 54), ("flip45", 55)] {
            let d = bitmap_descriptor(3, 24, 136., 110., kind);
            assert_eq!(d[52], 1);
            assert_eq!(d[offset], 1);
        }
    }
}
