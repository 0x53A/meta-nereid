//! Presentation policy only: desktop commands are never rewritten by folders.
use crate::desktop::DesktopEntry;
use serde_json::Value;
use std::collections::BTreeSet;

pub fn default_folder(id: &str) -> &str {
    match id {
        "bt-pair" | "hoki-audio" | "hoki-nfc" | "hoki-connect-ui" => "Tools",
        "demo-asteroid-app" | "hoki-egui-demo" | "hoki-wasm-host" | "imu-test-app"
        | "asteroid-counter" | "asteroid-crab-rave" | "hoki-watchface" => "Demos & Tests",
        id if id.starts_with("asteroid-") => "AsteroidOS",
        _ => "",
    }
}

pub fn folder<'a>(app: &'a DesktopEntry, config: &'a Value) -> &'a str {
    config
        .get("folders")
        .and_then(|v| v.get(&app.id))
        .and_then(Value::as_str)
        .or(app.folder.as_deref())
        .unwrap_or_else(|| default_folder(&app.id))
}

pub fn visible(app: &DesktopEntry) -> bool {
    app.id != "hoki-launcher"
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    App(usize),
    Folder(String),
    Back,
}

pub fn rows(apps: &[DesktopEntry], config: &Value, current: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    if !current.is_empty() {
        rows.push(Row::Back);
    }
    if current.is_empty() {
        let folders: BTreeSet<_> = apps
            .iter()
            .filter(|app| visible(app))
            .map(|app| folder(app, config))
            .filter(|folder| !folder.is_empty())
            .collect();
        rows.extend(folders.into_iter().map(|name| Row::Folder(name.into())));
    }
    rows.extend(
        apps.iter()
            .enumerate()
            .filter(|(_, app)| visible(app) && folder(app, config) == current)
            .map(|(i, _)| Row::App(i)),
    );
    rows
}

pub fn read_config() -> Value {
    let path = std::env::var_os("HOKI_LAUNCHER_CONFIG")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config"))
                })
                .map(|p| p.join("hoki-launcher.json"))
        });
    let Some(path) = path else {
        return Value::Null;
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("Ignoring {}: {e}", path.display());
            Value::Null
        }),
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(id: &str) -> DesktopEntry {
        crate::desktop::parse(
            "[Desktop Entry]\nName=Example\nExec=example\n",
            &std::path::PathBuf::from(format!("{id}.desktop")),
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn grouping_overrides_and_tools() {
        let apps = vec![
            app("asteroid-weather"),
            app("asteroid-counter"),
            app("hoki-music"),
            app("bt-pair"),
        ];
        let config = Value::Null;
        assert_eq!(
            rows(&apps, &config, ""),
            vec![
                Row::Folder("AsteroidOS".into()),
                Row::Folder("Demos & Tests".into()),
                Row::Folder("Tools".into()),
                Row::App(2)
            ]
        );
        assert_eq!(
            rows(&apps, &config, "AsteroidOS"),
            vec![Row::Back, Row::App(0)]
        );
        assert_eq!(rows(&apps, &config, "Tools"), vec![Row::Back, Row::App(3)]);
        let config =
            serde_json::json!({"folders": {"asteroid-weather": "", "hoki-music": "Media"}});
        assert_eq!(
            rows(&apps, &config, ""),
            vec![
                Row::Folder("Demos & Tests".into()),
                Row::Folder("Media".into()),
                Row::Folder("Tools".into()),
                Row::App(0)
            ]
        );
        assert_eq!(rows(&[], &config, ""), vec![]);
    }
    #[test]
    fn desktop_folder_is_overridden_by_user_config() {
        let mut app = app("hoki-music");
        app.folder = Some("Media".into());
        assert_eq!(folder(&app, &Value::Null), "Media");
        assert_eq!(
            folder(&app, &serde_json::json!({"folders":{"hoki-music":""}})),
            ""
        );
    }
}
