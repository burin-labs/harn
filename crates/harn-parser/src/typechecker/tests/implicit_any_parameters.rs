use super::{check_source_raw, DiagnosticSeverity};
use crate::diagnostic_codes::{Code, RepairSafety};

fn implicit_any_diagnostics(source: &str) -> Vec<crate::TypeDiagnostic> {
    check_source_raw(source)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == Code::ImplicitAnyParameter)
        .collect()
}

#[test]
fn every_named_callable_parameter_requires_an_annotation() {
    let source = r"
        struct Box { value: int }

        fn function_arg(value) {}
        pipeline pipeline_arg(value) {}
        gen fn generator_arg(value) -> Stream<int> { emit 1 }
        tool tool_arg(value) {}

        impl Box {
            fn method_arg(self, value) {}
        }
    ";

    let diagnostics = implicit_any_diagnostics(source);
    let messages = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 5, "{diagnostics:#?}");
    assert!(messages
        .iter()
        .any(|message| message.contains("function `function_arg` parameter `value`")));
    assert!(messages
        .iter()
        .any(|message| message.contains("pipeline `pipeline_arg` parameter `value`")));
    assert!(messages
        .iter()
        .any(|message| message.contains("generator `generator_arg` parameter `value`")));
    assert!(messages
        .iter()
        .any(|message| message.contains("tool `tool_arg` parameter `value`")));
    assert!(messages
        .iter()
        .any(|message| message.contains("method `method_arg` parameter `value`")));
    assert!(messages
        .iter()
        .all(|message| !message.contains("parameter `self`")));
}

#[test]
fn default_value_does_not_hide_an_unannotated_parameter() {
    let diagnostics = implicit_any_diagnostics("fn configure(options = nil) {}");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert!(diagnostics[0].message.contains("parameter `options`"));
}

#[test]
fn explicit_any_and_contextual_closure_parameters_remain_valid() {
    let diagnostics = implicit_any_diagnostics(
        r"
            fn explicit(value: any) {}
            fn apply(callback: fn(int) -> int) -> int { return callback(1) }
            fn caller() -> int { return apply({ value -> value + 1 }) }
        ",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn diagnostic_is_an_error_with_a_surface_changing_repair() {
    let diagnostics = implicit_any_diagnostics("fn unchecked(raw) { raw.email }");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
    let repair = diagnostic
        .repair
        .as_ref()
        .expect("repair must be registered");
    assert_eq!(repair.id.as_str(), "types/annotate-parameter");
    assert_eq!(repair.safety, RepairSafety::SurfaceChanging);
}
