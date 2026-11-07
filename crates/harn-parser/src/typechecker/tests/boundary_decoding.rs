//! A `?.` chain that hedges an undeclared shape is reported once; one over
//! declared optional fields is not. The typed decoders themselves are
//! registry-backed builtins, so `harn-cli`'s `boundary_decoding` covers them.

use super::*;
use crate::DiagnosticDetails;

fn rule_count(source: &str, rule_name: &str) -> usize {
    check_source_with_source(source)
        .iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.details,
                Some(DiagnosticDetails::LintRule { rule }) if *rule == rule_name
            )
        })
        .count()
}

#[test]
fn untyped_optional_chain_reports_once_per_chain() {
    let count = rule_count(
        r"
fn read(payload: dict, raw: any, bag: dict<string, any>) -> any {
  const a = payload?.data?.repository?.pullRequest
  const b = raw.data?.items?.[0]?.name
  const c = bag?.options?.verbose
  return [a, b, c]
}
",
        "untyped-optional-chain",
    );
    assert_eq!(count, 3);
}

#[test]
fn native_transcript_events_keep_guaranteed_fields_typed() {
    let source = r#"
fn read() -> string {
  const event = transcript_reminder_event({body: "hello"})
  const block = event?.blocks?.[0]?.text
  return event.reminder.body + (block ?? "")
}
fn passthrough(payload: dict) -> any {
  const event = transcript_suspension_event(payload)
  const block = event?.blocks?.[0]?.text
  return [block, event.suspension?.custom?.value]
}
"#;
    let checked = errors(source);
    assert!(checked.is_empty(), "{checked:?}");
    assert_eq!(rule_count(source, "untyped-optional-chain"), 1);
    let wrong = errors(
        r#"fn wrong() -> int { return transcript_reminder_event({body: "hello"}).reminder.body }"#,
    );
    assert_eq!(wrong.len(), 1, "{wrong:?}");
    assert!(wrong[0].contains("expected int, found string"), "{wrong:?}");
}

/// An open row is untyped only for the keys its tail carries.
#[test]
fn untyped_optional_chain_splits_an_open_row_by_declared_field() {
    let header = "type Row = {name: {first: string}?, ...dict}\n";
    let undeclared = rule_count(
        &format!("{header}fn read(row: Row) -> any {{\n  return row?.extra?.value\n}}\n"),
        "untyped-optional-chain",
    );
    let declared = rule_count(
        &format!("{header}fn read(row: Row) -> any {{\n  return row?.name?.first\n}}\n"),
        "untyped-optional-chain",
    );
    assert_eq!((undeclared, declared), (1, 0));
    let generic_tail = rule_count(
        "fn read<R>(row: {name: string, ...R}) -> any {\n  return row?.extra?.value\n}\n",
        "untyped-optional-chain",
    );
    assert_eq!(generic_tail, 1, "a still-generic row tail is untyped");
}

#[test]
fn open_record_reads_preserve_declared_fields_and_typed_tails() {
    let source = r"
type Event = {name: string, metadata?: {source: string}, ...dict}
fn known(event: Event) -> string { return event.name }
fn optional(event: Event?) -> string? { return event?.metadata?.source }
fn map_tail(event: {tag: string, ...dict<string, {name: string}>}) -> string {
  return event.extension.name
}
";
    let diagnostics = check_source_with_source(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let wrong = errors(
        "type Event = {name: string, ...dict}\nfn wrong(event: Event) -> int { return event.name }",
    );
    assert_eq!(wrong.len(), 1, "{wrong:?}");
    assert!(wrong[0].contains("expected int, found string"), "{wrong:?}");
    assert_eq!(
        rule_count(
            "type Event = {name: string, ...dict}\nfn unknown(event: Event) { return event?.extension?.name }",
            "untyped-optional-chain",
        ),
        1,
        "the unknown extension remains untyped"
    );
}

#[test]
fn nested_open_record_chains_classify_the_key_being_read() {
    let source = r#"
type Child = {name: string, ...dict}
type Packet = {child?: Child, ...dict}
fn known(packet: Packet?) -> string? { return packet?.child?.name }
fn indexed(packet: Packet?) -> string? { return packet?.["child"]?.["name"] }
fn unknown(packet: Packet?) { return packet?.child?.future?.name }
fn unknown_index(packet: Packet?) { return packet?.["extra"]?.["name"] }
fn dynamic(packet: Packet?, key: string) { return packet?.[key]?.["name"] }
"#;
    assert!(errors(source).is_empty(), "{:?}", errors(source));
    assert_eq!(rule_count(source, "untyped-optional-chain"), 3);
}

#[test]
fn open_record_subscripts_preserve_declared_and_tail_slot_types() {
    let source = r#"
type Row = {name: string, count?: int, ...dict}
fn known(row: Row) -> string { return row["name"] }
fn optional(row: Row?) -> int? { return row?.["count"] }
fn map_tail(row: {tag: string, ...dict<string, int>}) -> int? { return row["extra"] }
fn writes() {
  let row: Row = {name: "valid"}
  row["name"] = "updated"
  row["count"] = nil
  row["extension"] = {arbitrary: true}
}
"#;
    assert!(errors(source).is_empty(), "{:?}", errors(source));
    for source in [
        "type Row = {name: string, ...dict}\nfn wrong(row: Row) -> int { return row[\"name\"] }",
        "fn wrong() { let row: {name: string, ...dict} = {name: \"valid\"}; row[\"name\"] = 7 }",
        "fn wrong() { let row: {tag: string, ...dict<string, int>} = {tag: \"valid\"}; row[\"extra\"] = \"invalid\" }",
    ] {
        let checked = errors(source);
        assert_eq!(checked.len(), 1, "{source}: {checked:?}");
        assert!(
            checked[0].contains("expected") && checked[0].contains("found"),
            "{checked:?}"
        );
    }
}
#[test]
fn untyped_optional_chain_ignores_typed_optional_fields_and_single_links() {
    let count = rule_count(
        r"
type Author = {login: string}
type Pr = {author: Author?, mergeCommit: {oid: string}?}
fn read(pr: Pr?, options: dict?, authors: dict<string, Author>) -> any {
  const login = pr?.author?.login
  const oid = pr?.mergeCommit?.oid
  const flag = options?.verbose
  const named = authors?.ada?.login
  return [login, oid, flag, named]
}
",
        "untyped-optional-chain",
    );
    assert_eq!(count, 0);
}

/// A declared field whose type is untyped leaves the next `?.` hedging, and
/// an undeclared key is untyped when any one of several tails is.
#[test]
fn untyped_optional_chain_sees_untyped_declared_fields_and_any_untyped_tail() {
    let declared_any = rule_count(
        "type Envelope = {data: any, meta: {id: string}?}\n\
         fn read(env: Envelope) -> any {\n  return [env?.data?.items, env?.meta?.id]\n}\n",
        "untyped-optional-chain",
    );
    assert_eq!(declared_any, 1, "only the chain through `data: any`");
    let mixed_tails = rule_count(
        "type Typed = {id: string}\n\
         fn read<R>(row: {name: string, ...Typed, ...R}) -> any {\n  return row?.extra?.value\n}\n",
        "untyped-optional-chain",
    );
    assert_eq!(mixed_tails, 1, "the generic tail can carry `extra`");
}
