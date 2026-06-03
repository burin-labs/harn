use super::emit;

const HEADER: &str = "// header\n\n";

fn render_default() -> String {
    emit::render(harn_stdlib::CONNECTOR_EVENT_SCHEMAS_SOURCE, HEADER)
        .expect("canonical schema renders")
}

#[test]
fn renders_the_canonical_github_schema() {
    let rust = render_default();
    // Common block is a flattened struct.
    assert!(rust.contains("pub struct GitHubEventCommon {"));
    assert!(rust.contains("#[serde(flatten)]\n    pub common: GitHubEventCommon,"));
    // A required `any` subtree maps to JsonValue.
    assert!(rust.contains("pub raw: JsonValue,"));
    // Optional scalar gets the skip attribute.
    assert!(rust.contains(
        "#[serde(default, skip_serializing_if = \"Option::is_none\")]\n    pub action: Option<String>,"
    ));
    // Optional int maps to Option<i64>.
    assert!(rust.contains("pub installation_id: Option<i64>,"));
    // Required list maps to Vec with a serde default.
    assert!(rust.contains("#[serde(default)]\n    pub commits: Vec<JsonValue>,"));
    assert!(rust.contains("pub pull_request_numbers: Vec<i64>,"));
    // The dispatched enum and its manual Deserialize.
    assert!(rust.contains("pub enum GitHubEventPayload {"));
    assert!(rust.contains("Issues(GitHubIssuesEventPayload),"));
    assert!(rust.contains("InstallationRepositories(GitHubInstallationRepositoriesEventPayload),"));
    assert!(rust.contains("Other(GitHubEventCommon),"));
    assert!(rust.contains("impl<'de> Deserialize<'de> for GitHubEventPayload {"));
    // Dispatch discriminators match the wire event names.
    assert!(rust.contains("\"issue_comment\" => GitHubEventPayload::IssueComment("));
    assert!(rust.contains(
        "\"installation_repositories\" => GitHubEventPayload::InstallationRepositories("
    ));
}

#[test]
fn unsupported_type_form_is_a_hard_error() {
    let source = "type Bad = int\n";
    let err = emit::render(source, HEADER).expect_err("alias is unsupported");
    assert!(
        err.contains("Bad"),
        "error should name the offending type: {err}"
    );
}
