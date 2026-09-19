/// Build a private network namespace, enter a confinement handed over as
/// data, and exec the payload.
///
/// Hidden because no operator invokes this: the sandbox backend does, and only
/// when a policy asks for loopback-only child networking. It exists as a
/// separate entry point rather than as work the backend does inline because
/// the permission to create an unprivileged namespace is granted per
/// executable path by host policy on the distributions that restrict it, and
/// that grant has to name one stable installed file rather than whichever
/// build of the runtime happens to be running.
#[derive(clap::Args, Debug)]
pub(crate) struct NetnsLaunchArgs {
    /// Inherited Landlock ruleset descriptor. Absent on a host with no
    /// Landlock, where the resolved fallback lets the run proceed on the
    /// syscall filter alone.
    #[arg(long = "ruleset-fd")]
    pub ruleset_fd: Option<i32>,
    /// The compiled seccomp program, hex-encoded.
    ///
    /// Required even when the ruleset is absent. A launch that installed no
    /// filter would run the payload unconfined while every layer above still
    /// reported the profile as enforced, which is the exact failure the
    /// transferable confinement was built to end.
    #[arg(long = "seccomp-hex")]
    pub seccomp_hex: String,
    /// The payload: program first, then its arguments.
    #[arg(trailing_var_arg = true, required = true)]
    pub payload: Vec<String>,
}
