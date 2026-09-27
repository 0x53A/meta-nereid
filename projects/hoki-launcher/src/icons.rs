use std::path::{Path, PathBuf};

// Bounded lookup in the themes shipped by AsteroidOS, then hicolor/pixmaps.
// Unsupported/missing icons simply leave a text-only row.
pub fn candidates(name: &str, roots: &[PathBuf]) -> Vec<PathBuf> {
    if name.is_empty() {
        return vec![];
    }
    if Path::new(name).is_absolute() {
        return vec![name.into()];
    }
    if name.contains('/') || name == "." || name == ".." {
        return vec![];
    }
    let names = if Path::new(name).extension().is_some() {
        vec![name.to_owned()]
    } else {
        vec![format!("{name}.svg"), format!("{name}.png")]
    };
    let mut result = Vec::new();
    for root in roots {
        for directory in [
            "",
            "asteroid",
            "asteroid/scalable",
            "asteroid/scalable/apps",
            "asteroid/scalable/actions",
            "hicolor/scalable/apps",
            "hicolor/64x64/apps",
            "hicolor/48x48/apps",
            "hicolor/32x32/apps",
        ] {
            for name in &names {
                result.push(root.join(directory).join(name));
            }
        }
    }
    result
}

pub fn load(name: &str) -> slint::Image {
    let roots = if let Some(paths) = std::env::var_os("HOKI_ICON_PATH") {
        std::env::split_paths(&paths).collect()
    } else {
        vec![
            "/usr/share/icons".into(),
            "/usr/share/asteroid-icons".into(),
            "/usr/share/pixmaps".into(),
        ]
    };
    candidates(name, &roots)
        .into_iter()
        .filter(|p| p.is_file())
        .find_map(|path| slint::Image::load_from_path(&path).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_absolute_and_theme_icons() {
        let roots = vec![PathBuf::from("/icons")];
        assert!(candidates("", &roots).is_empty());
        assert_eq!(
            candidates("/tmp/app.png", &roots),
            vec![PathBuf::from("/tmp/app.png")]
        );
        assert!(candidates("ios-book", &roots)
            .contains(&PathBuf::from("/icons/asteroid/scalable/apps/ios-book.svg")));
        assert!(candidates("../secret", &roots).is_empty());
        let spo2 =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hoki-spo2/deploy/hoki-spo2.svg");
        assert_eq!(
            slint::Image::load_from_path(&spo2).unwrap().size().width,
            48
        );
    }
}
