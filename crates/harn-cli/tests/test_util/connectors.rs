#![allow(dead_code)]

use std::fs;
use std::path::Path;

const GITHUB_CONNECTOR: &str = r#"
import { verify_hmac_signature } from "std/connectors/shared"

fn header(headers, name) {
  return headers[name] ?? headers[lowercase(name)] ?? ""
}

pub fn provider_id() {
  return "github"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {harn_schema_name: "GitHubEventPayload", json_schema: {type: "object", additionalProperties: true}}
}

pub fn init(_ctx) {}

pub fn activate(_bindings) {}

pub fn normalize_inbound(raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  const secret = secret_get("github/webhook-secret")
  if !verify_hmac_signature(raw.body_text ?? "", header(raw.headers, "X-Hub-Signature-256"), secret) {
    return {type: "reject", status: 400, body: "invalid github signature"}
  }
  return {
    type: "event",
    event: {
      kind: header(raw.headers, "X-GitHub-Event"),
      dedupe_key: header(raw.headers, "X-GitHub-Delivery"),
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#;

const SLACK_CONNECTOR: &str = r#"
fn header(headers, name) {
  return headers[name] ?? headers[lowercase(name)] ?? ""
}

pub fn provider_id() {
  return "slack"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {harn_schema_name: "SlackEventPayload", json_schema: {type: "object", additionalProperties: true}}
}

pub fn init(_ctx) {}

pub fn activate(_bindings) {}

fn verified_slack_signature(raw) {
  const timestamp = header(raw.headers, "X-Slack-Request-Timestamp")
  const signature = header(raw.headers, "X-Slack-Signature")
  const secret = secret_get("slack/signing-secret")
  const expected = "v0=" + hmac_sha256(secret, "v0:" + timestamp + ":" + (raw.body_text ?? ""))
  return constant_time_eq(expected, signature)
}

pub fn normalize_inbound(raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  if !verified_slack_signature(raw) {
    return {type: "reject", status: 400, body: "invalid slack signature"}
  }
  if body.type == "url_verification" {
    return {
      type: "immediate_response",
      immediate_response: {status: 200, headers: {["content-type"]: "text/plain; charset=utf-8"}, body: body.challenge},
    }
  }
  return {
    type: "event",
    event: {
      kind: body.event.type,
      dedupe_key: body.event_id,
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#;

const NOTION_CONNECTOR: &str = r#"
import { verify_hmac_signature } from "std/connectors/shared"

fn header(headers, name) {
  return headers[name] ?? headers[lowercase(name)] ?? ""
}

pub fn provider_id() {
  return "notion"
}

pub fn kinds() {
  return ["webhook", "poll"]
}

pub fn payload_schema() {
  return {harn_schema_name: "NotionEventPayload", json_schema: {type: "object", additionalProperties: true}}
}

pub fn init(_ctx) {}

pub fn activate(_bindings) {}

pub fn normalize_inbound(raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  if body.verification_token != nil {
    return {
      type: "immediate_response",
      immediate_response: {
        status: 200,
        headers: {["content-type"]: "application/json"},
        body: json_stringify({status: "handshake_captured", verification_token: body.verification_token}),
      },
    }
  }
  const secret = secret_get("notion/verification-token")
  if !verify_hmac_signature(raw.body_text ?? "", header(raw.headers, "X-Notion-Signature"), secret) {
    return {type: "reject", status: 400, body: "invalid notion signature"}
  }
  return {
    type: "event",
    event: {
      kind: body.type,
      dedupe_key: "notion:" + body.entity.id,
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#;

pub fn github_connector_module() -> &'static str {
    GITHUB_CONNECTOR
}

pub fn provider_declarations() -> &'static str {
    r#"
[[providers]]
id = "github"
connector = { harn = "github_connector.harn" }

[[providers]]
id = "slack"
connector = { harn = "slack_connector.harn" }

[[providers]]
id = "notion"
connector = { harn = "notion_connector.harn" }
"#
}

pub fn write_first_party_connector_modules(dir: &Path) {
    write_if_missing(dir, "github_connector.harn", GITHUB_CONNECTOR);
    write_if_missing(dir, "slack_connector.harn", SLACK_CONNECTOR);
    write_if_missing(dir, "notion_connector.harn", NOTION_CONNECTOR);
}

fn write_if_missing(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}
