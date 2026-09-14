use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{Attribute, AttributeArg, Node, TypeExpr};

use crate::limits::{limits_and_budget_from_attributes, BudgetSpec, RouteLimits};
use crate::DispatchError;

mod diagnostics;
mod mcp_metadata;
mod schema_projection;
mod tool_catalog;

pub use diagnostics::{
    emit_export_diagnostics, ExportDiagnostic, ANNOTATIONS_BAD_ARGS, JOB_BAD_NAME,
    JOB_MODIFIER_WITHOUT_JOB, POLICY_BAD_ARGS, QUEUE_BAD_NAME, RAW_BAD_ARGS,
    RAW_CONFLICTS_WITH_STREAM, RAW_WITHOUT_ROUTE, RETRY_BAD_ARGS, ROUTE_ARG_NOT_STRING,
    ROUTE_BAD_ARITY, SCHEDULE_BAD_ARGS, SCOPES_ARG_NOT_STRING, STREAM_BAD_ARGS,
    STREAM_WITHOUT_ROUTE, WS_BAD_ARGS, WS_CONFLICTS_WITH_STREAM_OR_RAW, WS_WITHOUT_ROUTE,
};
pub use mcp_metadata::ToolAnnotations;
pub use schema_projection::type_expr_accepts_json_object;

#[derive(Clone, Debug, PartialEq)]
pub struct ExportedParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub has_default: bool,
    pub rest: bool,
}

