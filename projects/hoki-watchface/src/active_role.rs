pub fn is_watchface(reply: &str) -> bool {
    let Ok(args) = shell_words::split(reply.trim()) else {
        return false;
    };
    args.first().is_some_and(|program| {
        std::path::Path::new(program)
            .file_name()
            .is_some_and(|name| name == "hoki-watchface")
    }) && args
        .iter()
        .skip(1)
        .any(|argument| argument == "--watchface")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_quoted_and_path_commands() {
        for reply in [
            "hoki-watchface --watchface\n",
            "/usr/lib/hoki-watchface --watchface\n",
            "'/directory with spaces/hoki-watchface' '--watchface'\n",
        ] {
            assert!(is_watchface(reply), "{reply}");
        }
    }

    #[test]
    fn unrelated_commands_and_setup_mode_are_not_active_watchfaces() {
        for reply in [
            "other-player /faces/hoki-watchface.pbw\n",
            "hoki-watchface-backup --watchface\n",
            "/usr/lib/hoki-watchface\n",
            "other-player --watchface hoki-watchface\n",
            "'hoki-watchface --watchface\n",
            "",
            "error: hoki-watchface unavailable\n",
        ] {
            assert!(!is_watchface(reply), "{reply}");
        }
    }
}
