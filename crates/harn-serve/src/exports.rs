use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{Attribute, AttributeArg, Node, TypeExpr};

use crate::limits::{limits_and_budget_from_attributes, BudgetSpec, RouteLimits};
use crate::DispatchError;

#[derive(Clone, Debug, PartialEq)]
pub struct ExportedParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub has_default: bool,
    pub rest: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportedCallableKind {
    Function,
    Pipeline,
}

#[derive(Clone, Debug)]
pub struct ExportedFunction {
    pub name: String,
    pub kind: ExportedCallableKind,
    pub params: Vec<ExportedParam>,
    pub return_type: Option<TypeExpr>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    /// Scopes the caller's credential must carry to invoke this function.
    /// Populated from `@scopes("...", "...")` attribute literals on the
    /// declaration; empty when no attribute is present, meaning the route
    /// is unrestricted beyond whatever scopes the auth method enforces
    /// globally.
    pub required_scopes: BTreeSet<String>,
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
}

/// A `.harn` worker/job entrypoint declared with `@job("name")`.
///
/// A job is *not* a separate execution engine: the worker adapter lowers
/// it into a `TriggerBindingSpec` whose handler is the function's own
/// closure and dispatches it through `harn_vm`'s trigger
/// [`Dispatcher`](harn_vm::Dispatcher). Retry, dead-letter, per-dispatch
/// budget, and cancellation are therefore inherited from the dispatcher
/// rather than re-implemented here.
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
/// pub fn scan(event: TriggerEvent) -> dict { ... }
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
/// non-integer `max` or an unknown `backoff` keyword. The offending
/// field is dropped (the rest of the policy still applies).
pub const RETRY_BAD_ARGS: &str = "HARN-SRV-007";
/// `@schedule` / `@queue` / `@retry` appears without a `@job` attribute.
/// Those modifiers only mean something on a job, so they are ignored.
pub const JOB_MODIFIER_WITHOUT_JOB: &str = "HARN-SRV-008";

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
/// [`ExportCatalog::diagnostics`] and render them in their own UI.
pub fn emit_export_diagnostics(diagnostics: &[ExportDiagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("[harn] warning: {diagnostic}");
    }
}

#[derive(Clone, Debug)]
pub struct ExportCatalog {
    pub script_path: PathBuf,
    pub functions: BTreeMap<String, ExportedFunction>,
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