impl ExportedParam {
    /// Whether this parameter accepts a JSON object value — an inline or aliased
    /// object shape (declared type, or a projected `inputSchema` carrying
    /// `"type": "object"` / `properties`) or a bare `dict`. The single owner of
    /// the "is this a wrapper object parameter" question, shared by the A2A
    /// structured-message lift and the MCP flat-argument lift so both adapters
    /// agree on which single-parameter tools take an object.
    pub fn accepts_json_object(&self) -> bool {
        self.type_expr
            .as_ref()
            .is_some_and(type_expr_accepts_json_object)
            || self
                .input_schema
                .get("type")
                .and_then(serde_json::Value::as_str)
                == Some("object")
            || self.input_schema.get("properties").is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportedCallableKind {
    Function,
    Pipeline,
}

/// Declarative route auth-policy metadata declared via `@policy(...)`,
/// composing with the `@scopes` requirement rather than replacing it.
/// `allowed_kinds` is enforced by the `harn serve site` admission layer;
/// `match_labels` and `method_guards` are exported for audit tooling that
/// needs to confirm a handler declares resource/tenant/JSON-RPC method
/// guards implemented by `std/harness/policy.require_policy(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutePolicy {
    /// Principal kinds permitted to invoke the route (e.g. `"operator"`,
    /// `"tenant"`). Empty means the policy imposes no kind restriction.
    pub allowed_kinds: BTreeSet<String>,
    /// Runtime match labels this route declares (e.g. `"tenant"` or
    /// `"owner"`). The `.harn` handler enforces the comparison with
    /// `require_policy`; the catalog surfaces the labels for reviewers.
    pub match_labels: BTreeSet<String>,
    /// Method-specific runtime guard names this route declares (e.g.
    /// `"doc.read"` / `"doc.write"` for JSON-RPC bodies).
    pub method_guards: BTreeSet<String>,
}

impl RoutePolicy {
    /// Whether the policy imposes no restriction at all — used to collapse
    /// a `@policy` that parsed to nothing effective back to `None`.
    pub fn is_empty(&self) -> bool {
        self.allowed_kinds.is_empty()
            && self.match_labels.is_empty()
            && self.method_guards.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct ExportedFunction {
    pub name: String,
    pub kind: ExportedCallableKind,
    /// Short label taken from the first line of the declaration's doc comment.
    /// MCP clients show this next to the programmatic `name`.
    pub title: Option<String>,
    /// The declaration's own doc comment, verbatim.
    ///
    /// This is what an MCP client is shown when it asks what the tool does. It
    /// comes from the doc comment rather than a separate attribute because a
    /// description that lives anywhere but next to the code is a description
    /// that goes stale: the author already writes one, and duplicating it into
    /// an attribute creates two answers that can disagree.
    pub description: Option<String>,
    /// Behavior hints declared with `@annotations(...)`, projected onto the MCP
    /// tool. `None` leaves the client on the protocol's own conservative
    /// defaults, which is the honest answer when a script has not said.
    pub annotations: Option<ToolAnnotations>,
    pub params: Vec<ExportedParam>,
    pub return_type: Option<TypeExpr>,
    pub throws_type: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    pub error_schema: Option<serde_json::Value>,
    /// Scopes the caller's credential must carry to invoke this function,
    /// for *every* HTTP method (the method-agnostic baseline). Populated
    /// from un-prefixed `@scopes("...", "...")` literals on the
    /// declaration; empty when no such literal is present, meaning the
    /// route is unrestricted beyond whatever scopes the auth method
    /// enforces globally. The dispatch-level scope check (API / A2A / MCP
    /// and the site VM backstop) reads this baseline set, so it stays the
    /// strict-subset floor of whatever a per-method route additionally
    /// requires.
    pub required_scopes: BTreeSet<String>,
    /// Additional scopes required only for specific HTTP methods, declared
    /// with a method-prefixed `@scopes("GET read:x", "PUT write:x")`
    /// literal (see [`scopes_from_attributes`] for the grammar). The site
    /// adapter unions a request's resolved requirement as
    /// `required_scopes ∪ method_scopes[method]`; methods absent from the
    /// map fall back to the `required_scopes` baseline. Only the
    /// `harn serve site` HTTP admission layer consults this map — the
    /// dispatch-level adapters (API / A2A / MCP) have no per-method HTTP
    /// surface and use `required_scopes` alone. Empty for the common
    /// uniform case, keeping the per-method path zero-cost.
    pub method_scopes: BTreeMap<String, BTreeSet<String>>,
    /// Declarative auth policy declared via `@policy(...)` — today, the set
    /// of allowed principal kinds the dispatch must match, composing with
    /// `required_scopes`. `None` when no `@policy` is present (or it parsed
    /// to nothing effective). Consulted by the `harn serve site` admission
    /// layer (after the scope check) and exposed for audit. See
    /// [`RoutePolicy`].
    pub policy: Option<RoutePolicy>,
    /// Rate / backpressure ceilings declared via `@limits(...)`. `None`
    /// when the route is unbounded — the dispatch path short-circuits
    /// cheaply when both `limits` and `budget` are absent.
    pub limits: Option<RouteLimits>,
    /// Per-dispatch resource budget declared via `@budget(...)` (LLM
    /// cost / token / pg query / MCP call ceilings). `None` when no
    /// budget caps were declared.
    pub budget: Option<BudgetSpec>,
    /// HTTP route this function answers when hosted by `harn serve site`.
    /// Populated from a `@route("METHOD", "/path")` attribute, or
    /// inferred from a `handler_*` naming convention when the attribute is
    /// absent. `None` for functions that are dispatch-only (API/A2A/MCP)
    /// and not meant to be reached over a bare HTTP path.
    pub route: Option<RouteSpec>,
    /// Worker/job execution surface declared via `@job("name")`. `None`
    /// for ordinary `pub fn` handlers; `Some` marks a long-running /
    /// scheduled / operator-batch entrypoint that the worker adapter runs
    /// through the trigger dispatcher (retry / DLQ / budget / cancel all
    /// come free from the dispatcher). See [`JobSpec`].
    pub job: Option<JobSpec>,
    /// `true` when the function carries a `@stream` attribute alongside
    /// its HTTP route. A streaming route never buffers the request body
    /// and never dispatches into the VM: after the site adapter's
    /// admission checks (the embedder's `SiteAuth` hook plus `@scopes`)
    /// it hands the request head to the embedder-registered
    /// `SiteStreamProvider`, which returns a live SSE/chunked response.
    /// The `.harn` function body is a declaration-only stub for such
    /// routes — the stream source lives in embedder Rust.
    pub stream: bool,
    /// `true` when the function carries a `@raw` attribute alongside its
    /// HTTP route. Like `@stream`, a raw route never dispatches into the
    /// VM — after admission the site adapter hands the request to the
    /// embedder's `SiteStreamProvider` — but unlike `@stream` the
    /// request body *is* read: it is buffered (up to the configured
    /// body limit) and passed to the provider as raw bytes, untouched
    /// by the utf8-lossy / base64 JSON-envelope encoding. This is the
    /// seam for binary and multipart uploads (pack publish) whose
    /// handling lives in embedder Rust. The `.harn` function body is a
    /// declaration-only stub, exactly as for `@stream`.
    pub raw: bool,
    /// `true` when the function carries a `@ws` attribute alongside its
    /// HTTP route. Like `@stream`, a `@ws` route never dispatches into
    /// the VM: after the site adapter's admission checks (the embedder's
    /// `SiteAuth` hook plus `@scopes`) it performs the WebSocket upgrade
    /// and hands the upgrade handle to the embedder's
    /// `SiteStreamProvider::upgrade`, which drives the socket. The marker
    /// is the seam for embedder routes that need a real WebSocket
    /// connection (mirroring how `@stream` is the seam for SSE).
    ///
    /// `@ws` may be combined with `@stream` on one route: the adapter
    /// sniffs the request's `Upgrade`/`Connection` headers and routes a
    /// genuine WebSocket handshake to `SiteStreamProvider::upgrade` while
    /// every other request falls through to `SiteStreamProvider::open`
    /// (the SSE/stream path) — one route serving both transports (the
    /// gateway `/acp` carve-out). `@ws` still conflicts with `@raw` (a
    /// handshake carries no body, but `@raw` buffers one), so declaring
    /// that pair drops `@ws`. The `.harn` function body is a
    /// declaration-only stub.
    pub ws: bool,
}

/// A `.harn` worker/job entrypoint declared with `@job("name")`.
///
/// Jobs lower to `TriggerBindingSpec` and inherit retry, dead-letter,
/// budget, and cancellation behavior from [`Dispatcher`](harn_vm::Dispatcher).
///
/// Declared like the route/limits/budget attributes:
///
/// ```harn
/// @job("scan")
/// @schedule("0 * * * *", "UTC")   // optional — cron-driven daemon jobs
/// @queue("scan-jobs")             // optional — worker-queue fan-out
/// @retry(max: 3, backoff: "exponential")
/// @budget(llm_cost_usd: 0.50)
/// @scopes("scan:run")
/// pub fn scan(harness: Harness, event: TriggerEvent) -> dict { ... }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSpec {
    /// Stable job name; used as the trigger-binding id. Defaults to the
    /// function name when `@job()` is written with no argument.
    pub name: String,
    /// Cron expression (+ optional timezone) from `@schedule(...)`. Only
    /// the `harn serve worker` daemon acts on this; the one-shot
    /// `harn run --as-job` path ignores it. `None` for queue / one-shot
    /// jobs.
    pub schedule: Option<ScheduleSpec>,
    /// Worker-queue name from `@queue("q")`. `None` for inline jobs.
    pub queue: Option<String>,
    /// Retry policy from `@retry(max:, backoff:)`. `None` falls back to
    /// the dispatcher default (`TriggerRetryConfig::default`).
    pub retry: Option<RetrySpec>,
}

/// Cron schedule declared via `@schedule("expr", "tz")`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleSpec {
    /// Cron expression (5- or 6-field), passed verbatim to the cron
    /// connector.
    pub cron: String,
    /// IANA timezone name; `None` means the connector's default (UTC).
    pub timezone: Option<String>,
}

/// Retry policy declared via `@retry(max: N, backoff: "...")`.
///
/// Mirrors the trigger DSL's `retry: {max, policy}` shape. The worker
/// adapter maps this onto `harn_vm::TriggerRetryConfig` so the dispatcher
/// applies it unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrySpec {
    /// Maximum total attempts. `0` (or absent) defers to the dispatcher
    /// default.
    pub max_attempts: u32,
    /// Backoff strategy keyword: `svix` (default), `linear`, or
    /// `exponential`.
    pub backoff: RetryBackoff,
}

