//! How capability repairs compose at one call site, and which callables the
//! migration is allowed to touch at all.

use super::super::*;
use std::fs;

/// A call missing several capability carriers draws one repair per missing
/// argument and one whole-program repair supplying all of them, so the same
/// offset receives both `harness.env, ` and `harness.env, harness.fs, `. Those
/// are the same carriers in the same parameter order, and rejecting them as
/// ambiguous aborts the whole pass — one multi-carrier callee then blocks the
/// migration of every other file in the tree.
#[test]
fn a_multi_carrier_call_keeps_the_complete_prepend_over_its_prefix() {
    let insertion = |replacement: &str| FixEditWire {
        span: SpanWire {
            start: 46,
            end: 46,
            line: 2,
            column: 10,
            end_line: 2,
        },
        replacement: replacement.to_string(),
    };
    let collapsed = dedupe_wire_edits(&[
        insertion("harness.env, "),
        insertion("harness.env, harness.fs, "),
    ]);

    assert_eq!(
        collapsed
            .iter()
            .map(|edit| edit.replacement.as_str())
            .collect::<Vec<_>>(),
        vec!["harness.env, harness.fs, "],
        "the prefix insertion is subsumed, not a competing candidate"
    );
}

/// Two insertions at one offset that are not in a prefix relation really are
/// competing fixes, and must still reject rather than silently pick one.
#[test]
fn a_genuinely_ambiguous_insertion_pair_still_rejects() {
    let insertion = |replacement: &str| FixEditWire {
        span: SpanWire {
            start: 12,
            end: 12,
            line: 1,
            column: 13,
            end_line: 1,
        },
        replacement: replacement.to_string(),
    };
    let collapsed = dedupe_wire_edits(&[insertion("harness.fs, "), insertion("harness.env, ")]);

    assert_eq!(collapsed.len(), 2, "neither replacement subsumes the other");
    apply::validate_edit_composition(Path::new("call.harn"), &collapsed)
        .expect_err("competing carriers at one offset stay ambiguous");
}

/// A registry stores `handler: some_handler` and later calls `handler(args)`
/// through that reference, so adding a capability parameter to the callable
/// breaks the call — and leaves no static call site for the type checker to
/// reject. The two programs below differ only in whether the callable's value
/// escapes.
#[test]
fn a_callable_whose_value_escapes_keeps_its_signature() {
    fn threads_harness(source: &str, name: &str) -> bool {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join(name);
        fs::write(&script, source).unwrap();
        build_plan(&script, Some(RepairSafety::SurfaceChanging))
            .unwrap()
            .repairs
            .iter()
            .any(|repair| repair.repair.id == "bindings/thread-harness-whole-program")
    }

    let escaping = "fn read_it(path: string) -> string {\n  return fs_read_text(path)\n}\n\nfn bindings() -> list {\n  return [{name: \"read\", handler: read_it}]\n}\n\npub fn dispatch(name: string, arg: string) -> string {\n  for binding in bindings() {\n    if binding?.name == name {\n      const handler = binding?.handler\n      return handler(arg)\n    }\n  }\n  return \"\"\n}\n";
    let direct = "fn read_it(path: string) -> string {\n  return fs_read_text(path)\n}\n\npub fn dispatch(name: string, arg: string) -> string {\n  if name == \"read\" {\n    return read_it(arg)\n  }\n  return \"\"\n}\n";

    assert!(
        threads_harness(direct, "direct.harn"),
        "a directly-called helper is still migrated"
    );
    assert!(
        !threads_harness(escaping, "escaping.harn"),
        "a helper whose value is stored in a registry must keep its signature"
    );
}
