use super::*;
use crate::package::test_support::*;

#[test]
fn load_runtime_extensions_uses_only_root_llm_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[llm.aliases]
project-fast = { id = "project/model", provider = "project" }

[llm.providers.project]
base_url = "https://project.test/v1"
chat_endpoint = "/chat/completions"
"#,
    )
    .unwrap();
    fs::write(
        root.join(".harn/packages/acme/harn.toml"),
        r#"
[llm.aliases]
acme-fast = { id = "acme/model", provider = "acme" }

[llm.providers.acme]
base_url = "https://acme.test/v1"
chat_endpoint = "/chat/completions"
"#,
    )
    .unwrap();
    let harn_file = root.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();

    let extensions = load_runtime_extensions(&harn_file);
    let llm = extensions.llm.expect("merged llm config");
    assert!(llm.providers.contains_key("project"));
    assert!(llm.aliases.contains_key("project-fast"));
    assert!(!llm.providers.contains_key("acme"));
    assert!(!llm.aliases.contains_key("acme-fast"));
}

#[test]
fn load_runtime_extensions_ignores_package_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"

[[hooks]]
event = "PostToolUse"
pattern = "tool.name =~ \"read\""
handler = "workspace::after_read"
"#,
    )
    .unwrap();
    fs::write(
        root.join(".harn/packages/acme/harn.toml"),
        r#"
[package]
name = "acme"

[[hooks]]
event = "PreToolUse"
pattern = "tool.name =~ \"edit|write\""
handler = "acme::audit_edit"
"#,
    )
    .unwrap();
    let harn_file = root.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();

    let extensions = load_runtime_extensions(&harn_file);
    assert_eq!(extensions.hooks.len(), 1);
    assert_eq!(extensions.hooks[0].handler, "workspace::after_read");
}

#[test]
fn load_runtime_extensions_collects_manifest_provider_connectors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }
oauth = { resource = "https://echo.example/mcp", authorization_endpoint = "https://auth.echo.example/authorize", token_endpoint = "https://auth.echo.example/token", scopes = "echo.read" }

[[providers]]
id = "github"
connector = { rust = "builtin" }
"#,
    )
    .unwrap();
    let harn_file = root.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();

    let extensions = load_runtime_extensions(&harn_file);
    assert_eq!(extensions.provider_connectors.len(), 2);
    assert!(matches!(
        &extensions.provider_connectors[0].connector,
        ResolvedProviderConnectorKind::Harn { module } if module == "./echo_connector.harn"
    ));
    assert_eq!(
        extensions.provider_connectors[0]
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.resource.as_deref()),
        Some("https://echo.example/mcp")
    );
    assert_eq!(
        extensions.provider_connectors[0]
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.scopes.as_deref()),
        Some("echo.read")
    );
    assert!(matches!(
        extensions.provider_connectors[1].connector,
        ResolvedProviderConnectorKind::RustBuiltin
    ));
}

#[test]
fn trigger_manifest_entries_round_trip_through_toml() {
    let source = r#"
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
autonomy_tier = "act_with_approval"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "high"
budget = { max_cost_usd = 0.001, max_tokens = 500, hourly_cost_usd = 1.0, daily_cost_usd = 5.0, max_concurrent = 10, on_budget_exhausted = "retry_later" }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"

[[triggers]]
id = "daily-digest"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://digest-queue"
schedule = "0 9 * * *"
timezone = "America/Los_Angeles"
"#;
    let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
    let encoded = toml::to_string(&parsed).expect("trigger tables encode");
    let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
    assert_eq!(reparsed, parsed);
}