/// Backoff keyword from `@retry(backoff: "...")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RetryBackoff {
    /// Svix-style increasing schedule — the dispatcher default.
    #[default]
    Svix,
    /// Fixed delay between attempts.
    Linear,
    /// Doubling delay, capped.
    Exponential,
}

/// An HTTP method + path a `.harn` handler answers under `harn serve
/// site`. Declared with `@route("GET", "/users/{id}")` or inferred from
/// the `handler_*` naming convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteSpec {
    /// Uppercased HTTP method (`GET`, `POST`, …), or `*` to answer every
    /// method on the path — the handler inspects `req.method` itself.
    pub method: String,
    /// axum-style path with `{param}` captures, always rooted at `/`.
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct ExportCatalog {
    pub script_path: PathBuf,
    pub functions: BTreeMap<String, ExportedFunction>,
    /// The script's leading doc comment, served as the MCP server's
    /// `instructions` -- the one place a server can tell a client how its tools
    /// fit together before the client has called any of them.
    pub instructions: Option<String>,
    /// Non-fatal `HARN-SRV-*` diagnostics gathered while collecting the
    /// route/scope attributes. Empty for a well-formed script.
    pub diagnostics: Vec<ExportDiagnostic>,
}

impl ExportCatalog {
    pub fn from_path(path: &Path) -> Result<Self, DispatchError> {
        let source = fs::read_to_string(path).map_err(|error| {
            DispatchError::Io(format!("failed to read {}: {error}", path.display()))
        })?;
        let program = harn_parser::parse_source(&source).map_err(|error| {
            DispatchError::Validation(format!("failed to parse {}: {error}", path.display()))
        })?;

        // Resolve local and imported type aliases through the same module graph
        // as the checker so served JSON schemas stay structural without a
        // second import resolver.
        let schema_resolver = schema_projection::resolver_for_module(path, &program);
        let source_lines: Vec<&str> = source.lines().collect();
        let mut functions = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for node in &program {
            let (attrs, inner) = harn_parser::peel_attributes(node);
            let Node::FnDecl {
                name,
                params,
                return_type,
                throws,
                is_pub,
                ..
            } = &inner.node
            else {
                continue;
            };
            if !*is_pub {
                continue;
            }

            let scopes = scopes_from_attributes(attrs, name, &mut diagnostics);
            let policy = policy_from_attributes(attrs, name, &mut diagnostics);
            let (limits, budget) =
                limits_and_budget_from_attributes(attrs).map_err(DispatchError::Validation)?;
            let route = route_from_attributes(attrs, name, &mut diagnostics);
            let stream = stream_from_attributes(attrs, name, route.as_ref(), &mut diagnostics);
            let raw = raw_from_attributes(attrs, name, route.as_ref(), stream, &mut diagnostics);
            let ws = ws_from_attributes(attrs, name, route.as_ref(), raw, &mut diagnostics);
            let public_params = schema_projection::public_params(params);
            let (title, description) = mcp_metadata::title_and_description(
                mcp_metadata::doc_comment_above(&source_lines, inner.span.line),
            );
            functions.insert(
                name.clone(),
                ExportedFunction {
                    name: name.clone(),
                    kind: ExportedCallableKind::Function,
                    title,
                    description,
                    annotations: mcp_metadata::annotations_from_attributes(
                        attrs,
                        name,
                        &mut diagnostics,
                    ),
                    params: schema_projection::exported_params(public_params, &schema_resolver),
                    return_type: return_type.clone(),
                    throws_type: throws.clone(),
                    input_schema: schema_resolver.json_schema_for_typed_params(public_params),
                    output_schema: return_type
                        .as_ref()
                        .and_then(|type_expr| schema_resolver.json_schema_for_type_expr(type_expr)),
                    error_schema: throws
                        .as_ref()
                        .and_then(|type_expr| schema_resolver.json_schema_for_type_expr(type_expr)),
                    required_scopes: scopes.baseline,
                    method_scopes: scopes.per_method,
                    policy,
                    limits,
                    budget,
                    route,
                    stream,
                    raw,
                    ws,
                    job: job_from_attributes(attrs, name, &mut diagnostics),
                },
            );
        }

        let has_public_exports = !functions.is_empty();
        for node in &program {
            let (attrs, inner) = harn_parser::peel_attributes(node);
            let Node::Pipeline {
                name,
                params,
                return_type,
                throws,
                is_pub,
                ..
            } = &inner.node
            else {
                continue;
            };
            if has_public_exports && !*is_pub {
                continue;
            }
            let scopes = scopes_from_attributes(attrs, name, &mut diagnostics);
            let policy = policy_from_attributes(attrs, name, &mut diagnostics);
            let (limits, budget) =
                limits_and_budget_from_attributes(attrs).map_err(DispatchError::Validation)?;
            // Pipelines never carry a route, so a `@stream` / `@raw` on
            // one is inert — diagnose it the same way as on an unrouted fn.
            let stream = stream_from_attributes(attrs, name, None, &mut diagnostics);
            let raw = raw_from_attributes(attrs, name, None, stream, &mut diagnostics);
            let ws = ws_from_attributes(attrs, name, None, raw, &mut diagnostics);
            let public_params = schema_projection::public_params(params);
            let (title, description) = mcp_metadata::title_and_description(
                mcp_metadata::doc_comment_above(&source_lines, inner.span.line),
            );
            functions
                .entry(name.clone())
                .or_insert_with(|| ExportedFunction {
                    name: name.clone(),
                    kind: ExportedCallableKind::Pipeline,
                    title,
                    description,
                    annotations: mcp_metadata::annotations_from_attributes(
                        attrs,
                        name,
                        &mut diagnostics,
                    ),
                    params: schema_projection::exported_params(public_params, &schema_resolver),
                    return_type: return_type.clone(),
                    throws_type: throws.clone(),
                    input_schema: schema_resolver.json_schema_for_typed_params(public_params),
                    output_schema: return_type
                        .as_ref()
                        .and_then(|type_expr| schema_resolver.json_schema_for_type_expr(type_expr)),
                    error_schema: throws
                        .as_ref()
                        .and_then(|type_expr| schema_resolver.json_schema_for_type_expr(type_expr)),
                    required_scopes: scopes.baseline,
                    method_scopes: scopes.per_method,
                    policy,
                    limits,
                    budget,
                    // Pipelines are dispatch-only; they never carry an
                    // HTTP route. Only `pub fn` handlers participate in
                    // `harn serve site`.
                    route: None,
                    stream,
                    raw,
                    ws,
                    job: job_from_attributes(attrs, name, &mut diagnostics),
                });
        }

        Ok(Self {
            script_path: path.to_path_buf(),
            functions,
            instructions: mcp_metadata::module_doc_comment(&source_lines),
            diagnostics,
        })
    }