        let mut functions = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for node in &program {
            let (attrs, inner) = harn_parser::peel_attributes(node);
            let Node::FnDecl {
                name,
                params,
                return_type,
                is_pub,
                ..
            } = &inner.node
            else {
                continue;
            };
            if !*is_pub {
                continue;
            }

            let (limits, budget) = limits_and_budget_from_attributes(attrs);
            functions.insert(
                name.clone(),
                ExportedFunction {
                    name: name.clone(),
                    kind: ExportedCallableKind::Function,
                    params: exported_params(params),
                    return_type: return_type.clone(),
                    input_schema: harn_vm::json_schema_for_typed_params(params),
                    output_schema: return_type
                        .as_ref()
                        .and_then(harn_vm::json_schema_for_type_expr),
                    required_scopes: scopes_from_attributes(attrs, name, &mut diagnostics),
                    limits,
                    budget,
                    route: route_from_attributes(attrs, name, &mut diagnostics),
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
                is_pub,
                ..
            } = &inner.node
            else {
                continue;
            };
            if has_public_exports && !*is_pub {
                continue;
            }
            let required_scopes = scopes_from_attributes(attrs, name, &mut diagnostics);
            let (limits, budget) = limits_and_budget_from_attributes(attrs);
            functions
                .entry(name.clone())
                .or_insert_with(|| ExportedFunction {
                    name: name.clone(),
                    kind: ExportedCallableKind::Pipeline,
                    params: pipeline_exported_params(params),
                    return_type: return_type.clone(),
                    input_schema: pipeline_input_schema(params),
                    output_schema: return_type
                        .as_ref()
                        .and_then(harn_vm::json_schema_for_type_expr),
                    required_scopes,
                    limits,
                    budget,
                    // Pipelines are dispatch-only; they never carry an
                    // HTTP route. Only `pub fn` handlers participate in
                    // `harn serve site`.
                    route: None,
                    job: job_from_attributes(attrs, name, &mut diagnostics),
                });
        }

        Ok(Self {
            script_path: path.to_path_buf(),
            functions,
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

fn exported_params(params: &[harn_parser::TypedParam]) -> Vec<ExportedParam> {
    params
        .iter()
        .map(|param| ExportedParam {
            name: param.name.clone(),
            type_expr: param.type_expr.clone(),
            input_schema: param
                .type_expr
                .as_ref()
                .and_then(harn_vm::json_schema_for_type_expr)
                .unwrap_or_else(|| serde_json::json!({})),
            has_default: param.default_value.is_some(),
            rest: param.rest,
        })
        .collect()
}

fn pipeline_exported_params(params: &[String]) -> Vec<ExportedParam> {
    params
        .iter()
        .map(|name| ExportedParam {
            name: name.clone(),
            type_expr: None,
            input_schema: serde_json::json!({}),
            has_default: false,
            rest: false,
        })
        .collect()
}

/// Collect scope literals from any `@scopes(...)` attributes on a
/// declaration. Both positional and named arguments are accepted (named
/// args are useful for ergonomics like `@scopes(read: "personas:read")`
/// in callers that prefer key-value form); only string literals
/// contribute. Multiple `@scopes` attributes on the same declaration
/// union into one set.
fn scopes_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for attr in attrs {
        if attr.name != "scopes" {
            continue;
        }
        for arg in &attr.args {
            match &arg.value.node {
                Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
                    set.insert(value.clone());
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
    set
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

/// Resolve the worker/job binding a `pub fn` declares with `@job(...)`.
///
/// Mirrors [`route_from_attributes`]: a present-but-malformed `@job`
/// records a `HARN-SRV-*` diagnostic and returns `None` so the author
/// sees the mistake instead of a silently mis-named or unregistered job.
///
/// Shape (`retry:` rides inside `@job` because `retry` is a reserved
/// keyword and so cannot be its own `@retry` attribute name — the same
/// reason the trigger DSL nests `retry: {...}` inside `trigger_register`):
///
/// ```harn
/// @job("scan", retry: { max: 3, backoff: "exponential" })
/// @schedule("0 * * * *", "UTC")   // optional cron daemon job
/// @queue("scan-jobs")             // optional worker queue
/// pub fn scan(event: TriggerEvent) -> dict { ... }
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
        for modifier in ["schedule", "queue"] {
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
        retry: retry_from_job_attr(job_attr, fn_name, diagnostics),
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

/// Parse the optional `retry: { max:, backoff: }` named argument off the
/// `@job(...)` attribute. Mirrors the trigger DSL's `retry` dict so a job
/// author who knows `trigger_register` reuses the same shape.
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

    let mut max_attempts: u32 = 0;
    let mut backoff = RetryBackoff::default();
    for entry in entries {
        let key = match &entry.key.node {
            Node::Identifier(name) => name.clone(),
            Node::StringLiteral(name) | Node::RawStringLiteral(name) => name.clone(),
            _ => continue,
        };
        match key.as_str() {
            "max" | "max_attempts" => match &entry.value.node {
                Node::IntLiteral(value) if *value >= 0 => max_attempts = *value as u32,
                _ => diagnostics.push(ExportDiagnostic {
                    code: RETRY_BAD_ARGS,
                    line: retry_arg.span.line,
                    message: format!(
                        "`@job(retry:)` `max` on `{fn_name}` requires a non-negative integer; \
                         using the dispatcher default"
                    ),
                }),
            },
            "backoff" | "policy" => match &entry.value.node {
                Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
                    match value.trim().to_ascii_lowercase().as_str() {
                        "svix" | "" => backoff = RetryBackoff::Svix,
                        "linear" => backoff = RetryBackoff::Linear,
                        "exponential" | "exp" => backoff = RetryBackoff::Exponential,
                        other => diagnostics.push(ExportDiagnostic {
                            code: RETRY_BAD_ARGS,
                            line: retry_arg.span.line,
                            message: format!(
                                "`@job(retry:)` `backoff` on `{fn_name}` got unknown strategy \
                                 '{other}' (expected 'svix', 'linear', or 'exponential'); using 'svix'"
                            ),
                        }),
                    }
                }
                _ => diagnostics.push(ExportDiagnostic {
                    code: RETRY_BAD_ARGS,
                    line: retry_arg.span.line,
                    message: format!(
                        "`@job(retry:)` `backoff` on `{fn_name}` requires a string-literal \
                         strategy; using 'svix'"
                    ),
                }),
            },
            _ => continue,
        }
    }
    Some(RetrySpec {
        max_attempts,
        backoff,
    })
}

fn pipeline_input_schema(params: &[String]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": params
            .iter()
            .map(|name| (name.clone(), serde_json::json!({})))
            .collect::<serde_json::Map<_, _>>(),
        "required": params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_catalog_only_includes_public_functions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r#"
fn hidden() { return "nope" }
pub fn greet(name: string, excited: bool = false) -> string {
  if excited { return "hi!" }
  return name
}
"#,
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        assert!(catalog.function("hidden").is_none());
        let greet = catalog.function("greet").expect("greet export");
        assert_eq!(greet.params.len(), 2);
        assert_eq!(greet.input_schema["type"], "object");
        assert_eq!(
            greet.output_schema.as_ref().expect("output")["type"],
            "string"
        );
    }

    #[test]
    fn export_catalog_captures_scopes_attribute_from_function_decl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r#"
@scopes("personas:read", "sessions:write")
pub fn list_sessions() -> string {
  return "ok"
}

pub fn ping() -> string {
  return "pong"
}
"#,
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        let list = catalog.function("list_sessions").expect("list_sessions");
        assert_eq!(
            list.required_scopes,
            BTreeSet::from(["personas:read".to_string(), "sessions:write".to_string()])
        );
        let ping = catalog.function("ping").expect("ping");
        assert!(ping.required_scopes.is_empty());
    }

    #[test]
    fn export_catalog_parses_limits_and_budget_attributes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r#"
@limits(
    per_tenant: "100/min",
    per_route: "5000/min",
    burst: 50,
    algorithm: "sliding_window",
    in_flight_max: 20,
)
@budget(llm_cost_usd: 0.50, mcp_calls: 20)
pub fn create() -> string { return "ok" }

pub fn ping() -> string { return "pong" }
"#,
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        let create = catalog.function("create").expect("create export");
        let limits = create.limits.as_ref().expect("limits parsed");
        assert_eq!(limits.per_tenant.unwrap().count, 100);
        assert_eq!(limits.per_route.unwrap().count, 5_000);
        assert_eq!(limits.burst, Some(50));
        assert_eq!(limits.algorithm, crate::limits::Algorithm::SlidingWindow);
        assert_eq!(limits.in_flight_max, Some(20));
        let budget = create.budget.as_ref().expect("budget parsed");
        assert_eq!(budget.llm_cost_usd, Some(0.50));
        assert_eq!(budget.mcp_calls, Some(20));

        // Routes without the attributes get None — the dispatch path
        // short-circuits without consulting the registry.
        let ping = catalog.function("ping").expect("ping export");
        assert!(ping.limits.is_none());
        assert!(ping.budget.is_none());
    }

    #[test]
    fn route_attribute_parses_method_and_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r#"
@route("POST", "/users/{id}")
pub fn update_user(req: dict) -> dict { return req }

@route("/health")
pub fn liveness(req: dict) -> dict { return req }

@route("any", "metrics")
pub fn metrics(req: dict) -> dict { return req }

pub fn helper(req: dict) -> dict { return req }
"#,
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        let update = catalog.function("update_user").expect("update_user");
        assert_eq!(
            update.route,
            Some(RouteSpec {
                method: "POST".to_string(),
                path: "/users/{id}".to_string()
            })
        );
        // Single-arg form defaults to GET.
        let liveness = catalog.function("liveness").expect("liveness");
        assert_eq!(
            liveness.route,
            Some(RouteSpec {
                method: "GET".to_string(),
                path: "/health".to_string()
            })
        );
        // `any` lowercases to the `*` wildcard; a path missing its leading
        // slash is normalized.
        let metrics = catalog.function("metrics").expect("metrics");
        assert_eq!(
            metrics.route,
            Some(RouteSpec {
                method: "*".to_string(),
                path: "/metrics".to_string()
            })
        );
        // A plain `pub fn` with no attribute and no `handler_` prefix is
        // dispatch-only — it gets no HTTP route.
        let helper = catalog.function("helper").expect("helper");
        assert_eq!(helper.route, None);
    }

    #[test]
    fn handler_naming_convention_infers_route() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r"
pub fn handler(req: dict) -> dict { return req }
pub fn handler_echo(req: dict) -> dict { return req }
",
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        // Bare `handler` mounts at the site root.
        assert_eq!(
            catalog.function("handler").expect("handler").route,
            Some(RouteSpec {
                method: "*".to_string(),
                path: "/".to_string()
            })
        );
        // `handler_echo` mounts at `/echo`, answering every method.
        assert_eq!(
            catalog
                .function("handler_echo")
                .expect("handler_echo")
                .route,
            Some(RouteSpec {
                method: "*".to_string(),
                path: "/echo".to_string()
            })
        );
    }

    #[test]
    fn export_catalog_falls_back_to_legacy_pipelines_without_public_exports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(
            &path,
            r"
pipeline default(task) {
  __io_println(task)
}
",
        )
        .expect("write script");

        let catalog = ExportCatalog::from_path(&path).expect("catalog");
        let default = catalog.function("default").expect("default pipeline");
        assert_eq!(default.kind, ExportedCallableKind::Pipeline);
        assert_eq!(default.params[0].name, "task");
    }

