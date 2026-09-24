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
fn export_catalog_splits_method_prefixed_scopes_from_the_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.harn");
    std::fs::write(
        &path,
        r#"
@scopes("base:read", "GET extra:get", "put extra:put")
@route("*", "/r")
pub fn r() -> string { return "ok" }
"#,
    )
    .expect("write script");

    let catalog = ExportCatalog::from_path(&path).expect("catalog");
    let r = catalog.function("r").expect("r export");
    // An un-prefixed literal stays in the method-agnostic baseline.
    assert_eq!(r.required_scopes, BTreeSet::from(["base:read".to_string()]));
    // A method prefix (case-insensitive) routes the scope into the
    // per-method bucket under the uppercased method key.
    assert_eq!(
        r.method_scopes.get("GET"),
        Some(&BTreeSet::from(["extra:get".to_string()]))
    );
    assert_eq!(
        r.method_scopes.get("PUT"),
        Some(&BTreeSet::from(["extra:put".to_string()]))
    );
}

#[test]
fn export_catalog_keeps_colon_scopes_uniform_without_a_method_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.harn");
    // A leading word that is *not* an HTTP method (here a normal
    // `scope:verb` token) is never misread as a per-method prefix —
    // the historic uniform form is untouched.
    std::fs::write(
        &path,
        r#"
@scopes("personas:read")
pub fn r() -> string { return "ok" }
"#,
    )
    .expect("write script");

    let catalog = ExportCatalog::from_path(&path).expect("catalog");
    let r = catalog.function("r").expect("r export");
    assert_eq!(
        r.required_scopes,
        BTreeSet::from(["personas:read".to_string()])
    );
    assert!(r.method_scopes.is_empty());
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
pipeline default(harness: Harness, task: unknown) {
  harness.stdio.println(task)
}
",
    )
    .expect("write script");

    let catalog = ExportCatalog::from_path(&path).expect("catalog");
    let default = catalog.function("default").expect("default pipeline");
    assert_eq!(default.kind, ExportedCallableKind::Pipeline);
    assert_eq!(default.params.len(), 1);
    assert_eq!(default.params[0].name, "task");
    assert!(default.input_schema["properties"].get("harness").is_none());
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
fn policy_attribute_parses_allowed_kinds() {
    let catalog = catalog_from_source(
        r#"
@scopes("admin:dlq:write")
@policy(kinds: "operator platform_admin", matches: "tenant owner", methods: "doc.read doc.write")
@route("POST", "/admin/dlq/replay")
pub fn replay_dlq(req: dict) -> dict { return req }
"#,
    );
    assert!(
        catalog.diagnostics().is_empty(),
        "unexpected diagnostics: {:?}",
        catalog.diagnostics()
    );
    let policy = catalog
        .function("replay_dlq")
        .expect("replay_dlq")
        .policy
        .as_ref()
        .expect("policy present");
    assert_eq!(
        policy.allowed_kinds,
        BTreeSet::from(["operator".to_string(), "platform_admin".to_string()])
    );
    assert_eq!(
        policy.match_labels,
        BTreeSet::from(["owner".to_string(), "tenant".to_string()])
    );
    assert_eq!(
        policy.method_guards,
        BTreeSet::from(["doc.read".to_string(), "doc.write".to_string()])
    );
}

#[test]
fn policy_without_attribute_leaves_policy_none() {
    let catalog = catalog_from_source(
        r#"
@route("GET", "/open")
pub fn open_route(req: dict) -> dict { return req }
"#,
    );
    assert!(catalog
        .function("open_route")
        .expect("open_route")
        .policy
        .is_none());
}

#[test]
fn policy_with_unknown_arg_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@policy(roles: "operator")
@route("POST", "/x")
pub fn guarded(req: dict) -> dict { return req }
"#,
    );
    // The unrecognized `roles:` key is dropped, leaving no effective
    // policy — and a loud diagnostic is emitted.
    assert!(catalog
        .function("guarded")
        .expect("guarded")
        .policy
        .is_none());
    let diagnostic = catalog
        .diagnostics()
        .iter()
        .find(|d| d.code == POLICY_BAD_ARGS)
        .expect("policy diagnostic");
    assert!(diagnostic.message.contains("guarded"));
}

