//! Keep the VM's builtin-method dispatch and the method registry
//! (`harn_parser::builtin_methods`) one surface. Every registry row must be
//! handled by the matching VM dispatch (pure rows by the plain dispatch,
//! mutating rows by `apply_mutating_method`), and every name a VM dispatch
//! handles must have a registry row with the same effect. Either kind of
//! drift fails here: a missing row would make `harn check` reject a call the
//! runtime supports, and a stale row would let a crashing call pass.

use std::collections::BTreeSet;
use std::sync::Arc;

use harn_parser::builtin_methods::{self as methods, MethodReceiver};

use crate::value::{VmError, VmValue};

fn sample_args() -> Vec<VmValue> {
    // Enough positional args to satisfy guarded arms such as
    // `"replace" if args.len() >= 2`.
    vec![VmValue::string("a"), VmValue::string("b"), VmValue::Int(1)]
}

fn rejects(result: &Option<Result<VmValue, VmError>>) -> bool {
    matches!(result, Some(Err(e)) if format!("{e:?}").contains("has no method"))
}

fn pure_names(receiver: MethodReceiver) -> Vec<&'static str> {
    methods::methods_of(receiver)
        .filter(|row| !row.is_mutating())
        .map(|row| row.name())
        // `iter` is dispatched for every iterable in `call_method_sync`.
        .filter(|name| *name != "iter")
        .collect()
}

fn mutating_names(receiver: MethodReceiver) -> BTreeSet<&'static str> {
    methods::methods_of(receiver)
        .filter(|row| row.is_mutating())
        .map(|row| row.name())
        .collect()
}

#[test]
fn every_pure_registry_row_is_handled_by_the_plain_dispatch() {
    let args = sample_args();
    let string = arcstr::ArcStr::from("hello");
    for name in pure_names(MethodReceiver::String) {
        let result = Some(crate::vm::Vm::call_string_method(&string, name, &args));
        assert!(
            !rejects(&result),
            "string.{name} is in the registry but the VM rejects it"
        );
    }
    // `first`/`last` take no arguments; everything else tolerates the
    // sample arguments.
    let items: Arc<Vec<VmValue>> = Arc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
    for name in pure_names(MethodReceiver::List) {
        let args = if matches!(name, "first" | "last") {
            Vec::new()
        } else {
            args.clone()
        };
        let result = crate::vm::Vm::call_list_method_sync(&items, name, &args);
        assert!(
            !rejects(&result),
            "list.{name} is in the registry but the VM rejects it"
        );
    }
    let VmValue::Set(set) = VmValue::set([VmValue::Int(1), VmValue::Int(2)]) else {
        unreachable!("VmValue::set builds a Set");
    };
    let set_args = vec![VmValue::set([VmValue::Int(3)]), VmValue::string("a")];
    for name in pure_names(MethodReceiver::Set) {
        let result = crate::vm::Vm::call_set_method_sync(&set, name, &set_args);
        assert!(
            !rejects(&result),
            "set.{name} is in the registry but the VM rejects it"
        );
    }
    let VmValue::Dict(map) = VmValue::dict([("k", VmValue::Int(1))]) else {
        unreachable!("VmValue::dict builds a Dict");
    };
    for name in pure_names(MethodReceiver::Dict) {
        let result = crate::vm::Vm::call_dict_method_sync(&map, name, &args);
        assert!(
            !rejects(&result),
            "dict.{name} is in the registry but the VM rejects it"
        );
    }
    let range = VmValue::Range(Arc::new(crate::value::VmRange {
        start: 1,
        end: 3,
        inclusive: true,
    }));
    let VmValue::Range(r) = &range else {
        unreachable!("built a range");
    };
    for name in pure_names(MethodReceiver::Range) {
        let result = crate::vm::Vm::call_range_method_sync(&range, r, name, &args);
        assert!(
            result.as_ref().is_some_and(Result::is_ok),
            "range.{name} is in the registry but the VM does not handle it"
        );
    }
}

#[tokio::test]
async fn every_mutating_registry_row_is_handled_by_the_mutating_dispatch() {
    let mut vm = crate::vm::Vm::new();
    let receivers = [
        (
            MethodReceiver::List,
            VmValue::List(Arc::new(vec![VmValue::Int(2), VmValue::Int(1)])),
        ),
        (MethodReceiver::Set, VmValue::set([VmValue::Int(1)])),
        (
            MethodReceiver::Dict,
            VmValue::dict([("k", VmValue::Int(1))]),
        ),
    ];
    for (receiver, value) in receivers {
        for name in mutating_names(receiver) {
            let args = match name {
                "insert" => vec![VmValue::Int(0), VmValue::Int(9)],
                "extend" => vec![VmValue::List(Arc::new(vec![VmValue::Int(3)]))],
                "sort" | "pop" | "clear" | "reverse" => Vec::new(),
                // No key call happens on the empty receiver below.
                "sort_by" => vec![VmValue::BuiltinRef("key".into())],
                _ => vec![VmValue::Int(1)],
            };
            let value = if name == "sort_by" {
                VmValue::List(Arc::new(Vec::new()))
            } else {
                value.clone()
            };
            let mutation = vm
                .apply_mutating_method(&value, name, &args)
                .await
                .unwrap_or_else(|e| panic!("{receiver:?}.{name} failed: {e:?}"));
            assert!(
                mutation.is_some(),
                "{receiver:?}.{name} is a mutating registry row the VM does not apply"
            );
        }
        let none = vm
            .apply_mutating_method(&value, "frobnicate", &[])
            .await
            .expect("unknown names are not errors");
        assert!(
            none.is_none(),
            "an unknown name must fall back to a plain call"
        );
    }
}