#[test]
fn trigger_manifest_entries_parse_dlq_alerts() {
    let source = r##"
[[triggers]]
id = "cake-classifier"
kind = "webhook"
provider = "github"
handler = "handlers::classify"

[[triggers.dlq_alerts]]
destinations = [
  { kind = "slack", channel = "#ops", webhook_url_env = "OPS_SLACK_WEBHOOK" },
  { kind = "email", address = "ops@example.com" },
  { kind = "webhook", url = "https://alerts.example.com/harn" },
]
threshold = { entries_in_1h = 5, percent_of_dispatches = 20.0 }
"##;
    let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
    assert_eq!(parsed.triggers[0].dlq_alerts.len(), 1);
    let alert = &parsed.triggers[0].dlq_alerts[0];
    assert_eq!(alert.threshold.entries_in_1h, Some(5));
    assert_eq!(alert.threshold.percent_of_dispatches, Some(20.0));
    assert_eq!(alert.destinations[0].label(), "slack:#ops");
    assert_eq!(alert.destinations[1].label(), "email:ops@example.com");
    assert_eq!(
        alert.destinations[2].label(),
        "webhook:https://alerts.example.com/harn"
    );
}

#[test]
fn trigger_manifest_entries_round_trip_flow_control_tables() {
    let source = r#"
[[triggers]]
id = "github-priority"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
concurrency = { key = "event.headers.tenant", max = 2 }
throttle = { key = "event.headers.user", period = "1m", max = 30 }
rate_limit = { period = "1h", max = 1000 }
debounce = { key = "event.headers.pr_id", period = "30s" }
singleton = { key = "event.headers.repo" }
priority = { key = "event.headers.tier", order = ["gold", "silver", "bronze"] }
secrets = { signing_secret = "github/webhook-secret" }

[[triggers]]
id = "github-batch"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
batch = { key = "event.headers.repo", size = 50, timeout = "30s" }
secrets = { signing_secret = "github/webhook-secret" }
"#;
    let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
    let encoded = toml::to_string(&parsed).expect("trigger tables encode");
    let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
    assert_eq!(reparsed, parsed);
}

#[test]
fn trigger_manifest_entries_round_trip_stream_sources() {
    let source = r#"
[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"
when = "handlers::should_handle"
debounce = { key = "event.provider + \":\" + event.kind", period = "2s" }

[[triggers.sources]]
id = "open"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 14 * * 1-5"
timezone = "America/New_York"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
match = { events = ["quote.tick"] }
topic = "quotes"
consumer_group = "harn-market"
window = { mode = "sliding", key = "event.provider_payload.key", size = "5m", every = "1m", max_items = 5000 }
"#;
    let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
    assert_eq!(parsed.triggers.len(), 1);
    assert_eq!(parsed.triggers[0].sources.len(), 2);
    let encoded = toml::to_string(&parsed).expect("trigger tables encode");
    let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
    assert_eq!(reparsed, parsed);
}

#[test]
fn trigger_manifest_entries_parse_inline_sources() {
    let source = r#"
[[triggers]]
id = "ops-fan-in"
handler = "handlers::on_event"
sources = [
  { id = "tick", kind = "cron", provider = "cron", match = { events = ["cron.tick"] }, schedule = "*/5 * * * *", timezone = "UTC" },
  { id = "alerts", kind = "stream", provider = "nats", match = { events = ["alert.received"] }, subject = "alerts.>" },
]
"#;
    let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
    assert_eq!(parsed.triggers.len(), 1);
    assert_eq!(parsed.triggers[0].sources.len(), 2);
    assert_eq!(parsed.triggers[0].sources[1].provider.as_str(), "nats");
    assert_eq!(parsed.triggers[0].sources[1].kind, TriggerKind::Stream);
}

#[test]
fn load_runtime_extensions_ignores_package_triggers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"

[[triggers]]
id = "workspace-trigger"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://workspace-queue"
"#,
    )
    .unwrap();
    fs::write(
        root.join(".harn/packages/acme/harn.toml"),
        r#"
[package]
name = "acme"

[[triggers]]
id = "acme-trigger"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://acme-queue"
schedule = "0 9 * * *"
timezone = "UTC"
"#,
    )
    .unwrap();
    let harn_file = root.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();

    let extensions = load_runtime_extensions(&harn_file);
    assert_eq!(extensions.triggers.len(), 1);
    assert_eq!(extensions.triggers[0].id, "workspace-trigger");
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_local_handler_and_when() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[orchestrator.budget]
hourly_cost_usd = 1.0
daily_cost_usd = 5.0

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
tier = "suggest"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "normal"
budget = { max_cost_usd = 0.002, max_tokens = 250, hourly_cost_usd = 1.0, daily_cost_usd = 5.0, max_autonomous_decisions_per_hour = 25, max_autonomous_decisions_per_day = 100, max_concurrent = 10, on_budget_exhausted = "fail" }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}

