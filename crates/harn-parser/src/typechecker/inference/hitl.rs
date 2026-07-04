//! Type-inference and validation for first-class HITL expressions.
//!
//! Each [`HitlKind`] is a reserved keyword form (parsed from
//! `request_approval(...)`, `dual_control(...)`, `ask_user(...)`,
//! `escalate_to(...)`). Compared with stdlib function calls, the
//! typechecker enforces:
//!
//! - Required arguments are present.
//! - Argument names (when given) belong to the kind's parameter set.
//! - Each kind returns a canonical envelope type (so the caller cannot
//!   bypass approval semantics by faking an `ApprovalRecord` —
//!   signatures are still produced by the VM at runtime, see
//!   `crates/harn-vm/src/stdlib/hitl.rs`).
//!
//! Compilation lowers HitlExpr to the matching async builtin call;
//! the runtime enforcement layer (audit log, signature minting,
//! distinct-principal quorum, replay determinism) is unchanged.

use harn_lexer::Span;

use crate::ast::{HitlArg, HitlKind, Node, ShapeField, TypeExpr};
use crate::diagnostic_codes::Code;

use super::super::scope::TypeScope;
use super::super::TypeChecker;

/// Parameter spec for a HITL primitive: every recognized arg name and
/// whether it is required. Parameter ordering also defines positional
/// fallback (e.g. `ask_user("prompt")` is positional `prompt`).
struct HitlParamSpec {
    name: &'static str,
    required: bool,
}

fn params_for(kind: HitlKind) -> &'static [HitlParamSpec] {
    match kind {
        HitlKind::AskUser => &[
            HitlParamSpec {
                name: "prompt",
                required: true,
            },
            // The remaining params come from the options dict at runtime
            // (`schema`, `timeout`, `default`); we accept them as named
            // args at the language level for ergonomics.
            HitlParamSpec {
                name: "schema",
                required: false,
            },
            HitlParamSpec {
                name: "timeout",
                required: false,
            },
            HitlParamSpec {
                name: "default",
                required: false,
            },
        ],
        HitlKind::RequestApproval => &[
            HitlParamSpec {
                name: "action",
                required: true,
            },
            HitlParamSpec {
                name: "args",
                required: false,
            },
            HitlParamSpec {
                name: "detail",
                required: false,
            },
            HitlParamSpec {
                name: "quorum",
                required: false,
            },
            HitlParamSpec {
                name: "reviewers",
                required: false,
            },
            HitlParamSpec {
                name: "deadline",
                required: false,
            },
            HitlParamSpec {
                name: "principal",
                required: false,
            },
            HitlParamSpec {
                name: "evidence_refs",
                required: false,
            },
            HitlParamSpec {
                name: "undo_metadata",
                required: false,
            },
            HitlParamSpec {
                name: "capabilities_requested",
                required: false,
            },
        ],
        HitlKind::DualControl => &[
            HitlParamSpec {
                name: "n",
                required: true,
            },
            HitlParamSpec {
                name: "m",
                required: true,
            },
            HitlParamSpec {
                name: "action",
                required: true,
            },
            HitlParamSpec {
                name: "approvers",
                required: false,
            },
        ],
        HitlKind::EscalateTo => &[
            HitlParamSpec {
                name: "role",
                required: true,
            },
            HitlParamSpec {
                name: "reason",
                required: true,
            },
        ],
    }
}

impl TypeChecker {
    pub(in crate::typechecker) fn check_hitl_expr(
        &mut self,
        kind: HitlKind,
        args: &[HitlArg],
        scope: &mut TypeScope,
        span: Span,
    ) {
        // Walk argument expressions so nested type errors surface even
        // if the call shape is wrong.
        for arg in args {
            self.check_node(&arg.value, scope);
        }

        let params = params_for(kind);
        let kw = kind.as_keyword();

        // Validate all named args reference a known parameter.
        for arg in args {
            if let Some(name) = arg.name.as_deref() {
                if !params.iter().any(|p| p.name == name) {
                    let allowed = params.iter().map(|p| p.name).collect::<Vec<_>>().join(", ");
                    self.error_at(
                        Code::HitlInvalidApprovalArgument,
                        format!("{kw}: unknown argument `{name}` (expected one of: {allowed})"),
                        arg.span,
                    );
                }
            }
        }

        // Disallow positional args after a named arg, matching most
        // languages with mixed-arg syntax.
        let mut seen_named = false;
        for arg in args {
            match arg.name {
                Some(_) => seen_named = true,
                None if seen_named => {
                    self.error_at(
                        Code::HitlInvalidApprovalArgument,
                        format!("{kw}: positional argument cannot follow a named argument"),
                        arg.span,
                    );
                }
                None => {}
            }
        }

        // Disallow duplicate named args.
        for (i, arg) in args.iter().enumerate() {
            if let Some(name) = arg.name.as_deref() {
                if args
                    .iter()
                    .skip(i + 1)
                    .any(|other| other.name.as_deref() == Some(name))
                {
                    self.error_at(
                        Code::DuplicateArgument,
                        format!("{kw}: duplicate argument `{name}`"),
                        arg.span,
                    );
                }
            }
        }

        // Verify required args are present, either positionally (by
        // index in the param spec) or as a matching named arg.
        let positional_count = args.iter().take_while(|a| a.name.is_none()).count();
        for (i, p) in params.iter().enumerate() {
            if !p.required {
                continue;
            }
            let by_position = i < positional_count;
            let by_name = args.iter().any(|a| a.name.as_deref() == Some(p.name));
            if !by_position && !by_name {
                self.error_at(
                    Code::HitlMissingApprovalPolicy,
                    format!("{kw}: missing required argument `{}`", p.name),
                    span,
                );
            }
        }

        // Reject excess positional args.
        if positional_count > params.len() {
            self.error_at(
                Code::HitlInvalidApprovalArgument,
                format!(
                    "{kw}: too many positional arguments (max {} positional, got {})",
                    params.len(),
                    positional_count
                ),
                span,
            );
        }
    }