/// Method names in the `match method { "a" | "b" => ... }` arms of a VM
/// dispatch source, including continuation lines that start with `| "c"`.
/// Lines whose quoted names are followed by anything other than `=>`, a
/// guard, or a continuation (format args, `vec![...]` items) are ignored.
fn arm_names(source: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    for line in source.lines() {
        let mut rest = line.trim_start();
        rest = rest.strip_prefix("| ").unwrap_or(rest);
        let mut found = Vec::new();
        while let Some(after_quote) = rest.strip_prefix('"') {
            let Some((name, tail)) = after_quote.split_once('"') else {
                break;
            };
            found.push(name);
            rest = tail.trim_start();
            match rest.strip_prefix("| ") {
                Some(next) => rest = next,
                None => break,
            }
        }
        let is_arm = rest.is_empty() || rest.starts_with("=>") || rest.starts_with("if ");
        let all_idents = found.iter().all(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
        if is_arm && all_idents {
            names.extend(found);
        }
    }
    names
}

fn section<'a>(source: &'a str, from: &str, to: Option<&str>) -> &'a str {
    let start = source.find(from).expect("section start marker");
    let rest = source.get(start..).expect("section start");
    let end = to.map_or(rest.len(), |to| rest.find(to).expect("section end"));
    rest.get(..end).expect("section end")
}

#[test]
fn every_vm_dispatch_arm_has_a_registry_row_with_the_same_effect() {
    let mutating = include_str!("mutating.rs");
    let generator = include_str!("generator.rs");
    let dispatches: [(MethodReceiver, &str, bool); 10] = [
        (MethodReceiver::String, include_str!("string.rs"), false),
        (MethodReceiver::List, include_str!("list.rs"), false),
        (MethodReceiver::Set, include_str!("set.rs"), false),
        (MethodReceiver::Dict, include_str!("dict.rs"), false),
        (MethodReceiver::Range, include_str!("range.rs"), false),
        (MethodReceiver::Iter, include_str!("iter.rs"), false),
        (
            MethodReceiver::Generator,
            section(
                generator,
                "fn call_generator_method",
                Some("fn call_stream_method"),
            ),
            false,
        ),
        (
            MethodReceiver::List,
            section(mutating, "async fn mutate_list", Some("fn mutate_set")),
            true,
        ),
        (
            MethodReceiver::Set,
            section(mutating, "fn mutate_set", Some("fn mutate_dict")),
            true,
        ),
        (
            MethodReceiver::Dict,
            section(mutating, "fn mutate_dict", None),
            true,
        ),
    ];
    for (receiver, source, mutates) in dispatches {
        let names = arm_names(source);
        assert!(
            !names.is_empty(),
            "{receiver:?} dispatch scan found no arms; the scanner no longer matches the source"
        );
        for name in names {
            let row = methods::method(receiver, name).unwrap_or_else(|| {
                panic!(
                    "the VM {receiver:?} dispatch handles `{name}` but the method registry \
                     (crates/harn-parser/src/builtin_methods/table.rs) has no row for it"
                )
            });
            assert_eq!(
                row.is_mutating(),
                mutates,
                "{receiver:?}.{name}: the registry and the VM disagree on whether it mutates"
            );
        }
    }
}

#[test]
fn removed_spellings_fail_at_runtime_with_their_repair() {
    let string = arcstr::ArcStr::from("Hi");
    let err = crate::vm::Vm::call_string_method(&string, "to_lower", &[]).unwrap_err();
    assert!(format!("{err}").contains("did you mean `lower`"), "{err}");
    let items: Arc<Vec<VmValue>> = Arc::new(vec![VmValue::Int(1)]);
    let err = crate::vm::Vm::call_list_method_sync(&items, "push", &[VmValue::Int(2)])
        .expect("handled synchronously")
        .unwrap_err();
    assert!(
        format!("{err}").contains("call it on a `let` binding"),
        "{err}"
    );
    for removed in methods::REMOVED_METHODS {
        assert!(
            methods::method(removed.receiver, removed.name).is_none(),
            "{:?}.{} is both removed and live",
            removed.receiver,
            removed.name
        );
    }
}
