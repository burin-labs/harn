use std::{env, process};

const INTERNAL_EXECUTABLE_PATH_COMMAND: &str = "__internal-executable-path";

pub(crate) fn args_after_pre_runtime_command() -> Vec<String> {
    let raw_args: Vec<String> = env::args().collect();
    if handle_pre_runtime_command(&raw_args) {
        process::exit(0);
    }
    raw_args
}

fn handle_pre_runtime_command(raw_args: &[String]) -> bool {
    if !is_internal_executable_path_command(raw_args) {
        return false;
    }

    match env::current_exe() {
        Ok(path) => {
            println!("{}", path.display());
            true
        }
        Err(error) => {
            eprintln!("error: failed to resolve current executable path: {error}");
            process::exit(1);
        }
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
