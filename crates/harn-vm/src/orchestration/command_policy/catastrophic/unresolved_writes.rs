use std::path::Path;

pub(super) fn truncate_catastrophe(
    args: &[String],
    dynamic: &[bool],
    active_cwd: Option<&Path>,
) -> Option<String> {
    let mut size_zero = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut index = 0;
    let mut options_ended = false;
    while index < args.len() {
        let arg = &args[index];
        if options_ended {
            if !dynamic.get(index).copied().unwrap_or(true) {
                targets.push(arg);
            }
            index += 1;
            continue;
        }
        if arg == "--" {
            options_ended = true;
            index += 1;
            continue;
        }
        if arg == "-s" || arg == "--size" {
            if !dynamic.get(index + 1).copied().unwrap_or(true)
                && args.get(index + 1).is_some_and(|value| size_is_zero(value))
            {
                size_zero = true;
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-s") {
            size_zero |= !dynamic.get(index).copied().unwrap_or(true) && size_is_zero(value);
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--size=") {
            size_zero |= !dynamic.get(index).copied().unwrap_or(true) && size_is_zero(value);
            index += 1;
            continue;
        }
        if !arg.starts_with('-') && !dynamic.get(index).copied().unwrap_or(true) {
            targets.push(arg);
        }
        index += 1;
    }
    if !size_zero {
        return None;
    }
    targets
        .iter()
        .find_map(|target| unresolved_target_reason(target, active_cwd))
        .map(|reason| format!("`truncate -s 0` is blocked: {reason}"))
}

fn size_is_zero(value: &str) -> bool {
    let magnitude = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    let suffix = magnitude.trim_start_matches('0');
    if suffix.len() == magnitude.len() {
        return false;
    }
    match suffix.as_bytes() {
        [] => true,
        [unit] | [unit, b'B'] | [unit, b'i', b'B'] => b"kKMGTPEZYRQ".contains(unit),
        _ => false,
    }
}

pub(super) fn redirect_target_reason(target: &str, active_cwd: Option<&Path>) -> Option<String> {
    unresolved_target_reason(target, active_cwd)
        .map(|reason| format!("shell redirection is blocked: {reason}"))
}

fn unresolved_target_reason(target: &str, active_cwd: Option<&Path>) -> Option<String> {
    // Callers supply resolved argv literals or targets the shell AST marked literal.
    active_cwd
        .is_none()
        .then(|| format!("`{target}` cannot be resolved because no execution root is known"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal_truncate(args: &[String], active_cwd: Option<&Path>) -> Option<String> {
        truncate_catastrophe(args, &vec![false; args.len()], active_cwd)
    }

    #[test]
    fn literal_writes_without_an_execution_root_are_still_refused() {
        assert!(redirect_target_reason("source.rs", None)
            .unwrap()
            .contains("no execution root"));
        for flag in [vec!["-s", "0"], vec!["--size=0"], vec!["-s0"]] {
            let args: Vec<String> = flag
                .into_iter()
                .chain(["source.rs"])
                .map(str::to_string)
                .collect();
            assert!(literal_truncate(&args, None)
                .unwrap()
                .contains("no execution root"));
            assert!(literal_truncate(&args, Some(Path::new("/workspace"))).is_none());
        }
    }

    #[test]
    fn known_roots_leave_literal_targets_to_ordinary_permissions() {
        assert!(redirect_target_reason("$OUTPUT", Some(Path::new("/workspace"))).is_none());
    }

    #[test]
    fn truncate_end_of_options_preserves_literal_root_checks() {
        let args: Vec<String> = ["-s", "0", "--", "-draft"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert!(literal_truncate(&args, None)
            .unwrap()
            .contains("no execution root"));
        assert!(literal_truncate(&args, Some(Path::new("/workspace"))).is_none());

        let filenames_only: Vec<String> = ["--", "-s0", "target"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert!(literal_truncate(&filenames_only, None).is_none());
    }

    #[test]
    fn equivalent_zero_sizes_keep_the_unresolved_root_floor() {
        for size in ["00", "000", "0k", "0KB", "0KiB", "000MB"] {
            let args: Vec<String> = ["-s", size, "source.rs"]
                .into_iter()
                .map(str::to_string)
                .collect();
            assert!(literal_truncate(&args, None).is_some(), "{size}");
            assert!(literal_truncate(&args, Some(Path::new("/workspace"))).is_none());
        }
        for size in ["0b", "0x0", "0.0", "0invalid", "1", "01", "1K"] {
            let args: Vec<String> = ["-s", size, "source.rs"]
                .into_iter()
                .map(str::to_string)
                .collect();
            assert!(literal_truncate(&args, None).is_none(), "{size}");
        }
    }

    #[test]
    fn literal_metacharacters_do_not_erase_the_unresolved_root_floor() {
        for target in ["$OUTPUT", "*", "?", "[draft]", "{draft}", "~draft"] {
            assert!(redirect_target_reason(target, None).is_some(), "{target}");
            assert!(redirect_target_reason(target, Some(Path::new("/workspace"))).is_none());
        }
    }

    #[test]
    fn expanded_truncate_targets_do_not_gain_a_speculative_static_denial() {
        for target in ["$OUTPUT", "*"] {
            let args: Vec<String> = ["-s", "0", target]
                .into_iter()
                .map(str::to_string)
                .collect();
            assert!(truncate_catastrophe(&args, &[false, false, true], None).is_none());
            assert!(truncate_catastrophe(&args, &[false, false, false], None).is_some());
            assert!(truncate_catastrophe(
                &args,
                &[false, false, false],
                Some(Path::new("/workspace")),
            )
            .is_none());
        }
    }
}
