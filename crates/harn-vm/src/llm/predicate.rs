//! Predicate runtime boundary. Frontend admission can ship independently, but
//! a registered source method must never silently use unrestricted transport.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

const PREDICATE_BUILTINS: &[&VmBuiltinDef] = &[
    &EVALUATE_PREDICATE_BUILTIN_DEF,
    &EVALUATE_BUILTIN_DEF,
    &EVALUATE_REQUEST_BUILTIN_DEF,
    &ESTIMATE_STATE_TOKENS_BUILTIN_DEF,
];

pub(super) fn register(vm: &mut Vm) {
    register_builtin_defs(vm, PREDICATE_BUILTINS);
}

/// The single-boolean projection of `harness.llm.evaluate`: one boolean
/// question, named by the site, through the same budgeted evaluator.
#[harn_builtin(
    exposure = "harness.llm.evaluate_predicate",
    effects = ["llm.write@arg3.provider", "llm.write@arg3.model"],
    sig_expr = harn_builtin_meta::predicate::EVALUATE_PREDICATE,
    kind = "async",
    category = "llm.predicate"
)]
async fn evaluate_predicate_builtin(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let batched = super::decision::predicate_arguments(&args)?;
    let (outcome, answers, policy) = super::decision::evaluate(&ctx, &batched).await?;
    Ok(
        super::decision::outcome::project_to_predicate(outcome, &answers, policy.threshold)
            .into_value(),
    )
}

/// The batched entry point. The single-boolean one above is its projection,
/// so both run the same evaluator and cannot refuse differently.
#[harn_builtin(
    exposure = "harness.llm.evaluate",
    effects = ["llm.write@arg3.provider", "llm.write@arg3.model"],
    sig_expr = harn_builtin_meta::predicate::EVALUATE,
    kind = "async",
    category = "llm.predicate"
)]
async fn evaluate_builtin(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let (outcome, _, _) = super::decision::evaluate(&ctx, &args).await?;
    Ok(outcome.into_value())
}

/// Runtime-declared questions retain the same evaluator and closed outcome.
#[harn_builtin(
    exposure = "harness.llm.evaluate_request",
    effects = ["llm.write@arg3.provider", "llm.write@arg3.model"],
    sig_expr = harn_builtin_meta::predicate::EVALUATE_REQUEST,
    kind = "async",
    category = "llm.predicate"
)]
async fn evaluate_request_builtin(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let (outcome, _, _) = super::decision::evaluate(&ctx, &args).await?;
    Ok(outcome.into_value())
}

/// What the evaluator thinks a state costs, before sending it.
///
/// This is the number the `state_too_large` arm compares against a route's
/// declared window, reached through the same two calls the ceiling makes. A
/// caller sizing an input against any other estimate is sizing it against a
/// ruler the refusal does not use, which is how a window that "fits" comes
/// back refused.
#[harn_builtin(
    exposure = "harness.llm.estimate_state_tokens",
    effects = [],
    sig_expr = harn_builtin_meta::predicate::ESTIMATE_STATE_TOKENS,
    category = "llm.predicate"
)]
fn estimate_state_tokens_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let state = args.first().cloned().unwrap_or(VmValue::Nil);
    let json = super::helpers::vm_value_to_json(&state);
    Ok(VmValue::Int(
        super::decision::estimate_state_tokens(&json) as i64
    ))
}

#[cfg(test)]
mod tests {
    use harn_parser::{builtin_signatures::TyExt, DiagnosticSeverity, Parser, TypeChecker};

    #[test]
    fn predicate_stdlib_types_match_the_capability_contract_in_both_directions() {
        let declarations =
            harn_stdlib::get_stdlib_source("predicate").expect("embedded predicate types");
        for (name, contract) in [
            ("PredicateVerdict", harn_builtin_meta::predicate::VERDICT),
            ("EvaluationPolicy", harn_builtin_meta::predicate::POLICY),
            ("PredicateOutcome", harn_builtin_meta::predicate::OUTCOME),
            (
                "BooleanQuestion",
                harn_builtin_meta::predicate::BOOLEAN_QUESTION,
            ),
            (
                "ChoiceQuestion",
                harn_builtin_meta::predicate::CHOICE_QUESTION,
            ),
            (
                "ScoreQuestion",
                harn_builtin_meta::predicate::SCORE_QUESTION,
            ),
            ("EvaluationQuestion", harn_builtin_meta::predicate::QUESTION),
            (
                "BooleanAnswer",
                harn_builtin_meta::predicate::BOOLEAN_ANSWER,
            ),
            ("ChoiceAnswer", harn_builtin_meta::predicate::CHOICE_ANSWER),
            ("ScoreAnswer", harn_builtin_meta::predicate::SCORE_ANSWER),
            ("EvaluationAnswer", harn_builtin_meta::predicate::ANSWER),
            (
                "EvaluationOutcome",
                harn_builtin_meta::predicate::EVALUATION_OUTCOME,
            ),
        ] {
            let structural = harn_parser::format_type(&contract.to_type_expr());
            let source = format!("{declarations}\nfn to_contract(value: {name}) -> {structural} {{ return value }}\nfn from_contract(value: {structural}) -> {name} {{ return value }}");
            let program = Parser::new(harn_lexer::Lexer::new(&source).tokenize().unwrap())
                .parse()
                .unwrap();
            let errors: Vec<_> = TypeChecker::new()
                .check_with_source(&program, &source)
                .into_iter()
                .filter(|d| d.severity == DiagnosticSeverity::Error)
                .collect();
            assert!(errors.is_empty(), "{name}: {errors:?}");
        }
    }
}