pub fn should_handle(event: TriggerEvent) -> Result<bool, string> {
  return Result.Ok(event.provider == "github")
}
"#,
        ),
    );
    let extensions = load_runtime_extensions(&harn_file);
    assert_eq!(
        extensions
            .root_manifest
            .as_ref()
            .and_then(|manifest| manifest.orchestrator.budget.hourly_cost_usd),
        Some(1.0)
    );
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &extensions)
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    assert!(matches!(
        &collected[0].handler,
        CollectedTriggerHandler::Local { reference, .. } if reference.raw == "handlers::on_new_issue"
    ));
    assert_eq!(
        collected[0].config.dispatch_priority,
        TriggerDispatchPriority::Normal
    );
    assert_eq!(
        collected[0].config.autonomy_tier,
        harn_vm::AutonomyTier::Suggest
    );
    assert_eq!(
        collected[0]
            .flow_control
            .concurrency
            .as_ref()
            .map(|config| config.max),
        Some(10)
    );
    assert!(collected[0].when.is_some());
    assert_eq!(
        collected[0]
            .config
            .when_budget
            .as_ref()
            .and_then(|budget| budget.tokens_max),
        Some(500)
    );
    assert_eq!(collected[0].config.budget.hourly_cost_usd, Some(1.0));
    assert_eq!(
        collected[0].config.budget.max_autonomous_decisions_per_hour,
        Some(25)
    );
    assert_eq!(
        collected[0].config.budget.max_autonomous_decisions_per_day,
        Some(100)
    );
    assert_eq!(
        collected[0].config.budget.on_budget_exhausted,
        harn_vm::TriggerBudgetExhaustionStrategy::Fail
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_expression_keyed_flow_control() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-flow-control"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
concurrency = { key = "event.headers.tenant", max = 2 }
throttle = { key = "event.headers.user", period = "1m", max = 30 }
rate_limit = { period = "1h", max = 1000 }
debounce = { key = "event.headers.pr_id", period = "30s" }
singleton = { key = "event.headers.repo" }
priority = { key = "event.headers.tier", order = ["gold", "silver", "bronze"] }
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let extensions = load_runtime_extensions(&harn_file);
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &extensions)
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    let flow = &collected[0].flow_control;
    assert_eq!(
        flow.concurrency
            .as_ref()
            .and_then(|config| config.key.as_ref())
            .map(|expr| expr.raw.as_str()),
        Some("event.headers.tenant")
    );
    assert_eq!(flow.concurrency.as_ref().map(|config| config.max), Some(2));
    assert_eq!(
        flow.throttle
            .as_ref()
            .and_then(|config| config.key.as_ref())
            .map(|expr| expr.raw.as_str()),
        Some("event.headers.user")
    );
    assert_eq!(
        flow.throttle.as_ref().map(|config| config.period),
        Some(std::time::Duration::from_secs(60))
    );
    assert_eq!(flow.throttle.as_ref().map(|config| config.max), Some(30));
    assert!(flow
        .rate_limit
        .as_ref()
        .is_some_and(|config| config.key.is_none()));
    assert_eq!(
        flow.rate_limit.as_ref().map(|config| config.period),
        Some(std::time::Duration::from_secs(60 * 60))
    );
    assert_eq!(
        flow.rate_limit.as_ref().map(|config| config.max),
        Some(1000)
    );
    assert_eq!(
        flow.debounce.as_ref().map(|config| config.key.raw.as_str()),
        Some("event.headers.pr_id")
    );
    assert_eq!(
        flow.debounce.as_ref().map(|config| config.period),
        Some(std::time::Duration::from_secs(30))
    );
    assert_eq!(
        flow.singleton
            .as_ref()
            .and_then(|config| config.key.as_ref())
            .map(|expr| expr.raw.as_str()),
        Some("event.headers.repo")
    );
    assert_eq!(
        flow.priority.as_ref().map(|config| config.key.raw.as_str()),
        Some("event.headers.tier")
    );
    assert_eq!(
        flow.priority.as_ref().map(|config| config.order.clone()),
        Some(vec![
            "gold".to_string(),
            "silver".to_string(),
            "bronze".to_string(),
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_batch_flow_control() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-batch"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
batch = { key = "event.headers.repo", size = 50, timeout = "30s" }
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    assert_eq!(
        collected[0]
            .flow_control
            .batch
            .as_ref()
            .and_then(|config| config.key.as_ref())
            .map(|expr| expr.raw.as_str()),
        Some("event.headers.repo")
    );
    assert_eq!(
        collected[0]
            .flow_control
            .batch
            .as_ref()
            .map(|config| config.size),
        Some(50)
    );
    assert_eq!(
        collected[0]
            .flow_control
            .batch
            .as_ref()
            .map(|config| config.timeout),
        Some(std::time::Duration::from_secs(30))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_expands_stream_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"
when = "handlers::should_handle"
debounce = { key = "event.provider + \":\" + event.kind", period = "2s" }

[[triggers.sources]]
id = "open"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 14 * * 1-5"
timezone = "America/New_York"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
match = { events = ["quote.tick"] }
topic = "quotes"
consumer_group = "harn-market"
window = { mode = "sliding", key = "event.provider_payload.key", size = "5m", every = "1m", max_items = 5000 }
"#,
        Some(
            r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> bool {
  return event.provider == "cron" || event.provider == "kafka"
}

pub fn on_market_event(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 2);
    assert_eq!(collected[0].config.id, "market-fan-in.open");
    assert_eq!(collected[0].config.kind, TriggerKind::Cron);
    assert_eq!(collected[1].config.id, "market-fan-in.quotes");
    assert_eq!(collected[1].config.kind, TriggerKind::Stream);
    assert_eq!(collected[1].config.provider.as_str(), "kafka");
    assert_eq!(
        collected[1]
            .config
            .window
            .as_ref()
            .map(|window| window.mode),
        Some(TriggerStreamWindowMode::Sliding)
    );
    assert_eq!(
        collected[1]
            .flow_control
            .debounce
            .as_ref()
            .map(|config| config.period),
        Some(std::time::Duration::from_secs(2))
    );
    assert!(collected.iter().all(|trigger| trigger.when.is_some()));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_missing_trigger_match() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
handler = "handlers::on_new_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
        .await
        .expect_err("missing match should be rejected");
    assert!(error.contains("trigger table missing match"), "{error}");
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_missing_source_match() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
topic = "quotes"
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_market_event(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
        .await
        .expect_err("missing source match should be rejected");
    assert!(
        error.contains("trigger source 'quotes' missing match"),
        "{error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_a2a_allow_cleartext() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "local-a2a"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "a2a://127.0.0.1:8787/triage"
allow_cleartext = true
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    assert!(matches!(
        &collected[0].handler,
        CollectedTriggerHandler::A2a {
            target,
            allow_cleartext: true,
        } if target == "127.0.0.1:8787/triage"
    ));
}

#[test]
fn persona_triggers_install_as_manifest_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pr_opened"]
budget = { daily_usd = 2.0 }
"#,
        None,
    );
    let extensions = load_runtime_extensions(&harn_file);
    let bindings =
        collect_persona_trigger_binding_specs(&extensions).expect("persona bindings collect");

    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, "persona.merge_captain.github.pr_opened");
    assert_eq!(bindings[0].provider.as_str(), "github");
    assert_eq!(bindings[0].kind, "pr_opened");
    assert_eq!(bindings[0].handler.kind(), "persona");
    assert_eq!(bindings[0].daily_cost_usd, Some(2.0));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_persona_handler_uri() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"

[[triggers]]
id = "merge-captain-pr-opened"
kind = "webhook"
provider = "github"
match = { events = ["pr_opened"] }
handler = "persona://merge_captain"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");

    assert_eq!(collected.len(), 1);
    assert!(matches!(
        &collected[0].handler,
        CollectedTriggerHandler::Persona { binding } if binding.name == "merge_captain"
            && binding.entry_workflow == "workflows/merge_captain.harn#run"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_harn_provider_override() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "worker://echo-queue"
"#,
        None,
    );
    fs::write(
        tmp.path().join("echo_connector.harn"),
        test_harn_connector_source("echo"),
    )
    .unwrap();

    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].config.provider.as_str(), "echo");
    assert_eq!(
        harn_vm::provider_metadata("echo")
            .expect("provider metadata registered")
            .schema_name,
        "EchoEventPayload"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_duplicate_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "duplicate"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue-a"
secrets = { signing_secret = "github/webhook-secret" }

[[triggers]]
id = "duplicate"
kind = "webhook"
provider = "github"
match = { events = ["issues.edited"] }
handler = "worker://queue-b"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("duplicate trigger id 'duplicate'"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_unknown_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "unknown-provider"
kind = "webhook"
provider = "made-up"
match = { events = ["issues.opened"] }
handler = "worker://queue"
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("provider 'made-up' is not registered"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_non_bool_allow_cleartext() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-allow-cleartext-type"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "a2a://127.0.0.1:8787/triage"
allow_cleartext = "yes"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("`allow_cleartext` must be a boolean"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_priority_without_concurrency() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "priority-without-concurrency"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
priority = { key = "event.headers.tier", order = ["gold", "silver"] }
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("priority requires concurrency"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_allow_cleartext_on_non_a2a_handler() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-allow-cleartext-target"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
allow_cleartext = true
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("only valid for `a2a://...` handlers"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_unsupported_provider_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-kind"
kind = "cron"
provider = "github"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "0 9 * * *"
timezone = "UTC"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("does not support trigger kind 'cron'"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_missing_required_provider_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "missing-secret"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("requires secret 'signing_secret'"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_unresolved_handler() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "missing-handler"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::missing"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("handler 'handlers::missing' is not exported"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_malformed_cron() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-cron"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "not a cron"
timezone = "America/Los_Angeles"
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("invalid cron schedule"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_utc_offset_timezone() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-cron-timezone"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "0 9 * * *"
timezone = "+02:00"
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("use an IANA timezone name"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_invalid_dedupe_expression() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-dedupe"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
dedupe_key = "["
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("dedupe_key"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_zero_retention_days() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-retention"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
retry = { retention_days = 0 }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(
        error.contains("retry.retention_days"),
        "actual error: {error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_secret_namespace_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-secret"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
secrets = { signing_secret = "slack/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("uses namespace 'slack'"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_invalid_when_signature() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "bad-when"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("must have signature fn(TriggerEvent) -> bool or Result<bool, _>"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_when_budget_without_when() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[triggers]]
id = "bad-when-budget"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when_budget = { timeout = "5s" }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("when_budget requires a when predicate"));
}

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_rejects_invalid_when_budget_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "bad-when-timeout"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { timeout = "soon" }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> bool {
  return true
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.contains("when_budget.timeout"));
}
