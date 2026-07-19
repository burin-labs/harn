use super::*;

fn queue(document: &str) -> MockQueue {
    MockQueue::from_fixture(crate::llm::jsonl::parse_llm_mocks_jsonl(document).unwrap())
}

fn fixture(lines: &[&str]) -> MockQueue {
    let document = lines.join("\n");
    queue(&document)
}

#[test]
fn scopes_are_isolated_and_default_is_the_only_fallback() {
    let mut queue = queue(
        r#"{"schemaVersion":1,"strictScopes":false}
{"id":"main","scope":"agent.main","consume":"once","text":"MAIN"}
{"id":"default","scope":"default","consume":"once","text":"DEFAULT"}"#,
    );

    let fallback = queue
        .match_request("completion.judge", "verify")
        .expect("auxiliary scope falls through to default");
    assert_eq!(fallback.mock.text, "DEFAULT");
    assert_eq!(fallback.receipt.requested_scope, "completion.judge");
    assert_eq!(fallback.receipt.resolved_scope, "default");
    assert!(fallback.receipt.fell_through);

    let main = queue
        .match_request("agent.main", "turn")
        .expect("main scope remains available");
    assert_eq!(main.mock.text, "MAIN");
    assert!(!main.receipt.fell_through);

    assert!(queue.match_request("completion.judge", "again").is_none());
}

#[test]
fn strict_scopes_make_missing_scope_a_hard_miss() {
    let mut queue = queue(
        r#"{"schemaVersion":1,"strictScopes":true}
{"id":"default","scope":"default","consume":"once","text":"DEFAULT"}"#,
    );
    assert!(queue.match_request("completion.judge", "verify").is_none());
    assert_eq!(queue.miss_receipt("completion.judge").remaining, 0);
    assert_eq!(
        queue.match_request("default", "verify").unwrap().mock.text,
        "DEFAULT"
    );
}

#[test]
fn sticky_entries_do_not_change_remaining_counts() {
    let mut queue = fixture(&[
        r#"{"schemaVersion":1,"strictScopes":false}"#,
        r#"{"id":"judge","scope":"completion.judge","consume":"sticky","match":"*","text":"JUDGE"}"#,
        r#"{"id":"once","scope":"completion.judge","consume":"once","text":"ONCE"}"#,
    ]);

    let first = queue
        .match_request("completion.judge", "question")
        .expect("sticky entry");
    assert_eq!(first.mock.text, "ONCE");
    assert_eq!(first.receipt.remaining, 1);

    let second = queue
        .match_request("completion.judge", "question")
        .expect("sticky entry");
    assert_eq!(second.mock.text, "JUDGE");
    assert_eq!(second.receipt.consume, "sticky");
    assert_eq!(second.receipt.remaining, 1);

    let third = queue
        .match_request("completion.judge", "question")
        .expect("sticky entry repeats");
    assert_eq!(third.mock.entry_id, "judge");
}