    pub fn function(&self, name: &str) -> Option<&ExportedFunction> {
        self.functions.get(name)
    }

    /// Non-fatal `HARN-SRV-*` diagnostics gathered while collecting the
    /// route/scope attributes. Empty for a well-formed script.
    pub fn diagnostics(&self) -> &[ExportDiagnostic] {
        &self.diagnostics
    }
}

/// The two scope buckets a `@scopes(...)` attribute set resolves into: a
/// method-agnostic `baseline` required of every method, plus optional
/// `per_method` extras keyed by uppercased HTTP method. The site adapter
/// resolves a request's requirement as `baseline ∪ per_method[method]`;
/// every other adapter reads `baseline` alone.
#[derive(Default)]
struct ParsedScopes {
    baseline: BTreeSet<String>,
    per_method: BTreeMap<String, BTreeSet<String>>,
}

/// HTTP methods recognized as a `@scopes` literal prefix. A first
/// whitespace-delimited word matching one of these (case-insensitively)
/// switches the literal from the uniform form to the per-method form;
/// anything else is treated as a plain (baseline) scope, so an unusual
/// scope string that happens to contain a space is never misread as a
/// method prefix.
const SCOPE_METHOD_PREFIXES: [&str; 7] =
    ["GET", "PUT", "POST", "DELETE", "PATCH", "HEAD", "OPTIONS"];

/// Collect scope literals from any `@scopes(...)` attributes on a
/// declaration. Both positional and named arguments are accepted (named
/// args are useful for ergonomics like `@scopes(read: "personas:read")`
/// in callers that prefer key-value form); only string literals
/// contribute. Multiple `@scopes` attributes on the same declaration
/// union together.
///
/// ## Grammar
///
/// Each string literal is one of:
///
/// * **Uniform** — `"read:x"`: a bare scope required of every HTTP method.
///   This is the historic form and the default; it lands in the
///   `baseline` set unchanged.
/// * **Per-method** — `"GET read:x"`: an HTTP method (one of
///   [`SCOPE_METHOD_PREFIXES`], case-insensitive), a single run of
///   whitespace, then the scope. The scope is required *only* for that
///   method, in addition to the baseline. The whitespace separator can
///   never collide with a scope token (scopes use `:`-delimited words,
///   never spaces), so the uniform form is unambiguous and untouched.
///
/// So `@scopes("read:x", "PUT write:x")` requires `read:x` of every
/// method and additionally `write:x` of `PUT`. A method named in a
/// per-method literal but never given its own baseline still inherits the
/// baseline; a method *not* named anywhere falls back to the baseline
/// alone (resolution lives in the site adapter).
fn scopes_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> ParsedScopes {
    let mut parsed = ParsedScopes::default();
    for attr in attrs {
        if attr.name != "scopes" {
            continue;
        }
        for arg in &attr.args {
            match &arg.value.node {
                Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
                    match parse_scope_literal(value) {
                        Some((method, scope)) => {
                            parsed.per_method.entry(method).or_default().insert(scope);
                        }
                        None => {
                            parsed.baseline.insert(value.clone());
                        }
                    }
                }
                // A non-string scope is silently dropped by the
                // collector, which would leave the route *less*
                // restricted than the author wrote — worth a loud warning.
                _ => diagnostics.push(ExportDiagnostic {
                    code: SCOPES_ARG_NOT_STRING,
                    line: arg.span.line,
                    message: format!(
                        "`@scopes` on `{fn_name}` requires string-literal arguments; \
                         dropping a non-string scope leaves the route less restricted"
                    ),
                }),
            }
        }
    }
    parsed
}