    /// Build a catalog from inline source, asserting it parses cleanly.
    fn catalog_from_source(source: &str) -> ExportCatalog {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(&path, source).expect("write script");
        ExportCatalog::from_path(&path).expect("catalog")
    }

    #[test]
    fn well_formed_attributes_emit_no_diagnostics() {
        let catalog = catalog_from_source(
            r#"
@scopes("personas:read")
@route("POST", "/users/{id}")
pub fn update_user(req: dict) -> dict { return req }

@route("/health")
pub fn liveness(req: dict) -> dict { return req }
"#,
        );
        assert!(
            catalog.diagnostics().is_empty(),
            "unexpected diagnostics: {:?}",
            catalog.diagnostics()
        );
    }

    #[test]
    fn route_with_non_string_arg_is_diagnosed_and_unmounted() {
        // The second arg is an identifier, not a string literal. Left
        // unchecked the collector would treat this as `@route("GET")` and
        // mis-mount the handler at `/GET`.
        let catalog = catalog_from_source(
            r#"
pub fn make_path(req: dict) -> string { return "/x" }

@route("GET", make_path)
pub fn handler_users(req: dict) -> dict { return req }
"#,
        );
        let handler = catalog.function("handler_users").expect("handler_users");
        assert_eq!(
            handler.route, None,
            "a malformed @route must not fall back to the handler_ convention route"
        );
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![ROUTE_ARG_NOT_STRING]);
    }

    #[test]
    fn route_with_zero_args_is_diagnosed_and_unmounted() {
        let catalog = catalog_from_source(
            r"
@route()
pub fn handler_status(req: dict) -> dict { return req }
",
        );
        let handler = catalog.function("handler_status").expect("handler_status");
        assert_eq!(handler.route, None);
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![ROUTE_BAD_ARITY]);
    }

    #[test]
    fn route_with_too_many_args_is_diagnosed_and_unmounted() {
        let catalog = catalog_from_source(
            r#"
@route("GET", "/x", "/y")
pub fn handler_overspecified(req: dict) -> dict { return req }
"#,
        );
        let handler = catalog
            .function("handler_overspecified")
            .expect("handler_overspecified");
        assert_eq!(handler.route, None);
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![ROUTE_BAD_ARITY]);
    }

    #[test]
    fn scopes_with_non_string_arg_is_diagnosed_but_keeps_valid_scopes() {
        let catalog = catalog_from_source(
            r#"
pub fn make_scope(req: dict) -> string { return "sessions:write" }

@scopes("personas:read", make_scope)
pub fn list_sessions() -> string { return "ok" }
"#,
        );
        let list = catalog.function("list_sessions").expect("list_sessions");
        // The valid literal is still enforced; only the bad arg is dropped.
        assert_eq!(
            list.required_scopes,
            BTreeSet::from(["personas:read".to_string()])
        );
        let diagnostic = catalog
            .diagnostics()
            .iter()
            .find(|d| d.code == SCOPES_ARG_NOT_STRING)
            .expect("scopes diagnostic");
        assert!(diagnostic.message.contains("list_sessions"));
    }

    #[test]
    fn job_attribute_parses_name_schedule_queue_and_retry() {
        let catalog = catalog_from_source(
            r#"
@job("scan", retry: { max: 3, backoff: "exponential" })
@schedule("0 * * * *", "UTC")
@queue("scan-jobs")
pub fn scan(event: TriggerEvent) -> dict { return {ok: true} }

@job
pub fn sweep(event: TriggerEvent) -> dict { return {ok: true} }

pub fn helper(req: dict) -> dict { return req }
"#,
        );
        assert!(
            catalog.diagnostics().is_empty(),
            "unexpected diagnostics: {:?}",
            catalog.diagnostics()
        );

        let scan = catalog.function("scan").expect("scan export");
        let job = scan.job.as_ref().expect("scan is a job");
        assert_eq!(job.name, "scan");
        assert_eq!(
            job.schedule,
            Some(ScheduleSpec {
                cron: "0 * * * *".to_string(),
                timezone: Some("UTC".to_string()),
            })
        );
        assert_eq!(job.queue.as_deref(), Some("scan-jobs"));
        assert_eq!(
            job.retry,
            Some(RetrySpec {
                max_attempts: 3,
                backoff: RetryBackoff::Exponential,
            })
        );

        // Bare `@job` defaults the job name to the function name and
        // carries no schedule/queue/retry.
        let sweep = catalog.function("sweep").expect("sweep export");
        let sweep_job = sweep.job.as_ref().expect("sweep is a job");
        assert_eq!(sweep_job.name, "sweep");
        assert!(sweep_job.schedule.is_none());
        assert!(sweep_job.queue.is_none());
        assert!(sweep_job.retry.is_none());

        // A plain `pub fn` is not a job.
        let helper = catalog.function("helper").expect("helper export");
        assert!(helper.job.is_none());
    }

    #[test]
    fn job_with_non_string_name_is_diagnosed_and_unregistered() {
        let catalog = catalog_from_source(
            r#"
pub fn name_of(event: TriggerEvent) -> string { return "x" }

@job(name_of)
pub fn scan(event: TriggerEvent) -> dict { return {ok: true} }
"#,
        );
        let scan = catalog.function("scan").expect("scan export");
        assert!(scan.job.is_none());
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![JOB_BAD_NAME]);
    }

    #[test]
    fn schedule_modifier_without_job_is_diagnosed() {
        let catalog = catalog_from_source(
            r#"
@schedule("0 * * * *")
pub fn orphan(event: TriggerEvent) -> dict { return {ok: true} }
"#,
        );
        let orphan = catalog.function("orphan").expect("orphan export");
        assert!(orphan.job.is_none());
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![JOB_MODIFIER_WITHOUT_JOB]);
    }

    #[test]
    fn retry_with_unknown_backoff_keeps_max_and_diagnoses() {
        let catalog = catalog_from_source(
            r#"
@job("scan", retry: { max: 5, backoff: "wishful" })
pub fn scan(event: TriggerEvent) -> dict { return {ok: true} }
"#,
        );
        let scan = catalog.function("scan").expect("scan export");
        let retry = scan
            .job
            .as_ref()
            .expect("job")
            .retry
            .as_ref()
            .expect("retry");
        // The valid `max` survives; the bad backoff falls back to svix.
        assert_eq!(retry.max_attempts, 5);
        assert_eq!(retry.backoff, RetryBackoff::Svix);
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![RETRY_BAD_ARGS]);
    }
}
