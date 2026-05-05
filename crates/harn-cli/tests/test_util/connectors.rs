#![allow(dead_code)]

pub fn github_connector_module() -> &'static str {
    r#"
pub fn provider_id() {
  return "github"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return "GitHubEventPayload"
}

pub fn normalize_inbound(raw) {
  let decoded = base64_decode(raw.body_base64)
  let secret = secret_get("github/webhook-secret")
  let signature = raw.headers["X-Hub-Signature-256"] ?? raw.headers["x-hub-signature-256"] ?? ""
  let expected = "sha256=" + hmac_sha256(secret, decoded)
  if !constant_time_eq(signature, expected) {
    return {
      type: "reject",
      reject: {
        status: 400,
        body: {error: "invalid_signature"},
      },
    }
  }

  let body = raw.body_json ?? json_parse(decoded)
  let event = raw.headers["X-GitHub-Event"] ?? raw.headers["x-github-event"] ?? "webhook"
  let delivery = raw.headers["X-GitHub-Delivery"] ?? raw.headers["x-github-delivery"] ?? sha256(decoded)
  return {
    type: "event",
    event: {
      kind: event,
      dedupe_key: delivery,
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#
}
