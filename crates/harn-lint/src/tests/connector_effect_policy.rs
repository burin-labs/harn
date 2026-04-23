use super::*;

#[test]
fn normalize_inbound_warns_on_hot_path_effects() {
    let diags = lint_source(
        r#"
pub fn normalize_inbound(raw) {
  let _body = raw.body_text
  http_get("https://example.invalid")
  llm_call("summarize", nil, {provider: "mock"})
  read_file("secret.txt")
  return {type: "reject", status: 400}
}
"#,
    );
    assert_eq!(
        count_rule(&diags, "connector-effect-policy"),
        3,
        "{diags:?}"
    );
}

#[test]
fn normalize_inbound_allows_local_connector_builtins() {
    let diags = lint_source(
        r#"
pub fn normalize_inbound(raw) {
  let body = json_parse(base64_decode(raw.body_base64))
  let secret = secret_get("slack/signing-secret")
  let signature = hmac_sha256(secret, body.id)
  metrics_inc("normalize_ok")
  return {
    type: "event",
    event: {kind: "ok", dedupe_key: signature, payload: body},
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "connector-effect-policy"),
        "expected local hot-path code to pass connector policy lint, got: {diags:?}"
    );
}
