use super::*;

#[test]
fn runtime_kind_guards_resolve_dictionary_union_aliases() {
    for parameter in ["bool | dict<string, unknown>", "JsonSchema", "Alias"] {
        for body in [
            r#"if type_of(value) != "dict" { return nil }
               return value.get("type")"#,
            r#"if type_of(value) == "dict" { return value.get("type") }
               return nil"#,
            r#"return match type_of(value) {
                 "dict" -> { value.get("type") }
                 _ -> { nil }
               }"#,
        ] {
            let source = format!(
                "type JsonSchema = bool | dict<string, unknown>\n\
                 type Alias = JsonSchema\n\
                 fn property(value: {parameter}) -> unknown {{ {body} }}"
            );
            let errors = errors(&source);
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        }
    }
}

#[test]
fn runtime_kind_guards_keep_boolean_and_unguarded_dictionary_calls_invalid() {
    for body in [
        r#"return value.get("type")"#,
        r#"if type_of(value) == "bool" { return value.get("type") }
           return nil"#,
        r#"if type_of(value) != "dict" { return value.get("type") }
           return nil"#,
    ] {
        let source = format!(
            "type JsonSchema = bool | dict<string, unknown>\n\
             fn property(value: JsonSchema) -> unknown {{ {body} }}"
        );
        let diagnostics = check_source(&source);
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == DiagnosticSeverity::Error
                    && diagnostic.code == Code::UnknownMethod
                    && diagnostic.message.contains("bool")
            }),
            "{source}\n{diagnostics:?}"
        );
    }
}
