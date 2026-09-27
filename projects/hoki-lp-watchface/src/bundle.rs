use serde::Deserialize;
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub version: u32,
    pub kind: String,
    pub name: String,
    pub foreground: u32,
    pub brightness: u16,
    pub dim_brightness: u16,
}
impl Default for Bundle {
    fn default() -> Self {
        Self {
            version: 1,
            kind: "digital-v1".into(),
            name: "Hoki Digital".into(),
            foreground: 0xffe0e0e0,
            brightness: 127,
            dim_brightness: 64,
        }
    }
}
impl Bundle {
    fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !matches!(
                self.kind.as_str(),
                "digital-v1" | "seconds-v1" | "orbit-v1" | "instrument-v1"
            )
            || self.name.is_empty()
            || self.name.len() > 80
            || self.foreground >> 24 != 255
            || self.brightness > 255
            || self.brightness == 0
            || self.dim_brightness > self.brightness
        {
            return Err("unsupported ambient face bundle".into());
        }
        Ok(())
    }
}
pub fn load(id: &str) -> Result<Bundle, String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid face ID".into());
    }
    let path = format!("/usr/share/hoki/ambient-faces/{id}.json");
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("ambient bundle too large".into());
    }
    let face: Bundle = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    face.validate()?;
    Ok(face)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unsupported_assets_and_brightness() {
        let mut b = Bundle::default();
        assert!(b.validate().is_ok());
        b.kind = "executable".into();
        assert!(b.validate().is_err());
        b.kind = "digital-v1".into();
        b.dim_brightness = 256;
        assert!(b.validate().is_err());
        assert!(load("../escape").is_err());
    }
}
