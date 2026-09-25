//! Desktop Entry parsing. Main-entry metadata and Exec arguments stay separate
//! from desktop actions; no recursive interpretation of shell launcher scripts.
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug)]
pub struct DesktopEntry {
    pub name: String,
    pub argv: Vec<String>,
}

fn unescape(value: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        result.push(match chars.next() {
            Some('s') => ' ',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('\\') => '\\',
            _ => return Err("invalid desktop string escape".into()),
        });
    }
    Ok(result)
}

pub fn parse(text: &str, path: &Path) -> Result<Option<DesktopEntry>, String> {
    let mut main = false;
    let mut seen_main = false;
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
            if main && seen_main {
                return Err("duplicate Desktop Entry section".into());
            }
            seen_main |= main;
            continue;
        }
        if !main {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if values.insert(key, value.trim()).is_some() {
                return Err(format!("duplicate key {key}"));
            }
        }
    }
    if values.get("Hidden") == Some(&"true") || values.get("NoDisplay") == Some(&"true") {
        return Ok(None);
    }
    if values.get("Type").is_some_and(|v| *v != "Application") {
        return Ok(None);
    }
    let name = unescape(values.get("Name").ok_or("missing Name")?)?;
    let icon = unescape(values.get("Icon").copied().unwrap_or(""))?;
    let exec = unescape(values.get("Exec").ok_or("missing Exec")?)?;
    let tokens = shell_words::split(&exec).map_err(|e| e.to_string())?;
    let mut argv = Vec::new();
    let mut file_codes = 0;
    for (index, token) in tokens.into_iter().enumerate() {
        if token == "%i" {
            if index == 0 {
                return Err("field code in executable name".into());
            }
            if !icon.is_empty() {
                argv.extend(["--icon".into(), icon.clone()]);
            }
            continue;
        }
        let mut output = String::new();
        let mut removed = false;
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                output.push(c);
                continue;
            }
            let code = chars.next();
            if index == 0 && code != Some('%') {
                return Err("field code in executable name".into());
            }
            match code {
                Some('%') => output.push('%'),
                Some('c') => output.push_str(&name),
                Some('k') => output.push_str(&path.to_string_lossy()),
                Some(code @ ('f' | 'u' | 'F' | 'U')) => {
                    if matches!(code, 'F' | 'U') && token != format!("%{code}") {
                        return Err("list field code must be a whole argument".into());
                    }
                    file_codes += 1;
                    removed = true; // Launcher opens no selected files.
                }
                Some('d' | 'D' | 'n' | 'N' | 'v' | 'm') => removed = true,
                _ => return Err("unknown or misplaced Exec field code".into()),
            }
        }
        if !output.is_empty() || !removed {
            argv.push(output);
        }
    }
    if file_codes > 1 {
        return Err("multiple file field codes".into());
    }
    if argv.first().is_none_or(|s| s.is_empty() || s.contains('='))
        || argv.iter().any(|s| s.contains('\0'))
    {
        return Err("invalid executable or argument".into());
    }
    Ok(Some(DesktopEntry { name, argv }))
}

pub fn resolve_exec(mut args: Vec<String>, lib_dir: &Path) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    if args.is_empty() {
        return args;
    }
    if Path::new(&args[0])
        .file_name()
        .is_some_and(|n| n == "invoker")
    {
        let mut target = 1;
        while target < args.len() && args[target].starts_with('-') {
            if args[target] == "--" {
                target += 1;
                break;
            }
            let takes_value = matches!(args[target].as_str(), "--type" | "--delay");
            target += if takes_value { 2 } else { 1 };
        }
        if target < args.len() && !args[target].ends_with(".so") {
            args.drain(..target);
        } else {
            return args;
        }
    }
    if !args[0].contains('/') {
        let binary = lib_dir.join(&args[0]);
        if binary
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            args[0] = binary.to_string_lossy().into_owned();
        }
    }
    // Otherwise execute the PATH command/script itself. Parsing `exec` lines
    // discards environment/conditionals and can recurse forever on cyclic wrappers.
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actions_cannot_override_main_command_or_visibility() {
        let app = parse("[Desktop Entry]\nName=Player\nExec=player\n[Desktop Action Delete]\nName=Delete\nExec=erase\nNoDisplay=true\n", Path::new("player.desktop")).unwrap().unwrap();
        assert_eq!(app.name, "Player");
        assert_eq!(app.argv, ["player"]);
        assert!(
            parse(
                "[Desktop Entry]\nHidden=true\n",
                Path::new("hidden.desktop")
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn quoted_arguments_field_codes_and_percent_remain_literal() {
        let text = "[Desktop Entry]\nName=My %f Player\nIcon=icon name\nExec=\"/path with spaces/player\" \"two words\" \"\" %% %c %k %i %U\n";
        let app = parse(text, Path::new("/apps/my player.desktop"))
            .unwrap()
            .unwrap();
        assert_eq!(
            app.argv,
            [
                "/path with spaces/player",
                "two words",
                "",
                "%",
                "My %f Player",
                "/apps/my player.desktop",
                "--icon",
                "icon name"
            ]
        );
        let wire = serde_json::to_string(&app.argv).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&wire).unwrap(),
            app.argv
        );
    }
    #[test]
    fn malformed_commands_are_rejected() {
        for exec in [
            "player %x",
            "player %F %U",
            "player --files=%F",
            "\"unterminated",
            "player %i-suffix",
            "%i player",
            "%f player",
        ] {
            assert!(
                parse(
                    &format!("[Desktop Entry]\nName=Test\nExec={exec}\n"),
                    Path::new("x")
                )
                .is_err(),
                "{exec}"
            );
        }
    }
    #[test]
    fn invoker_and_wrappers_preserve_argument_boundaries() {
        let args = vec![
            "invoker".into(),
            "--type=generic".into(),
            "/usr/lib/player".into(),
            "two words".into(),
        ];
        assert_eq!(
            resolve_exec(args, Path::new("/nonexistent")),
            ["/usr/lib/player", "two words"]
        );
        let qt = vec![
            "invoker".into(),
            "--type=qt".into(),
            "/usr/lib/player.so".into(),
        ];
        assert_eq!(resolve_exec(qt.clone(), Path::new("/nonexistent")), qt);
        // No shell-script recursion, even if a wrapper's exec line names itself.
        assert_eq!(
            resolve_exec(vec!["cyclic-wrapper".into()], Path::new("/nonexistent")),
            ["cyclic-wrapper"]
        );
    }
    #[test]
    fn desktop_string_and_command_escapes_are_decoded_in_order() {
        let app = parse(
            r#"[Desktop Entry]
Name=Escape test
Exec=player "a\\\\b" "\\$HOME"
"#,
            Path::new("x"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(app.argv, ["player", r"a\b", "$HOME"]);
    }
}
