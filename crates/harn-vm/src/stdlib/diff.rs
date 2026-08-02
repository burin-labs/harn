//! Native projection of the workspace line-diff engine for `std/diff`.

use std::collections::BTreeMap;
use std::sync::Arc;

use arcstr::ArcStr;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::text_diff::{compute_line_diff, LineDiffOptions, DEFAULT_CONTEXT};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_diff_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&DIFF_LINE_ARTIFACT_IMPL_DEF];

fn string_arg<'a>(args: &'a [VmValue], index: usize, name: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.as_str()),
        Some(value) => Err(VmError::TypeError(format!(
            "__diff_line_artifact: `{name}` must be a string, got {}",
            value.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "__diff_line_artifact: `{name}` is required"
        ))),
    }
}

fn option_bool(options: Option<&crate::value::DictMap>, key: &str, default: bool) -> bool {
    match options.and_then(|values| values.get(key)) {
        Some(VmValue::Bool(value)) => *value,
        _ => default,
    }
}

fn option_context(options: Option<&crate::value::DictMap>, input_bytes: usize) -> usize {
    match options.and_then(|values| values.get("context")) {
        Some(VmValue::Int(value)) if *value < 0 => input_bytes,
        Some(VmValue::Int(value)) => usize::try_from(*value).unwrap_or(input_bytes),
        _ => DEFAULT_CONTEXT,
    }
}

fn count_value(value: usize) -> VmValue {
    VmValue::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__diff_line_artifact(before: string, after: string, options?: dict) -> dict",
    category = "diff"
)]
fn diff_line_artifact_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let before = string_arg(args, 0, "before")?;
    let after = string_arg(args, 1, "after")?;
    let options = args.get(2).and_then(VmValue::as_dict);
    let include_body = option_bool(options, "include_body", true);
    let include_changes = option_bool(options, "include_ops", true);
    let context = option_context(options, before.len().saturating_add(after.len()));
    let diff = compute_line_diff(
        before,
        after,
        LineDiffOptions {
            context,
            include_body,
            include_changes,
        },
    );
    let changes = diff
        .changes
        .into_iter()
        .map(|change| {
            VmValue::dict(BTreeMap::from([
                (
                    "kind".to_owned(),
                    VmValue::String(ArcStr::from(change.kind.as_str())),
                ),
                (
                    "line".to_owned(),
                    VmValue::String(ArcStr::from(change.line)),
                ),
                ("old_line".to_owned(), count_value(change.old_line)),
                ("new_line".to_owned(), count_value(change.new_line)),
            ]))
        })
        .collect();
    Ok(VmValue::dict(BTreeMap::from([
        (
            "changed".to_owned(),
            VmValue::Bool(diff.lines_added > 0 || diff.lines_removed > 0),
        ),
        ("insertions".to_owned(), count_value(diff.lines_added)),
        ("deletions".to_owned(), count_value(diff.lines_removed)),
        ("old_lines".to_owned(), count_value(diff.old_lines)),
        ("new_lines".to_owned(), count_value(diff.new_lines)),
        ("body".to_owned(), VmValue::String(ArcStr::from(diff.body))),
        ("ops".to_owned(), VmValue::List(Arc::new(changes))),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_projects_counts_body_and_optional_ops() {
        let result = diff_line_artifact_impl(
            &[
                VmValue::String(ArcStr::from("a\nb\nc\n")),
                VmValue::String(ArcStr::from("a\nB\nc\n")),
                VmValue::dict(BTreeMap::from([(
                    "include_ops".to_owned(),
                    VmValue::Bool(false),
                )])),
            ],
            &mut String::new(),
        )
        .expect("diff succeeds");
        let result = result.as_dict().expect("dict result");
        assert!(matches!(result.get("insertions"), Some(VmValue::Int(1))));
        assert!(matches!(result.get("deletions"), Some(VmValue::Int(1))));
        assert!(matches!(result.get("ops"), Some(VmValue::List(ops)) if ops.is_empty()));
        assert!(matches!(result.get("body"), Some(VmValue::String(body)) if body.contains("-b\n")));
    }
}