    /// Canonical typed envelope returned by each HITL primitive. Used
    /// by `infer_type` so callers can write
    /// `let record: ApprovalRecord = request_approval(...)` and have
    /// field access type-check.
    pub(in crate::typechecker) fn hitl_envelope_type(kind: HitlKind) -> TypeExpr {
        match kind {
            HitlKind::AskUser => TypeExpr::Named("any".into()),
            HitlKind::RequestApproval => approval_record_shape(),
            // `dual_control` evaluates the action closure on success and
            // returns *its* result; without inspecting the closure we
            // fall back to `any`. The dedicated [`hitl_expr_inferred_type`]
            // helper picks up the action's return type when it can be
            // syntactically inferred.
            HitlKind::DualControl => TypeExpr::Named("any".into()),
            HitlKind::EscalateTo => escalation_handle_shape(),
        }
    }

    /// Like [`hitl_envelope_type`] but able to look at the call's args
    /// to refine the result. For `ask_user`, the `schema:` arg can
    /// resolve to a concrete type. For `dual_control`, the action
    /// closure's return type flows out.
    pub(in crate::typechecker) fn hitl_expr_inferred_type(
        &self,
        kind: HitlKind,
        args: &[HitlArg],
        scope: &TypeScope,
    ) -> TypeExpr {
        match kind {
            HitlKind::AskUser => self
                .hitl_named_or_positional(args, "schema", 1)
                .and_then(|node| match &node.node {
                    Node::FunctionCall { name, args, .. }
                        if name == "schema_of" && args.len() == 1 =>
                    {
                        if let Node::Identifier(alias) = &args[0].node {
                            scope.resolve_type(alias).cloned()
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .unwrap_or_else(|| Self::hitl_envelope_type(kind)),
            HitlKind::DualControl => self
                .hitl_named_or_positional(args, "action", 2)
                .and_then(|node| match &node.node {
                    Node::Closure {
                        return_type, body, ..
                    } => return_type
                        .clone()
                        .or_else(|| body.last().and_then(|last| self.infer_type(last, scope))),
                    Node::Identifier(name) => {
                        scope.get_fn(name).and_then(|sig| sig.return_type.clone())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| Self::hitl_envelope_type(kind)),
            HitlKind::RequestApproval | HitlKind::EscalateTo => Self::hitl_envelope_type(kind),
        }
    }

    fn hitl_named_or_positional<'a>(
        &self,
        args: &'a [HitlArg],
        name: &str,
        position: usize,
    ) -> Option<&'a crate::ast::SNode> {
        args.iter()
            .find(|a| a.name.as_deref() == Some(name))
            .or_else(|| args.iter().filter(|a| a.name.is_none()).nth(position))
            .map(|a| &a.value)
    }
}

fn approval_record_shape() -> TypeExpr {
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
            optional: true,
        },
        ShapeField {
            name: "signatures".into(),
            type_expr: TypeExpr::List(Box::new(TypeExpr::Shape(vec![
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
            ]))),
            optional: false,
        },
    ])
}

fn escalation_handle_shape() -> TypeExpr {
    TypeExpr::Shape(vec![
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
            type_expr: TypeExpr::Union(vec![
                TypeExpr::LitString("pending".into()),
                TypeExpr::LitString("accepted".into()),
            ]),
            optional: false,
        },
        ShapeField {
            name: "accepted_at".into(),
            type_expr: TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
            optional: true,
        },
        ShapeField {
            name: "reviewer".into(),
            type_expr: TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]),
            optional: true,
        },
    ])
}
