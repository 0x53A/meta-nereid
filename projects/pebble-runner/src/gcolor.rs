/// Pebble GColor8 format: AARRGGBB (2 bits per channel)
/// Convert to RGBA8888
pub fn gcolor8_to_rgba(c: u8) -> [u8; 4] {
    let a2 = (c >> 6) & 0x03;
    let r2 = (c >> 4) & 0x03;
    let g2 = (c >> 2) & 0x03;
    let b2 = c & 0x03;

    // Scale 2-bit to 8-bit: 0->0, 1->85, 2->170, 3->255
    let scale = |v: u8| -> u8 { v * 85 };

    [scale(r2), scale(g2), scale(b2), scale(a2)]
}

/// Pebble's 64-color palette (the named colors)
#[allow(dead_code)]
pub mod colors {
    pub const BLACK: u8 = 0b11_00_00_00;
    pub const OXFORD_BLUE: u8 = 0b11_00_00_01;
    pub const DUKE_BLUE: u8 = 0b11_00_00_10;
    pub const BLUE: u8 = 0b11_00_00_11;
    pub const DARK_GREEN: u8 = 0b11_00_10_00;
    pub const MIDNIGHT_GREEN: u8 = 0b11_00_10_01;
    pub const COBALT_BLUE: u8 = 0b11_00_10_10;
    pub const BLUE_MOON: u8 = 0b11_00_10_11;
    pub const GREEN: u8 = 0b11_00_11_00;
    pub const MALACHITE: u8 = 0b11_00_11_10;
    pub const CYAN: u8 = 0b11_00_11_11;
    pub const BULGARIAN_ROSE: u8 = 0b11_01_00_00;
    pub const IMPERIAL_PURPLE: u8 = 0b11_01_00_01;
    pub const INDIGO: u8 = 0b11_01_00_10;
    pub const ELECTRIC_ULTRAMARINE: u8 = 0b11_01_00_11;
    pub const RED: u8 = 0b11_11_00_00;
    pub const ORANGE: u8 = 0b11_11_10_00;
    pub const YELLOW: u8 = 0b11_11_11_00;
    pub const WHITE: u8 = 0b11_11_11_11;
    pub const CLEAR: u8 = 0b00_00_00_00;
}