/// Parse `@policy(...)` declarations into a [`RoutePolicy`].
///
/// Supported arguments are whitespace-separated string values:
/// `kinds`, `matches`, and `methods`. `kinds` is enforced at admission;
/// the others are stable audit metadata for runtime `require_policy`
/// guards. Any other argument shape (unknown key, positional, or
/// non-string value) is dropped with a [`POLICY_BAD_ARGS`] diagnostic,
/// leaving the route's cataloged policy incomplete — mirroring how a
/// dropped `@scopes` literal leaves the route less restricted. Returns
/// `None` when no `@policy` is present, or when every declaration parsed to
/// nothing effective (so the catalog's `policy` field means "has an
/// effective policy").
fn policy_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RoutePolicy> {
    let mut policy = RoutePolicy::default();
    for attr in attrs {
        if attr.name != "policy" {
            continue;
        }
        for arg in &attr.args {
            let target = match arg.name.as_deref() {
                Some("kinds") => Some(&mut policy.allowed_kinds),
                Some("matches") => Some(&mut policy.match_labels),
                Some("methods") => Some(&mut policy.method_guards),
                _ => None,
            };
            match (target, &arg.value.node) {
                (Some(target), Node::StringLiteral(value) | Node::RawStringLiteral(value)) => {
                    target.extend(value.split_whitespace().map(str::to_string));
                }
                _ => diagnostics.push(ExportDiagnostic {
                    code: POLICY_BAD_ARGS,
                    line: arg.span.line,
                    message: format!(
                        "`@policy` on `{fn_name}` accepts only string-valued `kinds`, `matches`, \
                         and `methods` arguments; dropping an unrecognized argument leaves the \
                         route's policy catalog incomplete"
                    ),
                }),
            }
        }
    }
    (!policy.is_empty()).then_some(policy)
}

/// Split a `@scopes` literal into an optional `(METHOD, scope)` pair.
///
/// Returns `Some((uppercased_method, scope))` when the literal begins with
/// a recognized HTTP method ([`SCOPE_METHOD_PREFIXES`], case-insensitive)
/// followed by whitespace and a non-empty scope; `None` for the uniform
/// form (no method prefix, or a leading word that is not a method), which
/// the caller files under the method-agnostic baseline verbatim.
fn parse_scope_literal(literal: &str) -> Option<(String, String)> {
    let (first, rest) = literal.split_once(char::is_whitespace)?;
    let method = first.to_ascii_uppercase();
    if !SCOPE_METHOD_PREFIXES.contains(&method.as_str()) {
        return None;
    }
    let scope = rest.trim();
    if scope.is_empty() {
        return None;
    }
    Some((method, scope.to_string()))
}

/// Resolve the HTTP route a `pub fn` answers under `harn serve site`.
///
/// Two ways to declare one, in priority order:
///
/// 1. An explicit `@route("METHOD", "/path")` attribute. The first
///    positional string is the method (case-insensitive; `"*"` or
///    `"ANY"` matches every method), the second is the path. A
///    single-argument form `@route("/path")` defaults the method to
///    `GET`. Paths are normalized to start with `/`.
/// 2. The `handler_<name>` naming convention. `pub fn handler_health()`
///    is mounted at `GET|POST /health`; a bare `pub fn handler()` mounts
///    at the site root `/`. This keeps the zero-config path the issue
///    calls for ("mounts every exported `pub fn handler_*` at `/<name>`")
///    while letting authors opt into precise routing with the attribute.
///
/// A present-but-malformed `@route` does not fall back to the naming
/// convention: it records a `HARN-SRV-*` diagnostic and returns `None`,
/// so the author sees the mistake instead of a silently different route.
///
/// Returns `None` for any other `pub fn`, so a script can export helper
/// functions (reachable via the API/A2A/MCP dispatch adapters) without
/// every one of them grabbing an HTTP path.
fn route_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RouteSpec> {
    // An explicit (even if malformed) `@route` overrides the naming
    // convention: an author who wrote one expects that path, not a
    // surprise fallback to `/<name>`. A malformed one yields `None` plus
    // a diagnostic, leaving the handler unmounted until they fix it.
    if attrs.iter().any(|attr| attr.name == "route") {
        return explicit_route_attribute(attrs, fn_name, diagnostics);
    }
    handler_convention_route(fn_name)
}

fn explicit_route_attribute(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RouteSpec> {
    let attr = attrs.iter().find(|attr| attr.name == "route")?;
    let literals: Vec<&str> = attr
        .args
        .iter()
        .filter_map(|arg| match &arg.value.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.as_str()),
            _ => None,
        })
        .collect();

    // Any non-string argument makes the method/path positions ambiguous
    // (e.g. `@route("GET", some_var)` would otherwise collapse to the
    // single-arg form and mis-mount at `/GET`), so refuse to guess.
    if literals.len() != attr.args.len() {
        diagnostics.push(ExportDiagnostic {
            code: ROUTE_ARG_NOT_STRING,
            line: attr.span.line,
            message: format!(
                "`@route` on `{fn_name}` requires string-literal arguments \
                 (`@route(\"/path\")` or `@route(\"METHOD\", \"/path\")`); handler not mounted"
            ),
        });
        return None;
    }

    match literals.as_slice() {
        // `@route("/path")` — method defaults to GET.
        [path] => Some(RouteSpec {
            method: "GET".to_string(),
            path: normalize_route_path(path),
        }),
        // `@route("METHOD", "/path")` — explicit method.
        [method, path] => Some(RouteSpec {
            method: normalize_route_method(method),
            path: normalize_route_path(path),
        }),
        // Zero args (`@route()`) or three-plus: the method/path pair is
        // under- or over-specified, so the route is undefined.
        _ => {
            diagnostics.push(ExportDiagnostic {
                code: ROUTE_BAD_ARITY,
                line: attr.span.line,
                message: format!(
                    "`@route` on `{fn_name}` takes a path or a method and a path \
                     (`@route(\"/path\")` or `@route(\"METHOD\", \"/path\")`), \
                     found {} arguments; handler not mounted",
                    literals.len()
                ),
            });
            None
        }
    }
}

