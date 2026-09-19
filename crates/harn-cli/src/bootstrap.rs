use std::{env, process};

mod freshness_evidence;
mod freshness_manifest;
mod gate_receipt;

const INTERNAL_EXECUTABLE_PATH_COMMAND: &str = "__internal-executable-path";

pub(crate) fn args_after_pre_runtime_command() -> Vec<String> {
    let raw_args: Vec<String> = env::args().collect();
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
            eprintln!("error: failed to resolve current executable path: {error}");
            process::exit(1);
        }
    }
}

/// Run the namespace helper here, while the process still has one thread, or
/// return so the ordinary dispatcher can handle the invocation.
///
/// This is the only point at which the helper can run. Creating a user
/// namespace refuses a multi-threaded caller outright, and the subcommand
/// dispatcher runs inside the CLI's async runtime, whose worker threads are
/// already up by the time it is reached. Reached there, the helper fails at
/// `unshare` with a bare "invalid argument" that reads as a host that forbids
/// namespaces rather than as a caller that started too late, so the grant
/// looks unavailable on a host where it is in fact granted.
///
/// The invocation is recognised on raw argv rather than after parsing because
/// parsing is what the runtime is started for. It is still declared in the
/// command tree, so `--help`, the argument types, and the parse all stay in
/// one place, and the shared constants make the two sides one contract.
fn dispatch_namespace_helper(raw_args: &[String]) {
    if !is_namespace_helper_command(raw_args) {
        return;
    }
    let invocation = <crate::cli::NetnsLaunchInvocation as clap::Parser>::parse_from(
        std::iter::once(raw_args[0].as_str()).chain(raw_args[2..].iter().map(String::as_str)),
    );
    // The success type is uninhabited: a launch that worked has replaced this
    // process, so the only reachable arm is the failure one.
    let Err(error) = crate::commands::netns_launch::run(invocation.args);
    eprintln!("error: {error}");
    process::exit(1);
}

/// The helper is invoked as the first argument and nothing else, exactly as
/// the sandbox backend builds it.
fn is_namespace_helper_command(raw_args: &[String]) -> bool {
    raw_args
        .get(1)
        .is_some_and(|command| command == harn_vm::process_sandbox::NETNS_LAUNCH_SUBCOMMAND)
}

fn is_internal_executable_path_command(raw_args: &[String]) -> bool {
    matches!(
        raw_args,
        [_, command] if command == INTERNAL_EXECUTABLE_PATH_COMMAND
    )
}

#[cfg(test)]
mod tests {
    use super::{
        is_internal_executable_path_command, is_namespace_helper_command,
        INTERNAL_EXECUTABLE_PATH_COMMAND,
    };

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

    /// The argv the sandbox backend builds is the argv this dispatcher
    /// recognises.
    ///
    /// Drift here is silent and total: an unrecognised invocation falls
    /// through to the ordinary dispatcher, which reaches the helper only
    /// after the async runtime has started threads, and the namespace can no
    /// longer be created. The grant then reads as refused by the host. Both
    /// sides read one constant, and this asserts the shape around it rather
    /// than restating the name.
    #[test]
    fn the_namespace_helper_invocation_is_recognised_before_the_runtime() {
        let subcommand = harn_vm::process_sandbox::NETNS_LAUNCH_SUBCOMMAND.to_string();
        assert!(is_namespace_helper_command(&[
            "harn-netns-launch".to_string(),
            subcommand.clone(),
            "--seccomp-hex".to_string(),
            "00".to_string(),
            "--".to_string(),
            "/bin/true".to_string(),
        ]));
        assert!(!is_namespace_helper_command(&["harn".to_string()]));
        assert!(!is_namespace_helper_command(&[
            "harn".to_string(),
            "run".to_string(),
            subcommand,
        ]));
    }
}
