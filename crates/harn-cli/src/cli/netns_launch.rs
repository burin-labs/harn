/// Build a private network namespace and enter confinement, or finalize an
/// already-confined device mount handover, then exec the payload.
///
/// Hidden because no operator invokes this: the sandbox backend does, and only
/// when a policy asks for loopback-only child networking or device mount
/// finalization. It exists as a
/// separate entry point rather than as work the backend does inline because
/// the permission to create an unprivileged namespace is granted per
/// executable path by host policy on the distributions that restrict it, and
/// that grant has to name one stable installed file rather than whichever
/// build of the runtime happens to be running.
#[derive(clap::Args, Debug)]
pub(crate) struct NetnsLaunchArgs {
    /// Inherited Landlock ruleset descriptor. The Landlock handover refuses
    /// unavailable enforcement before launching this helper.
    #[arg(long = harn_vm::process_sandbox::NETNS_RULESET_FD_FLAG.trim_start_matches('-'), conflicts_with = "close_fd")]
    pub ruleset_fd: Option<i32>,
    /// The compiled seccomp program, hex-encoded.
    ///
    /// Required when constructing a namespace. Mutually exclusive with the
    /// device finalization mode, whose wrapper already installed the filter.
    #[arg(long = harn_vm::process_sandbox::NETNS_SECCOMP_FLAG.trim_start_matches('-'), required_unless_present = "close_fd", conflicts_with = "close_fd")]
    pub seccomp_hex: Option<String>,
    /// Verify and close one pinned device setup descriptor, as FD:absolute-path.
    /// Bubblewrap has already installed confinement in this mode.
    #[arg(long = harn_vm::process_sandbox::NETNS_CLOSE_FD_FLAG.trim_start_matches('-'))]
    pub close_fd: Vec<harn_vm::process_sandbox::DeviceMountFinalization>,
    /// The payload: program first, then its arguments.
    #[arg(trailing_var_arg = true, required = true)]
    pub payload: Vec<String>,
}

/// The helper invocation parsed on its own, before the runtime exists.
///
/// The pre-runtime dispatcher cannot parse the whole command tree, because
/// building that parse is part of what the runtime is started for, and the
/// helper must run while the process is still single-threaded. Flattening the
/// same argument type here keeps one definition of the arguments across both
/// entry points.
#[derive(clap::Parser, Debug)]
pub(crate) struct NetnsLaunchInvocation {
    #[command(flatten)]
    pub args: NetnsLaunchArgs,
}
