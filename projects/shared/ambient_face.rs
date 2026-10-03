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
    /// Wayland companion argv, launched as the compositor's user, without a shell.
    #[serde(default)]
    pub placeholder: Vec<String>,
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
            placeholder: Vec::new(),
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
            || self.placeholder.len() > 16
            || self.placeholder.iter().any(|arg| arg.len() > 4096 || arg.contains(['\0', '\n', '\r']))
            || self.placeholder.first().is_some_and(|exe| !std::path::Path::new(exe).is_absolute())
        {
            return Err("unsupported ambient face bundle".into());
        }
        Ok(())
    }

    pub fn placeholder_command(&self, id: &str) -> Vec<String> {
        if self.placeholder.is_empty() {
            vec!["/usr/lib/hoki-lp-placeholder".into(), id.into()]
        } else {
            self.placeholder.clone()
        }
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

    #[test]
    fn companions_are_argv_and_legacy_faces_get_the_default() {
        let mut face = Bundle::default();
        assert_eq!(face.placeholder_command("hoki-digital"), ["/usr/lib/hoki-lp-placeholder", "hoki-digital"]);
        face.placeholder = vec!["/usr/lib/custom face".into(), "arg with spaces".into()];
        assert!(face.validate().is_ok());
        assert_eq!(face.placeholder_command("ignored"), face.placeholder);
        face.placeholder[0] = "relative-program".into();
        assert!(face.validate().is_err());
        face.placeholder[0] = "/usr/lib/invalid\0program".into();
        assert!(face.validate().is_err());
        for data in [
            include_str!("../hoki-lp-watchface/deploy/hoki-digital.json"),
            include_str!("../hoki-lp-watchface/deploy/hoki-seconds.json"),
            include_str!("../hoki-lp-watchface/deploy/hoki-orbit.json"),
            include_str!("../hoki-lp-watchface/deploy/hoki-instrument.json"),
        ] {
            let face: Bundle = serde_json::from_str(data).unwrap();
            face.validate().unwrap();
            assert_eq!(face.placeholder.len(), 2);
        }
    }
}
