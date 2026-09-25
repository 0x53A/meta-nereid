//! Diagnostic wire layout verified against stock Wear OS DrawableInfo.java.
//! No nested pointers occur in the 96-byte DrawableInfo.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum DrawableKind { Bitmap = 1, DateTime = 5 }
pub fn drawable(id:u32,width:u32,height:u32,x:f32,y:f32,kind:DrawableKind)->[u8;96]{
    let mut d=[0;96];
    for (off,value) in [(0,id),(4,width),(8,height)] {d[off..off+4].copy_from_slice(&value.to_le_bytes());}
    for (off,value) in [(16,x),(20,y)] {d[off..off+4].copy_from_slice(&value.to_le_bytes());}
    // Ordinary ambient visibility and TWM visibility are separate flags.
    // Stock SidekickService sets BOTH for bitmap and datetime components.
    d[12]=1;
    d[64..68].copy_from_slice(&(kind as u32).to_le_bytes());
    // Byte 24 is hasRotation; leave false to allow the vendor RLE path.
    // Byte 92 is displayInTwm (not ordinary display visibility).
    d[92]=1;
    // All optional transforms/animations disabled.
    d
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn diagnostic_fits_capability_bounds(){
        let d=drawable(14301,240,160,86.,126.,DrawableKind::Bitmap);
        assert_eq!(u32::from_le_bytes(d[4..8].try_into().unwrap()),240);
        assert_eq!(u32::from_le_bytes(d[8..12].try_into().unwrap()),160);
        assert_eq!(f32::from_le_bytes(d[16..20].try_into().unwrap()),86.);
        assert!(86.+240. <=412. && 126.+160.<=412.);
        assert!(d[25..64].iter().chain(d[68..92].iter()).all(|b|*b==0));
        assert_eq!(d[92],1);
    }
}

/// Fail before reset/upload if the fixed diagnostic does not fit this device.
/// Available memory is a vendor-reported limit; encoded size is checked by HAL.
pub fn validate_capabilities(c:&crate::decode::Capabilities)->Result<(),String> {
    if c.available_memory == 0 { return Err("vendor reported zero resource memory".into()); }
    if c.width < 350 || c.height < 286 { return Err("diagnostic bitmap/clock exceeds display bounds".into()); }
    if c.rgb_bits != [5,6,5] || c.palette_size < 2 {
        return Err(format!("unexpected color capabilities: {:?}, palette {}",c.rgb_bits,c.palette_size));
    }
    Ok(())
}
#[cfg(test)] mod capability_tests {
    use super::*;
    #[test] fn rejects_uninitialized_or_incompatible_capabilities() {
        let mut c=crate::decode::Capabilities {operations:0x1102000b,rgb_bits:[5,6,5],palette_size:16,color_tail:[0;4],available_memory:120651,width:412,height:412};
        assert!(validate_capabilities(&c).is_ok());
        c.available_memory=0; assert!(validate_capabilities(&c).is_err());
        c.available_memory=120651;
        c.width=349; assert!(validate_capabilities(&c).is_err()); c.width=412;
        c.height=285; assert!(validate_capabilities(&c).is_err()); c.height=412;
        c.palette_size=1; assert!(validate_capabilities(&c).is_err()); c.palette_size=16;
        c.rgb_bits=[3,3,2]; assert!(validate_capabilities(&c).is_err());
    }
}

#[cfg(test)] mod stock_contract_tests {
    use super::*;
    #[test] fn datetime_matches_stock_visible_descriptor() {
        assert_eq!(&drawable(14303,192,64,110.,174.,DrawableKind::DateTime)[..],
            &include_bytes!("../tests/fixtures/datetime-wearos.bin")[..]);
    }
    #[test] fn bitmap_matches_stock_visible_generic_descriptor() {
        assert_eq!(&drawable(14301,240,160,86.,126.,DrawableKind::Bitmap)[..],
            &include_bytes!("../tests/fixtures/bitmap-wearos.bin")[..]);
    }
}

/// v1.2 CustomFontInfo and GlyphInfoU, matching stock SidekickService.
pub fn custom_digit_font(id:u32)->([u8;36],[u8;40]) {
    let mut font=[0u8;36];
    for (i,v) in [48u32,64,10,id].iter().enumerate() {
        font[4*i..4*i+4].copy_from_slice(&v.to_le_bytes());
    }
    // Stock caller leaves ascent/bottom/descent/leading/top at zero.
    let mut glyphs=[0u8;40];
    for digit in 0..10 {
        glyphs[digit*4..digit*4+2].copy_from_slice(&48u16.to_le_bytes());
        glyphs[digit*4+2..digit*4+4].copy_from_slice(&(0x30u16+digit as u16).to_le_bytes());
    }
    (font,glyphs)
}
#[cfg(test)] mod custom_font_tests {
    use super::*;
    #[test] fn unicode_digit_font_matches_stock_wire_contract() {
        let (font,glyphs)=custom_digit_font(14302);
        assert_eq!(&font[..],&include_bytes!("../tests/fixtures/custom-digit-font.bin")[..]);
        assert_eq!(&glyphs[..],&include_bytes!("../tests/fixtures/unicode-digit-glyphs.bin")[..]);
    }
}