fn handler_convention_route(fn_name: &str) -> Option<RouteSpec> {
    let path = match fn_name {
        "handler" => "/".to_string(),
        other => {
            let suffix = other.strip_prefix("handler_")?;
            if suffix.is_empty() {
                return None;
            }
            format!("/{suffix}")
        }
    };
    // Convention handlers answer both GET and POST so a script can serve
    // a read and a form-style write from one function without an explicit
    // attribute; the handler discriminates on `req.method`.
    Some(RouteSpec {
        method: "*".to_string(),
        path,
    })
}

fn normalize_route_method(method: &str) -> String {
    let upper = method.trim().to_ascii_uppercase();
    if upper == "ANY" || upper.is_empty() {
        "*".to_string()
    } else {
        upper
    }
}

fn normalize_route_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// Resolve the `@stream` marker on a declaration.
///
/// `@stream` is a bare attribute: it takes no arguments and only means
/// something on a declaration that resolved an HTTP route. A
/// well-formed marker turns the route into a streaming route — the site
/// adapter skips body buffering and VM dispatch and hands the request
/// head to the embedder's `SiteStreamProvider` after admission. A
/// malformed or unrouted `@stream` records a `HARN-SRV-*` diagnostic
/// and returns `false`, so the author sees the mistake instead of a
/// route that silently dispatches a stub handler (or a marker that
/// silently does nothing).
fn stream_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    route: Option<&RouteSpec>,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> bool {
    bare_route_marker_from_attributes(
        attrs,
        "stream",
        fn_name,
        route,
        diagnostics,
        STREAM_BAD_ARGS,
        STREAM_WITHOUT_ROUTE,
    )
}

/// Resolve the `@raw` marker on a declaration.
///
/// `@raw` mirrors `@stream` (a bare, route-only marker that turns the
/// route into a provider-answered route), except the request body *is*
/// buffered and handed to the provider as raw bytes. The two markers
/// contradict on body handling, so declaring both is diagnosed
/// (`HARN-SRV-013`) and `@raw` is dropped — the route behaves as
/// `@stream`.
fn raw_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    route: Option<&RouteSpec>,
    stream: bool,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> bool {
    let raw = bare_route_marker_from_attributes(
        attrs,
        "raw",
        fn_name,
        route,
        diagnostics,
        RAW_BAD_ARGS,
        RAW_WITHOUT_ROUTE,
    );
    if raw && stream {
        let line = attrs
            .iter()
            .find(|attr| attr.name == "raw")
            .map(|attr| attr.span.line)
            .unwrap_or(0);
        diagnostics.push(ExportDiagnostic {
            code: RAW_CONFLICTS_WITH_STREAM,
            line,
            message: format!(
                "`@raw` on `{fn_name}` conflicts with `@stream` (one never reads the request \
                 body, the other buffers it); dropping `@raw` — the route behaves as `@stream`"
            ),
        });
        return false;
    }
    raw
}

/// Resolve the `@ws` marker on a declaration.
///
/// `@ws` mirrors `@stream` (a bare, route-only marker that turns the
/// route into a provider-answered route), except the provider is handed
/// a WebSocket upgrade handle instead of producing a response body.
///
/// `@ws` *combines* with `@stream` on one route: the site adapter sniffs
/// the request's upgrade headers and routes a genuine WebSocket handshake
/// to the provider's `upgrade` entry point while every other request
/// falls through to the `open` (SSE/stream) entry point — one route, two
/// transports (the gateway `/acp` carve-out). Both flags are carried.
///
/// `@ws` still conflicts with `@raw`, though: a WebSocket handshake
/// carries no request body, while `@raw` exists precisely to buffer one,
/// so the pair contradicts. Declaring `@ws` alongside `@raw` is diagnosed
/// (`HARN-SRV-016`) and `@ws` is dropped — the route behaves as `@raw`.
fn ws_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    route: Option<&RouteSpec>,
    raw: bool,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> bool {
    let ws = bare_route_marker_from_attributes(
        attrs,
        "ws",
        fn_name,
        route,
        diagnostics,
        WS_BAD_ARGS,
        WS_WITHOUT_ROUTE,
    );
    if ws && raw {
        let line = attrs
            .iter()
            .find(|attr| attr.name == "ws")
            .map(|attr| attr.span.line)
            .unwrap_or(0);
        diagnostics.push(ExportDiagnostic {
            code: WS_CONFLICTS_WITH_STREAM_OR_RAW,
            line,
            message: format!(
                "`@ws` on `{fn_name}` conflicts with `@raw` (a WebSocket handshake carries no \
                 request body, but `@raw` buffers one); dropping `@ws` — the route behaves as \
                 `@raw`. (Pair `@ws` with `@stream` instead for a route that is both a WebSocket \
                 upgrade and an SSE/stream fallback.)"
            ),
        });
        return false;
    }
    ws
}