#[test]
fn job_attribute_parses_name_schedule_queue_and_retry() {
    let catalog = catalog_from_source(
        r#"
@job("scan", retry: { max: 3, backoff: "exponential" })
@schedule("0 * * * *", "UTC")
@queue("scan-jobs")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }

@job
pub fn sweep(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }

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
pub fn scan(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }
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
pub fn orphan(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }
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
pub fn scan(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }
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

#[test]
fn standalone_retry_unknown_key_is_diagnosed() {
    let catalog = catalog_from_source(
        r#"
@job("scan")
@retry(max: 5, patience: "high")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict { return {ok: true} }
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
    assert_eq!(retry.max_attempts, 5);
    assert_eq!(retry.backoff, RetryBackoff::Svix);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![RETRY_BAD_ARGS]);
}

#[test]
fn stream_attribute_marks_routed_functions_only() {
    let catalog = catalog_from_source(
        r#"
@stream
@route("GET", "/events")
pub fn events(req: dict) -> dict { return http_ok({}) }

@stream
pub fn handler_feed(req: dict) -> dict { return http_ok({}) }

@route("GET", "/plain")
pub fn plain(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(
        catalog.diagnostics().is_empty(),
        "unexpected diagnostics: {:?}",
        catalog.diagnostics()
    );
    // Works with an explicit @route and with the handler_* convention.
    assert!(catalog.function("events").expect("events").stream);
    assert!(catalog.function("handler_feed").expect("feed").stream);
    // A routed fn without the marker is a plain dispatch route.
    assert!(!catalog.function("plain").expect("plain").stream);
}

#[test]
fn stream_with_args_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@stream("sse")
@route("GET", "/events")
pub fn events(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(!catalog.function("events").expect("events").stream);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![STREAM_BAD_ARGS]);
}

#[test]
fn stream_without_route_is_diagnosed_and_ignored() {
    let catalog = catalog_from_source(
        r"
@stream
pub fn helper(req: dict) -> dict { return req }
",
    );
    assert!(!catalog.function("helper").expect("helper").stream);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![STREAM_WITHOUT_ROUTE]);
}

#[test]
fn raw_attribute_marks_routed_functions_only() {
    let catalog = catalog_from_source(
        r#"
@raw
@route("POST", "/packs/publish")
pub fn publish(req: dict) -> dict { return http_ok({}) }

@raw
pub fn handler_upload(req: dict) -> dict { return http_ok({}) }

@route("GET", "/plain")
pub fn plain(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(
        catalog.diagnostics().is_empty(),
        "unexpected diagnostics: {:?}",
        catalog.diagnostics()
    );
    // Works with an explicit @route and with the handler_* convention.
    assert!(catalog.function("publish").expect("publish").raw);
    assert!(catalog.function("handler_upload").expect("upload").raw);
    // A routed fn without the marker is a plain dispatch route.
    assert!(!catalog.function("plain").expect("plain").raw);
    // `@raw` never implies `@stream`.
    assert!(!catalog.function("publish").expect("publish").stream);
}

#[test]
fn raw_with_args_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@raw("bytes")
@route("POST", "/upload")
pub fn upload(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(!catalog.function("upload").expect("upload").raw);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![RAW_BAD_ARGS]);
}

#[test]
fn raw_without_route_is_diagnosed_and_ignored() {
    let catalog = catalog_from_source(
        r"
@raw
pub fn helper(req: dict) -> dict { return req }
",
    );
    assert!(!catalog.function("helper").expect("helper").raw);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![RAW_WITHOUT_ROUTE]);
}

#[test]
fn raw_conflicting_with_stream_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@stream
@raw
@route("GET", "/both")
pub fn both(req: dict) -> dict { return http_ok({}) }
"#,
    );
    // `@stream` wins; `@raw` is dropped with a diagnostic.
    let function = catalog.function("both").expect("both");
    assert!(function.stream);
    assert!(!function.raw);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![RAW_CONFLICTS_WITH_STREAM]);
}

#[test]
fn ws_attribute_marks_routed_functions_only() {
    let catalog = catalog_from_source(
        r#"
@ws
@route("GET", "/socket")
pub fn socket(req: dict) -> dict { return http_ok({}) }

@ws
pub fn handler_live(req: dict) -> dict { return http_ok({}) }

@route("GET", "/plain")
pub fn plain(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(
        catalog.diagnostics().is_empty(),
        "unexpected diagnostics: {:?}",
        catalog.diagnostics()
    );
    // Works with an explicit @route and with the handler_* convention.
    assert!(catalog.function("socket").expect("socket").ws);
    assert!(catalog.function("handler_live").expect("live").ws);
    // A routed fn without the marker is a plain dispatch route.
    assert!(!catalog.function("plain").expect("plain").ws);
    // `@ws` never implies `@stream` / `@raw`.
    assert!(!catalog.function("socket").expect("socket").stream);
    assert!(!catalog.function("socket").expect("socket").raw);
}

#[test]
fn ws_with_args_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@ws("chat")
@route("GET", "/socket")
pub fn socket(req: dict) -> dict { return http_ok({}) }
"#,
    );
    assert!(!catalog.function("socket").expect("socket").ws);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![WS_BAD_ARGS]);
}

#[test]
fn ws_without_route_is_diagnosed_and_ignored() {
    let catalog = catalog_from_source(
        r"
@ws
pub fn helper(req: dict) -> dict { return req }
",
    );
    assert!(!catalog.function("helper").expect("helper").ws);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![WS_WITHOUT_ROUTE]);
}

#[test]
fn ws_combined_with_stream_carries_both_flags_without_diagnostic() {
    // `@ws` + `@stream` is the *combined* route (one route that both
    // upgrades a genuine WebSocket handshake and falls through to the
    // SSE/stream path otherwise): both flags survive, no diagnostic.
    let catalog = catalog_from_source(
        r#"
@stream
@ws
@route("GET", "/both")
pub fn both(req: dict) -> dict { return http_ok({}) }
"#,
    );
    let function = catalog.function("both").expect("both");
    assert!(function.stream);
    assert!(function.ws);
    assert!(!function.raw);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert!(
        codes.is_empty(),
        "combined @ws @stream must not be diagnosed, got {codes:?}"
    );
}

#[test]
fn ws_conflicting_with_raw_is_diagnosed_and_dropped() {
    let catalog = catalog_from_source(
        r#"
@raw
@ws
@route("POST", "/both")
pub fn both(req: dict) -> dict { return http_ok({}) }
"#,
    );
    // `@raw` wins; `@ws` is dropped with a diagnostic.
    let function = catalog.function("both").expect("both");
    assert!(function.raw);
    assert!(!function.ws);
    let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![WS_CONFLICTS_WITH_STREAM_OR_RAW]);
}
