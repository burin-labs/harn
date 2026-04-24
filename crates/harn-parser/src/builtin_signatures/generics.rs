use crate::ast::{ShapeField, TypeExpr};

use super::BuiltinGenericSig;

/// Generic signature for `name`, if any. Returns `None` for builtins whose
/// return type is fully described by [`super::builtin_return_type`].
pub(crate) fn lookup_generic_builtin_sig(name: &str) -> Option<BuiltinGenericSig> {
    match name {
        "ask_user" => Some(ask_user_generic_sig()),
        "dual_control" => Some(dual_control_generic_sig()),
        "escalate_to" => Some(escalate_to_builtin_sig()),
        "hitl_pending" => Some(hitl_pending_builtin_sig()),
        "llm_call" | "llm_completion" => Some(llm_call_generic_sig()),
        "llm_call_structured" => Some(llm_call_structured_generic_sig()),
        "project_fingerprint" => Some(project_fingerprint_builtin_sig()),
        "request_approval" => Some(request_approval_builtin_sig()),
        "schema_parse" | "schema_check" => Some(schema_parse_generic_sig()),
        "schema_expect" => Some(schema_expect_generic_sig()),
        "handler_context" => Some(handler_context_builtin_sig()),
        "trust_graph_policy_for" => Some(trust_graph_policy_for_builtin_sig()),
        "trust_graph_query" => Some(trust_graph_query_builtin_sig()),
        "trust_graph_record" => Some(trust_graph_record_builtin_sig()),
        "trust_graph_verify_chain" => Some(trust_graph_verify_chain_builtin_sig()),
        "trust_query" => Some(trust_query_builtin_sig()),
        "trust_record" => Some(trust_record_builtin_sig()),
        "trigger_fire" => Some(trigger_fire_builtin_sig()),
        "trigger_inspect_action_graph" => Some(trigger_inspect_action_graph_builtin_sig()),
        "trigger_inspect_dlq" => Some(trigger_inspect_dlq_builtin_sig()),
        "trigger_inspect_lifecycle" => Some(trigger_inspect_lifecycle_builtin_sig()),
        "trigger_list" => Some(trigger_list_builtin_sig()),
        "trigger_register" => Some(trigger_register_builtin_sig()),
        "trigger_replay" => Some(trigger_replay_builtin_sig()),
        _ => None,
    }
}

fn ask_user_generic_sig() -> BuiltinGenericSig {
    let options_shape = TypeExpr::Shape(vec![
        ShapeField {
            name: "schema".into(),
            type_expr: schema_of_t(),
            optional: true,
        },
        ShapeField {
            name: "timeout".into(),
            type_expr: TypeExpr::Named("duration".into()),
            optional: true,
        },
        ShapeField {
            name: "default".into(),
            type_expr: TypeExpr::Named("T".into()),
            optional: true,
        },
    ]);
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![TypeExpr::Named("string".into()), options_shape],
        return_type: TypeExpr::Named("T".into()),
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

fn llm_call_structured_generic_sig() -> BuiltinGenericSig {
    // `llm_call_structured(prompt, schema, options?)` returns the
    // validated data directly. When the schema argument is a `Schema<T>`
    // (inline dict literal or type alias), T flows into the return type
    // so callers can dot-walk the response with full narrowing. No
    // `.data` unwrap needed — the helper is sugar for the .data
    // projection of `llm_call`.
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![
            TypeExpr::Named("string".into()),
            schema_of_t(),
            TypeExpr::Union(vec![
                TypeExpr::Named("dict".into()),
                TypeExpr::Named("nil".into()),
            ]),
        ],
        return_type: TypeExpr::Named("T".into()),
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

fn project_fingerprint_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("string".into())],
        return_type: TypeExpr::Shape(vec![
            ShapeField {
                name: "primary_language".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "languages".into(),
                type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                optional: false,
            },
            ShapeField {
                name: "frameworks".into(),
                type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                optional: false,
            },
            ShapeField {
                name: "package_manager".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
            ShapeField {
                name: "package_managers".into(),
                type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                optional: false,
            },
            ShapeField {
                name: "test_runner".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
            ShapeField {
                name: "build_tool".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
            ShapeField {
                name: "vcs".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
            ShapeField {
                name: "ci".into(),
                type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                optional: false,
            },
            ShapeField {
                name: "has_tests".into(),
                type_expr: TypeExpr::Named("bool".into()),
                optional: false,
            },
            ShapeField {
                name: "has_ci".into(),
                type_expr: TypeExpr::Named("bool".into()),
                optional: false,
            },
            ShapeField {
                name: "lockfile_paths".into(),
                type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                optional: false,
            },
        ]),
    }
}

fn trigger_list_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("TriggerBinding".into()))),
    }
}

