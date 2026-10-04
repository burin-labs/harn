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
        r#"
fn read(payload: dict, raw: any) -> any {
  const a = payload?.data?.repository?.pullRequest
  const b = raw.data?.items?.[0]?.name
  return [a, b]
}
"#,
        "untyped-optional-chain",
    );
    assert_eq!(count, 2);
}

#[test]
fn untyped_optional_chain_ignores_typed_optional_fields_and_single_links() {
    let count = rule_count(
        r#"
type Author = {login: string}
type Pr = {author: Author?, mergeCommit: {oid: string}?}
fn read(pr: Pr?, options: dict?) -> any {
  const login = pr?.author?.login
  const oid = pr?.mergeCommit?.oid
  const flag = options?.verbose
  return [login, oid, flag]
}
"#,
        "untyped-optional-chain",
    );
    assert_eq!(count, 0);
}
