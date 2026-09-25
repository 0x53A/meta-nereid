#[path = "../../shared/config_file.rs"]
mod file;
use std::io;
use std::path::Path;

pub fn save_roles(
    path: &Path,
    watchface: &[String],
    launcher: &[String],
    settings: &[String],
) -> io::Result<()> {
    file::save_values(
        path,
        &[
            ("watchface", shell_words::join(watchface)),
            ("launcher", shell_words::join(launcher)),
            ("settings", shell_words::join(settings)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_roles_preserves_timeout_and_other_settings() {
        let dir = std::env::temp_dir().join(format!("hoki-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shell.conf");
        std::fs::write(
            &path,
            "# note\ndisplay_timeout=15\nfuture_setting=yes\nwatchface=old\n",
        )
        .unwrap();
        let command = vec![
            "/path with spaces/face".into(),
            "argument with 'quote".into(),
            "".into(),
        ];
        save_roles(&path, &command, &[], &[]).unwrap();
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(data.contains("display_timeout=15\n"));
        assert!(data.contains("future_setting=yes\n"));
        assert!(data.contains("# note\n"));
        assert!(!data.contains("watchface=old"));
        let saved = data
            .lines()
            .find_map(|l| l.strip_prefix("watchface="))
            .unwrap();
        assert_eq!(shell_words::split(saved).unwrap(), command);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
