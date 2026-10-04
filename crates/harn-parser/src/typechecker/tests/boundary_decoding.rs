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