/// Shared resolution for the bare route markers (`@stream`, `@raw`):
/// present-and-well-formed on a routed declaration returns `true`;
/// arguments or a missing route record the given diagnostic codes and
/// return `false`, so the author sees the mistake instead of a route
/// that silently dispatches a stub handler (or a marker that silently
/// does nothing).
fn bare_route_marker_from_attributes(
    attrs: &[Attribute],
    marker: &str,
    fn_name: &str,
    route: Option<&RouteSpec>,
    diagnostics: &mut Vec<ExportDiagnostic>,
    bad_args_code: &'static str,
    without_route_code: &'static str,
) -> bool {
    let Some(attr) = attrs.iter().find(|attr| attr.name == marker) else {
        return false;
    };
    if route.is_none() {
        diagnostics.push(ExportDiagnostic {
            code: without_route_code,
            line: attr.span.line,
            message: format!(
                "`@{marker}` on `{fn_name}` has no effect without an HTTP route \
                 (`@route(...)` or the `handler_*` convention); ignoring it"
            ),
        });
        return false;
    }
    if !attr.args.is_empty() {
        diagnostics.push(ExportDiagnostic {
            code: bad_args_code,
            line: attr.span.line,
            message: format!(
                "`@{marker}` on `{fn_name}` takes no arguments, found {}; marker dropped — \
                 the route dispatches as a plain handler",
                attr.args.len()
            ),
        });
        return false;
    }
    true
}

/// Resolve the worker/job binding a `pub fn` declares with `@job(...)`.
///
/// Mirrors [`route_from_attributes`]: a present-but-malformed `@job`
/// records a `HARN-SRV-*` diagnostic and returns `None` so the author
/// sees the mistake instead of a silently mis-named or unregistered job.
///
/// Shape:
///
/// ```harn
/// @job("scan")
/// @retry(max: 3, backoff: "exponential")
/// @schedule("0 * * * *", "UTC")   // optional cron daemon job
/// @queue("scan-jobs")             // optional worker queue
/// pub fn scan(harness: Harness, event: TriggerEvent) -> dict { ... }
/// ```
///
/// The `@schedule` / `@queue` modifiers are parsed only when a `@job` is
/// present; written without one, they are dropped with a diagnostic (they
/// have no meaning off a job).
fn job_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<JobSpec> {
    let Some(job_attr) = attrs.iter().find(|attr| attr.name == "job") else {
        // The schedule/queue modifiers are inert without a `@job`.
        for modifier in ["schedule", "queue", "retry"] {
            if let Some(attr) = attrs.iter().find(|attr| attr.name == modifier) {
                diagnostics.push(ExportDiagnostic {
                    code: JOB_MODIFIER_WITHOUT_JOB,
                    line: attr.span.line,
                    message: format!(
                        "`@{modifier}` on `{fn_name}` has no effect without a `@job(\"name\")` \
                         attribute; ignoring it"
                    ),
                });
            }
        }
        return None;
    };

    // Split the `@job(...)` args into the optional positional name and
    // the named modifiers (`retry: {...}`). A non-string positional name
    // or more than one positional is ambiguous, so refuse to guess.
    let positionals: Vec<&AttributeArg> = job_attr
        .args
        .iter()
        .filter(|arg| arg.name.is_none())
        .collect();
    let name = match positionals.as_slice() {
        [] => fn_name.to_string(),
        [arg] => match &arg.value.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    fn_name.to_string()
                } else {
                    trimmed.to_string()
                }
            }
            _ => {
                diagnostics.push(ExportDiagnostic {
                    code: JOB_BAD_NAME,
                    line: job_attr.span.line,
                    message: format!(
                        "`@job` on `{fn_name}` takes an optional string-literal name \
                         (`@job` or `@job(\"name\")`); function not registered as a job"
                    ),
                });
                return None;
            }
        },
        _ => {
            diagnostics.push(ExportDiagnostic {
                code: JOB_BAD_NAME,
                line: job_attr.span.line,
                message: format!(
                    "`@job` on `{fn_name}` takes at most one string-literal name, found {}; \
                     function not registered as a job",
                    positionals.len()
                ),
            });
            return None;
        }
    };

    Some(JobSpec {
        name,
        schedule: schedule_from_attributes(attrs, fn_name, diagnostics),
        queue: queue_from_attributes(attrs, fn_name, diagnostics),
        retry: retry_from_attributes(attrs, job_attr, fn_name, diagnostics),
    })
}

fn schedule_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<ScheduleSpec> {
    let attr = attrs.iter().find(|attr| attr.name == "schedule")?;
    let literals: Vec<&str> = attr
        .args
        .iter()
        .filter_map(|arg| match &arg.value.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.as_str()),
            _ => None,
        })
        .collect();
    if literals.len() != attr.args.len() {
        diagnostics.push(ExportDiagnostic {
            code: SCHEDULE_BAD_ARGS,
            line: attr.span.line,
            message: format!(
                "`@schedule` on `{fn_name}` requires string-literal arguments \
                 (`@schedule(\"cron\")` or `@schedule(\"cron\", \"timezone\")`); schedule dropped"
            ),
        });
        return None;
    }
    match literals.as_slice() {
        [cron] => Some(ScheduleSpec {
            cron: cron.trim().to_string(),
            timezone: None,
        }),
        [cron, timezone] => Some(ScheduleSpec {
            cron: cron.trim().to_string(),
            timezone: Some(timezone.trim().to_string()),
        }),
        _ => {
            diagnostics.push(ExportDiagnostic {
                code: SCHEDULE_BAD_ARGS,
                line: attr.span.line,
                message: format!(
                    "`@schedule` on `{fn_name}` takes a cron expression and an optional timezone, \
                     found {} arguments; schedule dropped",
                    literals.len()
                ),
            });
            None
        }
    }
}

