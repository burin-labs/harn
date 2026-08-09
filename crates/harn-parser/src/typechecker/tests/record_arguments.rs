//! Closed record contracts at function-call boundaries.

use super::*;

#[test]
fn named_record_parameter_accepts_exact_literal_and_inferred_local() {
    let errs = errors(
        r#"type BranchRequest = {owner: string, repo: string, branch: string}

fn view(request: BranchRequest) -> string {
  return request.owner + "/" + request.repo + ":" + request.branch
}

pipeline test_named_record(task) {
  view({owner: "octo", repo: "demo", branch: "main"})
  let request = {owner: "octo", repo: "demo", branch: "next"}
  view(request)
  let internal = {owner: "octo", repo: "demo", branch: "release", force: true}
  view(internal)
}"#,
    );

    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn intersected_record_parameter_accepts_exact_literal_and_inferred_local() {
    let errs = errors(
        r#"type Repository = {owner: string, repo: string}
type Branch = {branch: string}
type BranchRequest = Repository & Branch

fn view(request: BranchRequest) -> string {
  return request.owner + "/" + request.repo + ":" + request.branch
}

pipeline test_intersected_record(task) {
  view({owner: "octo", repo: "demo", branch: "main"})
  let request = {owner: "octo", repo: "demo", branch: "next"}
  view(request)
}"#,
    );

    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn named_record_parameter_rejects_inexact_literals() {
    let errs = errors(
        r#"type BranchRequest = {owner: string, repo: string, branch: string}
type Repository = {owner: string, repo: string}
type IntersectedBranchRequest = Repository & {branch: string}
type NestedRequest = {repository: {owner: string, repo: string}}

fn view(request: BranchRequest) -> nil {
  return nil
}

fn view_intersected(request: IntersectedBranchRequest) -> nil {
  return nil
}

fn view_nested(request: NestedRequest) -> nil {
  return nil
}

pipeline test_inexact_records(task) {
  view({owner: "octo", repo: "demo"})
  view({owner: "octo", repo: "demo", branch: 42})
  view({owner: "octo", repo: "demo", branch: "main", force: true})
  view_intersected({owner: "octo", repo: "demo", branch: "main", force: true})
  view_nested({repository: {owner: "octo", repo: "demo", fork: true}})
}"#,
    );

    assert_eq!(
        errs.len(),
        5,
        "expected one error per inexact literal: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|error| error.contains("unknown field `force` in closed record")),
        "expected a precise unknown-field error: {errs:?}"
    );
}

#[test]
fn anonymous_closed_record_parameter_rejects_extra_literal_fields() {
    let errs = errors(
        r#"fn greet(user: {name: string}) -> string {
  return "hi " + user.name
}

pipeline test_closed_record(task) {
  greet({name: "Bob", age: 25})
}"#,
    );

    assert_eq!(errs.len(), 1, "expected an excess-field error: {errs:?}");
    assert!(errs[0].contains("unknown field `age` in closed record"));
}

#[test]
fn open_record_parameter_accepts_extra_literal_fields() {
    let errs = errors(
        r#"fn greet(user: {name: string, ...dict}) -> string {
  return "hi " + user.name
}

pipeline test_open_record(task) {
  greet({name: "Bob", age: 25})
}"#,
    );

    assert!(
        errs.is_empty(),
        "open record rejected extra fields: {errs:?}"
    );
}

#[test]
fn closed_builtin_record_parameter_rejects_extra_literal_field() {
    let errs = errors(
        r#"pipeline t(task) {
  llm_call("prompt", nil, {provider: "mock", max_toknes: 256})
}"#,
    );

    assert_eq!(errs.len(), 1, "expected an excess-field error: {errs:?}");
    assert!(errs[0].contains("unknown field `max_toknes` in closed record"));
    assert!(errs[0].contains("did you mean `max_tokens`?"));
}
