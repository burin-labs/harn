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
//! and treating the forgotten case as permission is how a credential reaches
//! a child nobody meant to hand it to.
//!
//! The declaration is deliberately the permissive one, so this changes no
//! behaviour on this surface, and it only applies when nothing else has
//! declared a policy: a dispatch that happens inside an agent session must
//! not replace that session's choice with a looser one. Giving this surface a
//! real policy to declare is its own decision; keeping the call named and in
//! a file of its own is what makes that gap findable rather than implicit.

/// Declare this surface's default environment for the life of one dispatch.
pub(crate) fn declare() -> harn_vm::stdlib::process::SessionEnvironmentGuard {
    harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::inherited(),
    )
}
