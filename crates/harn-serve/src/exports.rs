use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{Attribute, Node, TypeExpr};

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
                    required_scopes: scopes_from_attributes(attrs),
                    limits,
                    budget,
                    route: route_from_attributes(attrs, name),
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
            let required_scopes = scopes_from_attributes(attrs);
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
                });
        }

        Ok(Self {
            script_path: path.to_path_buf(),
            functions,
        })
    }

    pub fn function(&self, name: &str) -> Option<&ExportedFunction> {
        self.functions.get(name)
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
fn scopes_from_attributes(attrs: &[Attribute]) -> BTreeSet<String> {
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
                _ => {}
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
/// Returns `None` for any other `pub fn`, so a script can export helper
/// functions (reachable via the API/A2A/MCP dispatch adapters) without
/// every one of them grabbing an HTTP path.
fn route_from_attributes(attrs: &[Attribute], fn_name: &str) -> Option<RouteSpec> {
    if let Some(route) = explicit_route_attribute(attrs) {
        return Some(route);
    }
    handler_convention_route(fn_name)
}

fn explicit_route_attribute(attrs: &[Attribute]) -> Option<RouteSpec> {
    let attr = attrs.iter().find(|attr| attr.name == "route")?;
    let literals: Vec<String> = attr
        .args
        .iter()
        .filter_map(|arg| match &arg.value.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.clone()),
            _ => None,
        })
        .collect();
    match literals.as_slice() {
        // `@route("/path")` — method defaults to GET.
        [path] => Some(RouteSpec {
            method: "GET".to_string(),
            path: normalize_route_path(path),
        }),
        // `@route("METHOD", "/path")` — explicit method.
        [method, path, ..] => Some(RouteSpec {
            method: normalize_route_method(method),
            path: normalize_route_path(path),
        }),
        [] => None,
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
}
