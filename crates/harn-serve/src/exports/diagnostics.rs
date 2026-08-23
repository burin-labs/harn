//! Non-fatal `HARN-SRV-*` diagnostics gathered while reading export attributes.

/// A `HARN-SRV-*` diagnostic raised while building the export catalog.
///
/// These flag the malformed `@route(...)` / `@scopes(...)` attribute
/// forms that the collector would otherwise drop silently — leaving a
/// handler mis-routed, unmounted, or less scope-restricted than the
/// author intended. They are surfaced by the serve adapters at startup
/// (see [`emit_export_diagnostics`]) rather than aborting catalog
/// construction, so one bad attribute doesn't take down a script whose
/// other handlers are fine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportDiagnostic {
    /// Stable code so log scanners and editors can key on the condition.
    pub code: &'static str,
    /// 1-based source line of the offending attribute (0 when unknown).
    pub line: usize,
    pub message: String,
}

/// `@route` carries an argument that is not a string literal, so the
/// method/path positions are ambiguous and the handler is not mounted.
pub const ROUTE_ARG_NOT_STRING: &str = "HARN-SRV-001";
/// `@route` has the wrong number of arguments — it takes a path, or a
/// method and a path. The handler is not mounted.
pub const ROUTE_BAD_ARITY: &str = "HARN-SRV-002";
/// `@scopes` carries an argument that is not a string literal; that
/// scope requirement is dropped, leaving the route less restricted.
pub const SCOPES_ARG_NOT_STRING: &str = "HARN-SRV-003";
/// `@job` carries a non-string name, or more than one positional
/// argument. The function is not registered as a job.
pub const JOB_BAD_NAME: &str = "HARN-SRV-004";
/// `@schedule` is malformed — it takes a cron expression and an optional
/// timezone, both string literals. The schedule is dropped.
pub const SCHEDULE_BAD_ARGS: &str = "HARN-SRV-005";
/// `@queue` carries a non-string queue name, or the wrong number of
/// arguments. The queue binding is dropped.
pub const QUEUE_BAD_NAME: &str = "HARN-SRV-006";
/// `@retry(max:, backoff:)` carries an unrecognised argument shape — a
/// positional argument, non-integer `max`, or an unknown `backoff`
/// keyword. The offending field is dropped (the rest of the policy still
/// applies).
pub const RETRY_BAD_ARGS: &str = "HARN-SRV-007";
/// `@schedule` / `@queue` / `@retry` appears without a `@job` attribute.
/// Those modifiers only mean something on a job, so they are ignored.
pub const JOB_MODIFIER_WITHOUT_JOB: &str = "HARN-SRV-008";
/// `@stream` carries arguments — it is a bare marker. The marker is
/// dropped, so the route dispatches into the VM like any other handler.
pub const STREAM_BAD_ARGS: &str = "HARN-SRV-009";
/// `@stream` appears on a declaration without an HTTP route (no
/// `@route(...)`, no `handler_*` convention, or a pipeline). Streaming
/// only means something on a routed `pub fn`, so it is ignored.
pub const STREAM_WITHOUT_ROUTE: &str = "HARN-SRV-010";
/// `@raw` carries arguments — it is a bare marker. The marker is
/// dropped, so the route dispatches into the VM like any other handler.
pub const RAW_BAD_ARGS: &str = "HARN-SRV-011";
/// `@raw` appears on a declaration without an HTTP route. Raw-body
/// hand-off only means something on a routed `pub fn`, so it is ignored.
pub const RAW_WITHOUT_ROUTE: &str = "HARN-SRV-012";
/// `@raw` and `@stream` appear on the same declaration. They contradict
/// on body handling (`@stream` never reads the request body, `@raw`
/// buffers it for the provider), so `@raw` is dropped and the route
/// behaves as `@stream`.
pub const RAW_CONFLICTS_WITH_STREAM: &str = "HARN-SRV-013";
/// `@ws` carries arguments — it is a bare marker. The marker is dropped,
/// so the route dispatches into the VM like any other handler.
pub const WS_BAD_ARGS: &str = "HARN-SRV-014";
/// `@ws` appears on a declaration without an HTTP route. A WebSocket
/// upgrade only means something on a routed `pub fn`, so it is ignored.
pub const WS_WITHOUT_ROUTE: &str = "HARN-SRV-015";
/// `@ws` and `@raw` appear on the same declaration. A WebSocket
/// handshake carries no request body, but `@raw` exists to buffer one, so
/// they contradict; `@ws` is dropped and the route behaves as `@raw`.
/// (`@ws` + `@stream` is *not* a conflict — it is the combined route that
/// upgrades a genuine handshake and falls through to the stream otherwise.)
pub const WS_CONFLICTS_WITH_STREAM_OR_RAW: &str = "HARN-SRV-016";
/// `@policy(...)` carries an argument that is not the supported
/// `kinds: "..."` string form (an unknown key, a positional argument, or a
/// non-string value). The offending argument is dropped, leaving the
/// route's principal-kind guard incomplete; any host-side defense-in-depth
/// check still applies.
pub const POLICY_BAD_ARGS: &str = "HARN-SRV-017";
/// `@annotations(...)` carries something other than named boolean arguments, or
/// names a hint MCP does not define. The offending hint is dropped; the rest of
/// the declaration still applies.
pub const ANNOTATIONS_BAD_ARGS: &str = "HARN-SRV-018";

impl std::fmt::Display for ExportDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "{}: {} (line {})", self.code, self.message, self.line)
        } else {
            write!(f, "{}: {}", self.code, self.message)
        }
    }
}

/// Print catalog diagnostics to stderr at server startup, matching the
/// `[harn] …` banner the adapters already emit. Standalone serve
/// commands call this so authors see malformed attributes immediately;
/// embedders that build a router directly can read
/// [`super::ExportCatalog::diagnostics`] and render them in their own UI.
pub fn emit_export_diagnostics(diagnostics: &[ExportDiagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("[harn] warning: {diagnostic}");
    }
}
