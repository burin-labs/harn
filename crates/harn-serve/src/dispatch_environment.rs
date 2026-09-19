//! The environment a dispatched function's subprocesses run under.
//!
//! The MCP and A2A dispatch surface has no way for a caller to declare an
//! environment policy, the way an ACP session does on `session/new`. Until it
//! does, a dispatched function's children inherit this server's environment,
//! which is what they have always done.
//!
//! What changed is that the inheriting is now *said*. Since harn#8477 the
//! process host refuses an inheriting spawn with no policy behind it, because
//! a spawn seam cannot tell a deliberate inherit apart from a forgotten one,
//! and treating the forgotten case as permission is how a credential reaches a
//! child nobody meant to hand it to.
//!
//! The declaration here is deliberately the permissive one, so this changes no
//! behaviour on this surface. Giving the surface a real policy to declare is
//! its own decision and its own change; keeping the call named and in a file
//! of its own is what makes that gap findable rather than implicit.

/// Installs the dispatch surface's environment declaration for as long as it
/// is held, and clears it on drop, including on the panicking path.
pub(crate) struct InheritedDispatchEnvironment;

impl InheritedDispatchEnvironment {
    pub(crate) fn install() -> Self {
        harn_vm::stdlib::process::set_session_environment(Some(
            harn_vm::security::SessionEnvironment::inherited(),
        ));
        Self
    }
}

impl Drop for InheritedDispatchEnvironment {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
    }
}
