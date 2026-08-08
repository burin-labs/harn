//! Tests for the repair synthesis in `super::super::repair_synthesis`: the
//! capability and root arguments a migration inserts at a diagnosed call, and
//! the signature edits that thread a harness to reach them.

use super::*;

#[test]
fn callable_param_insert_handles_dict_defaults_before_body() {
    let source = "pub fn poll(check, options: dict = {}) -> any {\n  harness.clock.now_ms()\n}\n";
    let (offset, has_params) = super::signature_threading::callable_param_insert(
        source,
        harn_lexer::Span::with_offsets(0, source.len(), 1, 1),
    )
    .expect("callable header");
    assert!(has_params);
    assert_eq!(&source[offset..offset + 5], "check");
}

#[test]
fn missing_capability_argument_repair_uses_typed_root_field() {
    let source = "fn main(harness: Harness) {\n  helper(\"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let span = source.find("\"old\"").unwrap();
    let span = harn_lexer::Span::with_offsets(span, span + 5, 2, 10);
    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("string"),
        source,
        &program,
    )
    .expect("capability migration repair");
    assert_eq!(edits.len(), 1);
    let insert_at = source.find("(\"old\")").unwrap() + 1;
    assert_eq!(edits[0].span.start, insert_at);
    assert_eq!(edits[0].span.end, insert_at);
    assert_eq!(edits[0].replacement, "harness.fs, ");
}

#[test]
fn missing_capability_argument_is_inserted_at_the_diagnosed_position() {
    let source = "fn main(harness: Harness) {\n  helper(harness.fs, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("\"old\"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + 5, 2, 22);
    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessSystem"),
        &named_type("string"),
        source,
        &program,
    )
    .expect("positioned capability repair");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].replacement, ", harness.system");
    assert_eq!(
        FixEdit::apply_all(source, &edits),
        "fn main(harness: Harness) {\n  helper(harness.fs, harness.system, \"old\")\n}\n"
    );
}

#[test]
fn attenuated_capability_argument_repair_projects_existing_root_grant() {
    let source = "fn main(harness: Harness) {\n  helper(harness, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, \"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 10);
    let (repair, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("Harness"),
        source,
        &program,
    )
    .expect("attenuation repair");
    assert_eq!(repair.id.as_str(), "bindings/attenuate-capability-argument");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].span, span);
    assert_eq!(edits[0].replacement, "harness.fs");
}

/// A root grant reached through a field attenuates the same way a bare one does.
///
/// The attenuation repair only matched a bare identifier, so a call that passed
/// `request.harness` got no repair at all. That is not a rare shape: a request
/// record carrying its own `harness` field is the ordinary way to hand a
/// capability through a typed boundary, and it is exactly what harn#6138 hit.
/// The migration would attenuate the callee's parameter and leave every such
/// caller passing a full `Harness`, so the tree it produced did not type-check.
#[test]
fn attenuated_capability_argument_repair_projects_a_root_grant_reached_through_a_field() {
    let source = "fn main(request: Request) {\n  helper(request.harness, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("request.harness, \"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "request.harness".len(), 2, 10);
    let (repair, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessObs"),
        &named_type("Harness"),
        source,
        &program,
    )
    .expect("attenuation repair for a field-reached root grant");
    assert_eq!(repair.id.as_str(), "bindings/attenuate-capability-argument");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].span, span);
    assert_eq!(edits[0].replacement, "request.harness.obs");
    assert_eq!(
        FixEdit::apply_all(source, &edits),
        "fn main(request: Request) {\n  helper(request.harness.obs, \"old\")\n}\n"
    );
}

/// An argument that is not a plain path is left alone.
///
/// Appending a sub-grant to a path is structural. Appending it to a call would
/// change what runs, and appending it to anything the fixer cannot re-root is a
/// guess. Those sites belong to a human.
#[test]
fn attenuated_capability_argument_repair_declines_a_non_path_argument() {
    let source = "fn main(harness: Harness) {\n  helper(pick(harness), \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("pick(harness)").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "pick(harness)".len(), 2, 10);
    assert!(
        synthesize_missing_capability_argument_repair(
            span,
            &named_type("HarnessObs"),
            &named_type("Harness"),
            source,
            &program,
        )
        .is_none(),
        "a call expression must not be re-rooted"
    );
}

#[test]
fn attenuated_capability_bundle_repair_projects_existing_root_grant() {
    let source = "fn main(harness: Harness) {\n  helper(harness, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, \"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 10);
    let (repair, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &capability_shape(&[("fs", "HarnessFs"), ("tools", "HarnessTools")]),
        &named_type("Harness"),
        source,
        &program,
    )
    .expect("capability bundle repair");
    assert_eq!(
        repair.id.as_str(),
        "bindings/attenuate-capability-bundle-argument"
    );
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].span, span);
    assert_eq!(
        edits[0].replacement,
        "{fs: harness.fs, tools: harness.tools}"
    );
}

