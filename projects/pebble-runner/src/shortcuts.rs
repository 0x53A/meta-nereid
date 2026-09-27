//! User desktop entries for PBWs that Pebble Runner can open directly.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub type InstalledApp = (String, String, String, bool);
const PREFIX: &str = "pebble-runner-pbw-";
const MARKER: &str = "X-ManagedBy=pebble-runner";

fn is_managed(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| {
        text.lines().any(|line| line == MARKER)
    })
}

pub fn applications_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|p| p.join("applications"))
}

pub fn encode_filename(filename: &str) -> String {
    filename.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn decode_filename(encoded: &str) -> Option<String> {
    if encoded.len() % 2 != 0 || encoded.is_empty() || !encoded.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    let filename = String::from_utf8(bytes?).ok()?;
    if filename.ends_with(".pbw") && Path::new(&filename).file_name()?.to_str()? == filename {
        Some(filename)
    } else {
        None
    }
}

fn desktop_name(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub fn sync(apps: &[InstalledApp]) -> Result<(), String> {
    let Some(dir) = applications_dir() else {
        return Err("HOME and XDG_DATA_HOME are unset".into());
    };
    sync_to(&dir, apps)
}

pub fn sync_to(dir: &Path, apps: &[InstalledApp]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut expected = HashSet::new();
    for (name, _, filename, _) in apps {
        if decode_filename(&encode_filename(filename)).as_deref() != Some(filename) {
            continue;
        }
        let id = format!("{PREFIX}{}", encode_filename(filename));
        let path = dir.join(format!("{id}.desktop"));
        expected.insert(path.clone());
        let contents = format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec=/usr/lib/pebble-runner --app-id {}\nIcon=ios-apps-outline\nCategories=Applications;\nX-Hoki-Folder=Pebble apps\n{MARKER}\n",
            desktop_name(name), encode_filename(filename)
        );
        if std::fs::read_to_string(&path).ok().as_deref() == Some(&contents) {
            continue;
        }
        if path.exists() && !is_managed(&path) {
            eprintln!("Leaving unmanaged desktop entry: {}", path.display());
            continue;
        }
        let (temp, mut file) = (0..100)
            .find_map(|n| {
                let temp = dir.join(format!("{id}.{}.{}.tmp", std::process::id(), n));
                std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)
                    .ok().map(|file| (temp, file))
            })
            .ok_or_else(|| format!("cannot create temporary desktop entry for {id}"))?;
        use std::io::Write;
        file.write_all(contents.as_bytes()).map_err(|e| format!("{}: {e}", temp.display()))?;
        drop(file);
        std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".desktop"))
            || expected.contains(&path) {
            continue;
        }
        if is_managed(&path) {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_follow_installed_apps_and_keep_foreign_files() {
        let dir = std::env::temp_dir().join(format!("pebble-shortcuts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let filename = "my game.pbw";
        let apps = vec![("A\\B\nGame".into(), "Author".into(), filename.into(), false)];
        sync_to(&dir, &apps).unwrap();
        let id = encode_filename(filename);
        let path = dir.join(format!("{PREFIX}{id}.desktop"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Name=A\\\\B\\nGame\n"));
        assert!(text.contains("X-Hoki-Folder=Pebble apps\n"));
        assert!(text.contains("X-ManagedBy=pebble-runner\n"));
        assert_eq!(decode_filename(&id).as_deref(), Some(filename));
        let foreign = dir.join(format!("{PREFIX}foreign.desktop"));
        std::fs::write(&foreign, "[Desktop Entry]\nName=Foreign\n").unwrap();
        std::fs::write(&path, "[Desktop Entry]\nName=Mine\n").unwrap();
        sync_to(&dir, &apps).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[Desktop Entry]\nName=Mine\n");
        sync_to(&dir, &[]).unwrap();
        assert!(path.exists());
        assert!(foreign.exists());
        std::fs::remove_file(&path).unwrap();
        sync_to(&dir, &apps).unwrap();
        sync_to(&dir, &[]).unwrap();
        assert!(!path.exists());
        std::fs::remove_file(foreign).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
