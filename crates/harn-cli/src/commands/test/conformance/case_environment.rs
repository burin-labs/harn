//! The environment a conformance case's subprocesses run under.
//!
//! A conformance case is a `.harn` script, and many of them shell out: they
//! resolve a repository root with `git`, re-exec `harn` against a generated
//! child script, or probe a toolchain. Those children need the runner's own
//! environment to find anything at all, so a case inherits.
//!
//! Since harn#8477 an inheriting spawn has to have a session environment
//! behind it. The process host cannot tell a deliberate inherit apart from a
//! forgotten one, and while it treated the forgotten case as permission, a
//! credential could reach a child nobody meant to hand it to. `harn run`
//! already launched a policy for the same reason; this is the conformance
//! runner's equivalent, and `inherited` reproduces its behaviour exactly.

/// Installs the conformance runner's environment declaration for one case and
/// clears it when the case ends, including on the panicking path.
pub(super) struct ConformanceCaseEnvironment;

impl ConformanceCaseEnvironment {
    pub(super) fn install() -> Self {
        harn_vm::stdlib::process::set_session_environment(Some(
            harn_vm::security::SessionEnvironment::inherited(),
        ));
        Self
    }
}

impl Drop for ConformanceCaseEnvironment {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
    }
}
