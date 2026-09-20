//! Predicate runtime boundary. Frontend admission can ship independently, but
//! a registered source method must never silently use unrestricted transport.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

const PREDICATE_BUILTINS: &[&VmBuiltinDef] = &[&EVALUATE_PREDICATE_BUILTIN_DEF];

pub(super) fn register(vm: &mut Vm) {
    register_builtin_defs(vm, PREDICATE_BUILTINS);
}

/// Predicate execution requires the dedicated budgeted evaluator.
#[harn_builtin(
    exposure = "harness.llm.evaluate_predicate",
    effects = ["llm.write@arg3.provider", "llm.write@arg3.model"],
    sig_expr = harn_builtin_meta::predicate::EVALUATE,
    kind = "async",
    category = "llm.predicate"
)]
async fn evaluate_predicate_builtin(
    _ctx: AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    Err(VmError::Runtime(
        "predicate execution is unavailable: this runtime has no budgeted predicate evaluator"
            .into(),
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
            ("PredicatePolicy", harn_builtin_meta::predicate::POLICY),
            ("PredicateOutcome", harn_builtin_meta::predicate::OUTCOME),
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
