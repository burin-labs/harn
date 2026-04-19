use crate::ast::{ShapeField, TypeExpr};

use super::BuiltinGenericSig;

/// Generic signature for `name`, if any. Returns `None` for builtins whose
/// return type is fully described by [`super::builtin_return_type`].
pub(crate) fn lookup_generic_builtin_sig(name: &str) -> Option<BuiltinGenericSig> {
    match name {
        "llm_call" | "llm_completion" => Some(llm_call_generic_sig()),
        "schema_parse" | "schema_check" => Some(schema_parse_generic_sig()),
        "schema_expect" => Some(schema_expect_generic_sig()),
        "trigger_fire" => Some(trigger_fire_builtin_sig()),
        "trigger_inspect_dlq" => Some(trigger_inspect_dlq_builtin_sig()),
        "trigger_list" => Some(trigger_list_builtin_sig()),
        "trigger_register" => Some(trigger_register_builtin_sig()),
        "trigger_replay" => Some(trigger_replay_builtin_sig()),
        _ => None,
    }
}

fn schema_of_t() -> TypeExpr {
    TypeExpr::Applied {
        name: "Schema".into(),
        args: vec![TypeExpr::Named("T".into())],
    }
}

fn llm_call_generic_sig() -> BuiltinGenericSig {
    // options param is modeled as a shape with `output_schema: Schema<T>`
    // so that `bind_from_arg_node` can pull T out of the dict literal's
    // `output_schema:` entry. Other option keys are not modeled here; they
    // participate in the ordinary args-typechecking loop (which is
    // permissive for dict-typed params).
    let options_shape = TypeExpr::Shape(vec![ShapeField {
        name: "output_schema".into(),
        type_expr: schema_of_t(),
        optional: true,
    }]);
    let return_shape = TypeExpr::Shape(vec![
        ShapeField {
            name: "text".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "model".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "provider".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "input_tokens".into(),
            type_expr: TypeExpr::Named("int".into()),
            optional: false,
        },
        ShapeField {
            name: "output_tokens".into(),
            type_expr: TypeExpr::Named("int".into()),
            optional: false,
        },
        ShapeField {
            name: "data".into(),
            type_expr: TypeExpr::Named("T".into()),
            optional: false,
        },
        ShapeField {
            name: "visible_text".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
        ShapeField {
            name: "tool_calls".into(),
            type_expr: TypeExpr::Named("list".into()),
            optional: true,
        },
        ShapeField {
            name: "thinking".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
        ShapeField {
            name: "stop_reason".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
    ]);
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
            options_shape,
        ],
        return_type: return_shape,
    }
}

fn schema_parse_generic_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![TypeExpr::Named("unknown".into()), schema_of_t()],
        return_type: TypeExpr::Applied {
            name: "Result".into(),
            args: vec![
                TypeExpr::Named("T".into()),
                TypeExpr::Named("string".into()),
            ],
        },
    }
}

fn schema_expect_generic_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![TypeExpr::Named("unknown".into()), schema_of_t()],
        return_type: TypeExpr::Named("T".into()),
    }
}

fn trigger_list_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("TriggerBinding".into()))),
    }
}

fn trigger_register_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("TriggerConfig".into())],
        return_type: TypeExpr::Named("TriggerHandle".into()),
    }
}

fn trigger_fire_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![
            TypeExpr::Named("TriggerHandle".into()),
            TypeExpr::Named("TriggerEvent".into()),
        ],
        return_type: TypeExpr::Named("DispatchHandle".into()),
    }
}

fn trigger_replay_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("string".into())],
        return_type: TypeExpr::Named("DispatchHandle".into()),
    }
}

fn trigger_inspect_dlq_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("DlqEntry".into()))),
    }
}