fn dual_control_generic_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec!["T".into()],
        params: vec![
            TypeExpr::Named("int".into()),
            TypeExpr::Named("int".into()),
            TypeExpr::FnType {
                params: Vec::new(),
                return_type: Box::new(TypeExpr::Named("T".into())),
            },
            TypeExpr::Union(vec![
                TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
                TypeExpr::Named("nil".into()),
            ]),
        ],
        return_type: TypeExpr::Named("T".into()),
    }
}

fn request_approval_builtin_sig() -> BuiltinGenericSig {
    let options_shape = TypeExpr::Shape(vec![
        ShapeField {
            name: "detail".into(),
            type_expr: TypeExpr::Named("any".into()),
            optional: true,
        },
        ShapeField {
            name: "quorum".into(),
            type_expr: TypeExpr::Named("int".into()),
            optional: true,
        },
        ShapeField {
            name: "reviewers".into(),
            type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
            optional: true,
        },
        ShapeField {
            name: "deadline".into(),
            type_expr: TypeExpr::Named("duration".into()),
            optional: true,
        },
    ]);
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("string".into()), options_shape],
        return_type: approval_record_type(),
    }
}

fn approval_record_type() -> TypeExpr {
    TypeExpr::Shape(vec![
        ShapeField {
            name: "approved".into(),
            type_expr: TypeExpr::Named("bool".into()),
            optional: false,
        },
        ShapeField {
            name: "reviewers".into(),
            type_expr: TypeExpr::List(Box::new(TypeExpr::Named("string".into()))),
            optional: false,
        },
        ShapeField {
            name: "approved_at".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "reason".into(),
            type_expr: TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
            optional: false,
        },
        ShapeField {
            name: "signatures".into(),
            type_expr: TypeExpr::List(Box::new(approval_signature_type())),
            optional: false,
        },
    ])
}

fn approval_signature_type() -> TypeExpr {
    TypeExpr::Shape(vec![
        ShapeField {
            name: "reviewer".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "signed_at".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
        ShapeField {
            name: "signature".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: false,
        },
    ])
}

fn escalate_to_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("string".into()),
        ],
        return_type: TypeExpr::Shape(vec![
            ShapeField {
                name: "request_id".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "role".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "reason".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "trace_id".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "status".into(),
                type_expr: TypeExpr::Named("string".into()),
                optional: false,
            },
            ShapeField {
                name: "accepted_at".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
            ShapeField {
                name: "reviewer".into(),
                type_expr: TypeExpr::Union(vec![
                    TypeExpr::Named("string".into()),
                    TypeExpr::Named("nil".into()),
                ]),
                optional: false,
            },
        ]),
    }
}

fn hitl_pending_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Union(vec![
            hitl_pending_filters_type(),
            TypeExpr::Named("nil".into()),
        ])],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("HitlPendingRequest".into()))),
    }
}

fn hitl_pending_filters_type() -> TypeExpr {
    TypeExpr::Shape(vec![
        ShapeField {
            name: "since".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
        ShapeField {
            name: "until".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
        ShapeField {
            name: "kinds".into(),
            type_expr: TypeExpr::List(Box::new(TypeExpr::Named("HitlRequestKind".into()))),
            optional: true,
        },
        ShapeField {
            name: "agent".into(),
            type_expr: TypeExpr::Named("string".into()),
            optional: true,
        },
        ShapeField {
            name: "limit".into(),
            type_expr: TypeExpr::Named("int".into()),
            optional: true,
        },
    ])
}

fn handler_context_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![],
        return_type: TypeExpr::Union(vec![
            TypeExpr::Named("HandlerContext".into()),
            TypeExpr::Named("nil".into()),
        ]),
    }
}

fn trust_record_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("string".into()),
            TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
            TypeExpr::Named("TrustOutcome".into()),
            TypeExpr::Named("AutonomyTier".into()),
        ],
        return_type: TypeExpr::Named("TrustRecord".into()),
    }
}

fn trust_query_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Union(vec![
            TypeExpr::Named("TrustQueryFilters".into()),
            TypeExpr::Named("nil".into()),
        ])],
        return_type: TypeExpr::Named("list".into()),
    }
}

fn trust_graph_record_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("dict".into())],
        return_type: TypeExpr::Named("TrustEntryId".into()),
    }
}

fn trust_graph_query_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
        ],
        return_type: TypeExpr::Named("TrustScore".into()),
    }
}

fn trust_graph_policy_for_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Named("string".into())],
        return_type: TypeExpr::Named("CapabilityPolicy".into()),
    }
}

fn trust_graph_verify_chain_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![],
        return_type: TypeExpr::Named("TrustChainReport".into()),
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

fn trigger_inspect_lifecycle_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("nil".into()),
        ])],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("dict".into()))),
    }
}

fn trigger_inspect_action_graph_builtin_sig() -> BuiltinGenericSig {
    BuiltinGenericSig {
        type_params: vec![],
        params: vec![TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("nil".into()),
        ])],
        return_type: TypeExpr::List(Box::new(TypeExpr::Named("TriggerActionGraphEvent".into()))),
    }
}
