use std::path::Path;

pub(super) fn truncate_catastrophe(args: &[String], active_cwd: Option<&Path>) -> Option<String> {
    let mut size_zero = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-s" || arg == "--size" {
            if args
                .get(index + 1)
                .map(|value| value.trim_start_matches(['+', '-']))
                == Some("0")
            {
                size_zero = true;
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-s") {
            size_zero |= value.trim_start_matches(['+', '-']) == "0";
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--size=") {
            size_zero |= value.trim_start_matches(['+', '-']) == "0";
            index += 1;
            continue;
        }
        if !arg.starts_with('-') {
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

pub(super) fn redirect_target_reason(target: &str, active_cwd: Option<&Path>) -> Option<String> {
    unresolved_target_reason(target, active_cwd)
        .map(|reason| format!("shell redirection is blocked: {reason}"))
}

fn unresolved_target_reason(target: &str, active_cwd: Option<&Path>) -> Option<String> {
    // Dynamic targets remain subject to ordinary approval and confinement after
    // expansion; the static floor must not guess which file they name.
    if target.contains(['$', '*', '?', '[', ']', '{', '}', '~']) {
        return None;
    }
    active_cwd
        .is_none()
        .then(|| format!("`{target}` cannot be resolved because no execution root is known"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(truncate_catastrophe(&args, None)
                .unwrap()
                .contains("no execution root"));
            assert!(truncate_catastrophe(&args, Some(Path::new("/workspace"))).is_none());
        }
    }

    #[test]
    fn dynamic_targets_do_not_gain_a_speculative_static_denial() {
        assert!(redirect_target_reason("$OUTPUT", None).is_none());
        assert!(redirect_target_reason("$OUTPUT", Some(Path::new("/workspace"))).is_none());
    }
}
