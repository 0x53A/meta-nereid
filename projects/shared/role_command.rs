//! Commands sent to the compositor's line-oriented control socket.
use std::path::Path;

pub fn watchface(executable: &Path, pbw: Option<&Path>) -> std::io::Result<String> {
    let mut args = vec![executable.as_os_str(), std::ffi::OsStr::new("--watchface")];
    if let Some(path) = pbw {
        args.push(path.as_os_str());
    }
    let mut quoted = Vec::new();
    for arg in args {
        let value = arg
            .to_str()
            .filter(|s| !s.contains(['\n', '\r', '\0']))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid role command path",
                )
            })?;
        quoted.push(format!("'{}'", value.replace('\'', "'\\''")));
    }
    Ok(format!("set-watchface {}", quoted.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_paths_and_rejects_line_injection() {
        assert_eq!(
            watchface(Path::new("/a b/it's"), Some(Path::new("/face.pbw"))).unwrap(),
            "set-watchface '/a b/it'\\''s' '--watchface' '/face.pbw'"
        );
        assert!(watchface(Path::new("/a\nquit"), None).is_err());
    }
}
