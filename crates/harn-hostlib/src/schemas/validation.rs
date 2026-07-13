use std::sync::OnceLock;

use harn_vm::{VmDictExt, VmValue};

use crate::error::HostlibError;

use super::{SchemaKind, SCHEMAS};

struct CompiledSchema {
    module: &'static str,
    method: &'static str,
    kind: SchemaKind,
    body: Result<VmValue, String>,
}

fn compiled_schemas() -> &'static [CompiledSchema] {
    static COMPILED: OnceLock<Vec<CompiledSchema>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        SCHEMAS
            .iter()
            .map(|(module, method, kind, body)| {
                let body = serde_json::from_str::<serde_json::Value>(body)
                    .map_err(|err| format!("schema is not valid JSON: {err}"))
                    .map(|json| harn_vm::json_to_vm_value(&json))
                    .and_then(|schema| harn_vm::schema::canonicalize_json_schema(&schema));
                CompiledSchema {
                    module,
                    method,
                    kind: *kind,
                    body,
                }
            })
            .collect()
    })
}

fn compiled_schema(
    module: &str,
    method: &str,
    kind: SchemaKind,
) -> Option<&'static CompiledSchema> {
    compiled_schemas()
        .iter()
        .find(|schema| schema.module == module && schema.method == method && schema.kind == kind)
}

pub(crate) fn validate_request_args(
    builtin: &'static str,
    module: &'static str,
    method: &'static str,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let request = normalize_request_arg(builtin, module, method, args)?;
    let schema = compiled_schema(module, method, SchemaKind::Request).ok_or_else(|| {
        HostlibError::Backend {
            builtin,
            message: format!("missing request schema for {module}.{method}"),
        }
    })?;
    let schema = schema
        .body
        .as_ref()
        .map_err(|message| HostlibError::Backend {
            builtin,
            message: format!("invalid request schema for {module}.{method}: {message}"),
        })?;
    harn_vm::schema::validate_value_against_canonical_schema(&request, schema, true).map_err(
        |message| HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message,
        },
    )
}

pub(crate) fn validate_response(
    builtin: &'static str,
    module: &'static str,
    method: &'static str,
    response: VmValue,
) -> Result<VmValue, HostlibError> {
    let schema = compiled_schema(module, method, SchemaKind::Response).ok_or_else(|| {
        HostlibError::Backend {
            builtin,
            message: format!("missing response schema for {module}.{method}"),
        }
    })?;
    let schema = schema
        .body
        .as_ref()
        .map_err(|message| HostlibError::Backend {
            builtin,
            message: format!("invalid response schema for {module}.{method}: {message}"),
        })?;
    harn_vm::schema::validate_value_against_canonical_schema(&response, schema, true).map_err(
        |message| HostlibError::Backend {
            builtin,
            message: format!("response schema violation for {module}.{method}: {message}"),
        },
    )
}

fn normalize_request_arg(
    builtin: &'static str,
    module: &'static str,
    method: &'static str,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    if args.len() > 1 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message: format!("expected exactly one request argument, got {}", args.len()),
        });
    }

    let first = args.first().ok_or(HostlibError::MissingParameter {
        builtin,
        param: "request",
    })?;
    match first {
        VmValue::Dict(map) => Ok(prune_top_level_nil_dict_fields(map)),
        VmValue::String(feature) if (module, method) == ("tools", "enable") => {
            let mut normalized = harn_vm::value::DictMap::new();
            normalized.put_str("feature", feature.to_string());
            Ok(VmValue::dict(normalized))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message: format!("expected a dict request body, got {}", other.type_name()),
        }),
    }
}

fn prune_top_level_nil_dict_fields(map: &harn_vm::value::DictMap) -> VmValue {
    let mut pruned = harn_vm::value::DictMap::new();
    for (key, child) in map.iter() {
        if matches!(child, VmValue::Nil) {
            continue;
        }
        pruned.insert(key.clone(), child.clone());
    }
    VmValue::dict_map(pruned)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harn_vm::VmValue;

    use super::*;

    #[test]
    fn request_validation_prunes_nil_optional_fields() {
        let request = VmValue::dict([
            ("session_id", VmValue::string("session-1")),
            ("scope_id", VmValue::string("scope-1")),
            ("root", VmValue::Nil),
        ]);

        let validated = validate_request_args("hostlib_fs_snapshot", "fs", "snapshot", &[request])
            .expect("nil optional root should be omitted before schema validation");
        let fields = validated.as_dict().expect("validated request is a dict");
        assert!(fields.get("root").is_none());
        assert_eq!(
            fields.get("session_id").map(VmValue::display),
            Some("session-1".to_string())
        );
    }

    #[test]
    fn run_command_request_schema_accepts_env_remove() {
        let request = VmValue::dict([
            (
                "argv",
                VmValue::List(Arc::new(vec![VmValue::string("env")])),
            ),
            (
                "env_remove",
                VmValue::List(Arc::new(vec![VmValue::string("HARN_EVENT_LOG_DIR")])),
            ),
        ]);

        validate_request_args(
            "hostlib_tools_run_command",
            "tools",
            "run_command",
            &[request],
        )
        .expect("env_remove must be a valid run_command request field");
    }

    #[test]
    fn response_validation_rejects_incomplete_run_command_result() {
        let response = VmValue::dict([("status", VmValue::string("completed"))]);

        let err = validate_response(
            "hostlib_tools_run_command",
            "tools",
            "run_command",
            response,
        )
        .expect_err("producer responses must satisfy the public response contract");

        assert!(matches!(err, HostlibError::Backend { .. }));
    }

    #[test]
    fn request_validation_does_not_prune_nested_nil_fields() {
        let request = VmValue::dict([
            (
                "argv",
                VmValue::List(Arc::new(vec![VmValue::string("env")])),
            ),
            ("env", VmValue::dict([("FOO", VmValue::Nil)])),
        ]);

        let err = validate_request_args(
            "hostlib_tools_run_command",
            "tools",
            "run_command",
            &[request],
        )
        .expect_err("nested nil map values must remain visible to schema validation");
        match err {
            HostlibError::InvalidParameter { message, .. } => {
                assert!(
                    message.contains("env") && message.contains("string"),
                    "nested env nil should fail as a non-string value, got: {message}"
                );
            }
            other => panic!("expected request validation error, got {other:?}"),
        }
    }

    #[test]
    fn dry_run_request_schema_allows_handler_rejected_plan_ops() {
        let unknown = VmValue::dict([
            ("op", VmValue::string("blow_up_the_world")),
            ("path", VmValue::string("multi.txt")),
        ]);
        let missing_op = VmValue::dict_map(harn_vm::value::DictMap::new());
        let non_string_op = VmValue::dict([("op", VmValue::Int(1))]);
        let request = VmValue::dict([(
            "plan",
            VmValue::List(Arc::new(vec![unknown, missing_op, non_string_op])),
        )]);

        validate_request_args("hostlib_ast_dry_run", "ast", "dry_run", &[request]).expect(
            "dry_run unknown/missing/non-string ops must reach the handler for structured rejection",
        );
    }

    #[test]
    fn extension_hostlib_rules_search_has_request_schema() {
        let request = VmValue::dict([
            (
                "rule",
                VmValue::string(
                    "id = \"noop\"\nlanguage = \"typescript\"\n[rule]\npattern = \"$X\"",
                ),
            ),
            ("source", VmValue::string("foo();")),
            ("language", VmValue::string("typescript")),
        ]);

        validate_request_args("hostlib_rules_search", "rules", "search", &[request])
            .expect("rules.search should be covered by the shared hostlib schema catalog");
    }
}