#[test]
fn missing_capability_argument_repair_inserts_before_parenthesized_expression() {
    let source = "fn main(harness: Harness) {\n  helper((params ?? {}) + {path: \"src\"})\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let mut span = None;
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == "helper" {
                span = args.first().map(|arg| arg.span);
            }
        }
    });
    let span = span.expect("helper first argument");

    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("dict"),
        source,
        &program,
    )
    .expect("capability migration repair");
    let insert_at = source.find("((params").unwrap() + 1;
    assert_eq!(edits[0].span.start, insert_at);
    assert_eq!(edits[0].replacement, "harness.fs, ");
}

#[test]
fn missing_root_argument_threads_a_distinct_root_past_a_narrow_harness_binding() {
    let source = "fn leaf(harness: HarnessFs, path: string) {\n  needs_root(harness, path)\n}\n\nfn main(harness: Harness) {\n  leaf(harness.fs, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, path").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 14);
    let (_, edits, _) = synthesize_missing_root_argument_repair(
        span,
        source,
        &program,
        &BTreeSet::new(),
        &AmbientRepairContext {
            cross_module_importer_count: 0,
        },
        &mut ValueEscape {
            manifest_handlers: &BTreeSet::new(),
            referenced_by_value: &BTreeSet::new(),
            escape_sites: &BTreeMap::new(),
            frozen: &mut Vec::new(),
        },
    )
    .expect("root threading repair");
    let fixed = FixEdit::apply_all(source, &edits);
    assert_eq!(
        fixed,
        "fn leaf(_harness: Harness, harness: HarnessFs, path: string) {\n  needs_root(_harness, harness, path)\n}\n\nfn main(harness: Harness) {\n  leaf(harness, harness.fs, \"old\")\n}\n"
    );
}

#[test]
fn missing_capability_argument_repair_rejects_non_call_spans() {
    let source = "import { helper } from \"./lib\"\n\nfn main(harness: Harness) {}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("helper").unwrap();
    let import_span = harn_lexer::Span::with_offsets(start, start + "helper".len(), 1, 10);

    assert!(
        synthesize_missing_capability_argument_repair(
            import_span,
            &named_type("HarnessAst"),
            &named_type("string"),
            source,
            &program,
        )
        .is_none(),
        "an imported-declaration diagnostic must never edit the importer"
    );
}

#[test]
fn missing_root_argument_repair_preserves_parenthesized_first_argument() {
    // An expression span excludes its grouping parentheses, so a repair that
    // inserted at the diagnosed offset would produce `((harness, params ...`.
    let source = "pipeline test(harness: Harness, params: dict) {\n  host_search_request((params ?? {}) + {path: \"root\"})\n}\n";
    let program = harn_parser::parse_source(source).expect("source parses");
    let mut span = None;
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == "host_search_request" {
                span = args.first().map(|arg| arg.span);
            }
        }
    });
    let span = span.expect("host_search_request first argument");

    let (_, edits, _) = synthesize_missing_root_argument_repair(
        span,
        source,
        &program,
        &BTreeSet::new(),
        &AmbientRepairContext {
            cross_module_importer_count: 0,
        },
        &mut ValueEscape {
            manifest_handlers: &BTreeSet::new(),
            referenced_by_value: &BTreeSet::new(),
            escape_sites: &BTreeMap::new(),
            frozen: &mut Vec::new(),
        },
    )
    .expect("root argument repair");

    let applied = FixEdit::apply_all(source, &edits);
    assert!(
        applied.contains("host_search_request(harness, (params ?? {}) + {path: \"root\"})"),
        "capability must be inserted at the call boundary: {applied}"
    );
    harn_parser::parse_source(&applied).expect("repair must remain parse-safe");
}

/// A read of a local binding is not a value read of a same-named callable.
///
/// The set is collected across the whole program so a registry in one module
/// can freeze a handler defined in another. That reach is what makes the
/// distinction load-bearing: without it an ordinary `const repo_root = ...`
/// anywhere in the corpus freezes every `repo_root` helper in it.
#[test]
fn value_references_skip_locally_bound_names() {
    let source = concat!(
        "fn main(harness: Harness) {\n",
        "  const repo_root = discover()\n",
        "  let shell_argv = false\n",
        "  const {alias: renamed} = config()\n",
        "  const [first] = items()\n",
        "  print(repo_root, shell_argv, renamed, first)\n",
        "}\n",
    );
    let program = harn_parser::parse_source(source).unwrap();
    let names = super::signature_threading::collect_value_reference_sites(&program)
        .into_iter()
        .map(|site| site.name)
        .collect::<BTreeSet<_>>();
    for local in ["repo_root", "shell_argv", "renamed", "first"] {
        assert!(!names.contains(local), "`{local}` is a local binding");
    }
}

/// A genuine first-class read still freezes: this is the case the whole
/// mechanism exists for, and the local-binding filter must not weaken it.
#[test]
fn value_references_still_catch_a_bare_handler_reference() {
    let source = concat!(
        "fn handler(args: dict) -> string {\n",
        "  return \"\"\n",
        "}\n",
        "fn main(harness: Harness) {\n",
        "  const registry = {handler: handler}\n",
        "  use_it(registry)\n",
        "}\n",
    );
    let program = harn_parser::parse_source(source).unwrap();
    let names = super::signature_threading::collect_value_reference_sites(&program)
        .into_iter()
        .map(|site| site.name)
        .collect::<BTreeSet<_>>();
    assert!(
        names.contains("handler"),
        "a bare reference must still freeze"
    );
}
