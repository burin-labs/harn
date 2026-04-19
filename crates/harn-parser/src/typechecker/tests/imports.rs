use super::*;

fn parse_program(source: &str) -> Vec<crate::SNode> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    parser.parse().unwrap()
}

#[test]
fn imported_type_aliases_are_registered_into_scope() {
    let imported = parse_program(
        r#"
type SignatureStatus = { state: "verified" } | { state: "failed", reason: string }

type GitHubEventPayload = {
  provider: "github",
  action: string | nil,
  raw: dict,
}

type SlackEventPayload = {
  provider: "slack",
  subtype: string | nil,
  raw: dict,
}

type ProviderPayload = GitHubEventPayload | SlackEventPayload

type TriggerEvent = {
  provider_payload: ProviderPayload,
  signature_status: SignatureStatus,
}
"#,
    );
    let program = parse_program(
        r#"
pipeline t(task) {
  let event: TriggerEvent = {
    provider_payload: {
      provider: "github",
      action: "opened",
      raw: {},
    },
    signature_status: {
      state: "failed",
      reason: "bad signature",
    },
  }

  let payload = event.provider_payload
  if payload.provider == "github" {
    let action: string | nil = payload.action
  } else {
    let subtype: string | nil = payload.subtype
  }

  let status = event.signature_status
  if status.state == "failed" {
    let reason: string = status.reason
  }
}
"#,
    );

    let diagnostics = TypeChecker::new()
        .with_imported_type_decls(imported)
        .check(&program);
    let errors: Vec<String> = diagnostics
        .into_iter()
        .filter(|diag| diag.severity == DiagnosticSeverity::Error)
        .map(|diag| diag.message)
        .collect();
    assert!(errors.is_empty(), "got imported-type errors: {errors:?}");
}

#[test]
fn imported_structs_allow_field_access() {
    let imported = parse_program(
        r#"
struct HeaderRecord {
  name: string,
  value: string,
}
"#,
    );
    let program = parse_program(
        r#"
pipeline t(task) {
  let header: HeaderRecord = HeaderRecord { name: "X-Test", value: "ok" }
  let value: string = header.value
}
"#,
    );

    let diagnostics = TypeChecker::new()
        .with_imported_type_decls(imported)
        .check(&program);
    let errors: Vec<String> = diagnostics
        .into_iter()
        .filter(|diag| diag.severity == DiagnosticSeverity::Error)
        .map(|diag| diag.message)
        .collect();
    assert!(errors.is_empty(), "got imported-struct errors: {errors:?}");
}
