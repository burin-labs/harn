use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_command_plan_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "__command_sequence_plan(source: string, cwd: string) -> {status: \"measured\" | \"unmeasured\", steps: list<{argv: list<string>, cwd: string}>}",
    category = "runtime",
    doc = "Internal structural POSIX sequence observation for std/command. Does not execute commands."
)]
fn command_sequence_plan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = args.first().map(VmValue::as_str_cow);
    let cwd = args.get(1).map(VmValue::as_str_cow);
    let plan = source
        .zip(cwd)
        .and_then(|(source, cwd)| crate::shells::plan_posix_sequence(&source, &cwd));
    let mut result = crate::value::DictMap::new();
    result.put_str(
        "status",
        if plan.is_some() {
            "measured"
        } else {
            "unmeasured"
        },
    );
    let steps = plan
        .unwrap_or_default()
        .into_iter()
        .map(|step| {
            let mut record = crate::value::DictMap::new();
            record.put("argv", VmValue::strings(step.argv.iter()));
            record.put_str("cwd", &step.cwd);
            VmValue::dict(record)
        })
        .collect();
    result.put("steps", VmValue::list(steps));
    Ok(VmValue::dict(result))
}

#[harn_builtin(
    exposure = "pure",
    sig = "__shell_command_shape(source: string, shell: string) -> {status: \"measured\" | \"unmeasured\", dialect: string, segments: list<{operator: string, stages: list<{kind: \"command\" | \"compound\", text: string, words: list<string>}>}>}",
    category = "runtime",
    doc = "Internal structural shell-line observation for std/command. Does not execute commands."
)]
fn shell_command_shape_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = args.first().map(VmValue::as_str_cow).unwrap_or_default();
    let shell = args.get(1).map(VmValue::as_str_cow).unwrap_or_default();
    let dialect = crate::shells::shell_dialect_for_id(&shell);
    let segments = dialect
        .and_then(|dialect| crate::orchestration::shell_command_shape(dialect, &source).segments);
    let mut result = crate::value::DictMap::new();
    result.put_str(
        "status",
        if segments.is_some() {
            "measured"
        } else {
            "unmeasured"
        },
    );
    result.put_str("dialect", dialect.map_or("", |dialect| dialect.as_str()));
    let segments = segments
        .unwrap_or_default()
        .into_iter()
        .map(|segment| {
            let stages = segment
                .stages
                .into_iter()
                .map(|stage| {
                    let mut record = crate::value::DictMap::new();
                    record.put_str("kind", stage.kind);
                    record.put_str("text", &stage.text);
                    record.put("words", VmValue::strings(stage.words.iter()));
                    VmValue::dict(record)
                })
                .collect();
            let mut record = crate::value::DictMap::new();
            record.put_str("operator", segment.operator);
            record.put("stages", VmValue::list(stages));
            VmValue::dict(record)
        })
        .collect();
    result.put("segments", VmValue::list(segments));
    Ok(VmValue::dict(result))
}
