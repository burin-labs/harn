use std::{env, process};

mod freshness_evidence;
mod freshness_manifest;
mod gate_receipt;

const INTERNAL_EXECUTABLE_PATH_COMMAND: &str = "__internal-executable-path";

pub(crate) fn args_after_pre_runtime_command() -> Vec<String> {
    // `env::args()` panics on a non-UTF-8 argument; report it instead.
    let raw_args: Vec<String> = match env::args_os().map(|arg| arg.into_string()).collect() {
        Ok(args) => args,
        Err(arg) => {
            cli_diagnostic!("error: argument `{}` is not valid UTF-8", arg.display());
            process::exit(2);
        }
    };
    if handle_pre_runtime_command(&raw_args) {
        process::exit(0);
    }
    raw_args
}

fn handle_pre_runtime_command(raw_args: &[String]) -> bool {
    dispatch_namespace_helper(raw_args);
    if gate_receipt::handle(raw_args) {
        return true;
    }
    if freshness_evidence::handle(raw_args) {
        return true;
    }
    if !is_internal_executable_path_command(raw_args) {
        return false;
    }

    match env::current_exe() {
        Ok(path) => {
            println!("{}", path.display());
            true
        }
        Err(error) => {
            cli_diagnostic!("error: failed to resolve current executable path: {error}");
            process::exit(1);
        }
    }
}

/// Run the namespace helper while the process still has one thread, or return
/// so the ordinary dispatcher can handle the invocation.
fn dispatch_namespace_helper(raw_args: &[String]) {
    if let Some(Err(error)) = harn_vm::process_sandbox::run_namespace_helper_if_invoked(raw_args) {
        cli_diagnostic!("error: {error}");
        process::exit(1);
    }
}

fn is_internal_executable_path_command(raw_args: &[String]) -> bool {
    matches!(
        raw_args,
        [_, command] if command == INTERNAL_EXECUTABLE_PATH_COMMAND
    )
}

#[cfg(test)]
mod tests {
    use super::{is_internal_executable_path_command, INTERNAL_EXECUTABLE_PATH_COMMAND};

    #[test]
    fn internal_executable_path_command_requires_exact_private_shape() {
        assert!(is_internal_executable_path_command(&[
            "harn".to_string(),
            INTERNAL_EXECUTABLE_PATH_COMMAND.to_string(),
        ]));
        assert!(!is_internal_executable_path_command(&[
            "harn".to_string(),
            INTERNAL_EXECUTABLE_PATH_COMMAND.to_string(),
            "extra".to_string(),
        ]));
        assert!(!is_internal_executable_path_command(&[
            "harn".to_string(),
            "version".to_string(),
        ]));
    }
}
