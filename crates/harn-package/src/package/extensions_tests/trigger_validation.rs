use super::*;

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
provider = "webhook"
handler = "handlers::on_new_issue"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
        .await
        .expect_err("missing match should be rejected");
    assert!(
        error.to_string().contains("trigger table missing match"),
        "{error}"
    );
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

pub fn on_market_event(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
        .await
        .expect_err("missing source match should be rejected");
    assert!(
        error
            .to_string()
            .contains("trigger source 'quotes' missing match"),
        "{error}"
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "worker://queue-a"
secrets = { signing_secret = "webhook/signing-secret" }

[[triggers]]
id = "duplicate"
kind = "webhook"
provider = "webhook"
match = { events = ["issues.edited"] }
handler = "worker://queue-b"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("duplicate trigger id 'duplicate'"));
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
    assert!(error
        .to_string()
        .contains("provider 'made-up' is not registered"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "a2a://127.0.0.1:8787/triage"
allow_cleartext = "yes"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("`allow_cleartext` must be a boolean"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
priority = { key = "event.headers.tier", order = ["gold", "silver"] }
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("priority requires concurrency"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "worker://queue"
allow_cleartext = true
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("only valid for `a2a://...` handlers"));
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
provider = "webhook"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "0 9 * * *"
timezone = "UTC"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not support trigger kind 'cron'"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "worker://queue"
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("requires secret 'signing_secret'"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "handlers::missing"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        Some(
            r#"
import "std/triggers"

pub fn on_new_issue(harness: Harness, event: TriggerEvent) {
  harness.stdio.log(event.kind)
}
"#,
        ),
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("handler 'handlers::missing' is not exported"));
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
    assert!(error.to_string().contains("invalid cron schedule"));
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
    assert!(error.to_string().contains("use an IANA timezone name"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "worker://queue"
dedupe_key = "["
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("dedupe_key"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "worker://queue"
secrets = { signing_secret = "webhook/signing-secret" }
retry = { retention_days = 0 }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("retry.retention_days"),
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
provider = "webhook"
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
    assert!(error.to_string().contains("uses namespace 'slack'"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "worker://queue"
secrets = { signing_secret = "webhook/signing-secret" }
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
    assert!(error
        .to_string()
        .contains("must have signature fn(TriggerEvent) -> bool or Result<bool, _>"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
when_budget = { timeout = "5s" }
handler = "worker://queue"
secrets = { signing_secret = "webhook/signing-secret" }
"#,
        None,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("when_budget requires a when predicate"));
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
provider = "webhook"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { timeout = "soon" }
handler = "worker://queue"
secrets = { signing_secret = "webhook/signing-secret" }
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
    assert!(error.to_string().contains("when_budget.timeout"));
}