fn queue_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<String> {
    let attr = attrs.iter().find(|attr| attr.name == "queue")?;
    match attr.args.as_slice() {
        [arg] => match &arg.value.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value)
                if !value.trim().is_empty() =>
            {
                Some(value.trim().to_string())
            }
            _ => {
                diagnostics.push(ExportDiagnostic {
                    code: QUEUE_BAD_NAME,
                    line: attr.span.line,
                    message: format!(
                        "`@queue` on `{fn_name}` requires a non-empty string-literal queue name \
                         (`@queue(\"queue-name\")`); queue dropped"
                    ),
                });
                None
            }
        },
        _ => {
            diagnostics.push(ExportDiagnostic {
                code: QUEUE_BAD_NAME,
                line: attr.span.line,
                message: format!(
                    "`@queue` on `{fn_name}` takes exactly one string-literal queue name, found {}; \
                     queue dropped",
                    attr.args.len()
                ),
            });
            None
        }
    }
}

fn retry_from_attributes(
    attrs: &[Attribute],
    job_attr: &Attribute,
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RetrySpec> {
    if let Some(attr) = attrs.iter().find(|attr| attr.name == "retry") {
        return retry_from_attr(attr, fn_name, diagnostics);
    }
    retry_from_job_attr(job_attr, fn_name, diagnostics)
}

/// Parse the optional compact `retry: { max:, backoff: }` named argument
/// off `@job(...)`. Standalone `@retry(...)` is the preferred spelling,
/// but the dict form is useful when generated metadata already mirrors
/// the trigger DSL's shape.
fn retry_from_job_attr(
    job_attr: &Attribute,
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RetrySpec> {
    let retry_arg = job_attr
        .args
        .iter()
        .find(|arg| arg.name.as_deref() == Some("retry"))?;
    let Node::DictLiteral(entries) = &retry_arg.value.node else {
        diagnostics.push(ExportDiagnostic {
            code: RETRY_BAD_ARGS,
            line: retry_arg.span.line,
            message: format!(
                "`@job(retry:)` on `{fn_name}` requires a dict \
                 (`retry: {{ max: 3, backoff: \"exponential\" }}`); retry dropped"
            ),
        });
        return None;
    };

    Some(retry_from_entries(
        entries.iter().filter_map(|entry| {
            let key = match &entry.key.node {
                Node::Identifier(name) => name.as_str(),
                Node::StringLiteral(name) | Node::RawStringLiteral(name) => name.as_str(),
                _ => return None,
            };
            Some((key, &entry.value.node, retry_arg.span.line, "@job(retry:)"))
        }),
        fn_name,
        diagnostics,
    ))
}

fn retry_from_attr(
    attr: &Attribute,
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<RetrySpec> {
    let mut fields = Vec::new();
    for arg in &attr.args {
        let Some(name) = arg.name.as_deref() else {
            diagnostics.push(ExportDiagnostic {
                code: RETRY_BAD_ARGS,
                line: arg.span.line,
                message: format!(
                    "`@retry` on `{fn_name}` accepts named arguments \
                     (`@retry(max: 3, backoff: \"exponential\")`); ignoring a positional argument"
                ),
            });
            continue;
        };
        fields.push((name, &arg.value.node, arg.span.line, "@retry"));
    }
    Some(retry_from_entries(fields, fn_name, diagnostics))
}

fn retry_from_entries<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a Node, usize, &'static str)>,
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> RetrySpec {
    let mut max_attempts: u32 = 0;
    let mut backoff = RetryBackoff::default();
    for (key, value, line, context) in entries {
        match key {
            "max" | "max_attempts" => match value {
                Node::IntLiteral(value) if *value >= 0 => max_attempts = *value as u32,
                _ => diagnostics.push(ExportDiagnostic {
                    code: RETRY_BAD_ARGS,
                    line,
                    message: format!(
                        "`{context}` `max` on `{fn_name}` requires a non-negative integer; \
                         using the dispatcher default"
                    ),
                }),
            },
            "backoff" | "policy" => match value {
                Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
                    match value.trim().to_ascii_lowercase().as_str() {
                        "svix" | "" => backoff = RetryBackoff::Svix,
                        "linear" => backoff = RetryBackoff::Linear,
                        "exponential" => backoff = RetryBackoff::Exponential,
                        other => diagnostics.push(ExportDiagnostic {
                            code: RETRY_BAD_ARGS,
                            line,
                            message: format!(
                                "`{context}` `backoff` on `{fn_name}` got unknown strategy \
                                 '{other}' (expected 'svix', 'linear', or 'exponential'); using 'svix'"
                            ),
                        }),
                    }
                }
                _ => diagnostics.push(ExportDiagnostic {
                    code: RETRY_BAD_ARGS,
                    line,
                    message: format!(
                        "`{context}` `backoff` on `{fn_name}` requires a string-literal \
                         strategy; using 'svix'"
                    ),
                }),
            },
            _ => diagnostics.push(ExportDiagnostic {
                code: RETRY_BAD_ARGS,
                line,
                message: format!(
                    "`{context}` on `{fn_name}` got unknown field `{key}` \
                     (expected `max`, `max_attempts`, `backoff`, or `policy`); field ignored"
                ),
            }),
        }
    }
    RetrySpec {
        max_attempts,
        backoff,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "exports/typed_pipeline_tests.rs"]
mod typed_pipeline_tests;
