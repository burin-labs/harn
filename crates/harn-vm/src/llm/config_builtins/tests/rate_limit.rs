//! `llm_rate_limit` set/query/clear semantics.

use super::super::rate_limit_builtins::llm_rate_limit_builtin;
use super::fixtures::{build_dict, expect_int};
use crate::value::{VmError, VmValue};

#[test]
fn test_llm_rate_limit_sets_and_queries_rich_details() {
    let _guard = crate::llm::env_guard();
    crate::llm::reset_llm_state();
    let provider = VmValue::String(arcstr::ArcStr::from("quota-builtin-provider"));
    let mut out = String::new();

    let set = llm_rate_limit_builtin(
        &[
            provider.clone(),
            build_dict(vec![
                ("rpm", VmValue::Int(12)),
                ("tpm", VmValue::Int(34_000)),
                ("input_tpm", VmValue::Int(20_000)),
                ("output_tpm", VmValue::Int(14_000)),
                ("concurrency", VmValue::Int(2)),
            ]),
        ],
        &mut out,
    )
    .expect("set rate limit");
    assert!(matches!(set, VmValue::Bool(true)));

    let legacy_query = llm_rate_limit_builtin(std::slice::from_ref(&provider), &mut out)
        .expect("query legacy rpm");
    assert!(matches!(legacy_query, VmValue::Int(12)));

    let details = llm_rate_limit_builtin(
        &[
            provider.clone(),
            build_dict(vec![("details", VmValue::Bool(true))]),
        ],
        &mut out,
    )
    .expect("query details");
    let details = details.as_dict().expect("details dict");
    expect_int(details, "rpm", 12);
    expect_int(details, "tpm", 34_000);
    expect_int(details, "input_tpm", 20_000);
    expect_int(details, "output_tpm", 14_000);
    expect_int(details, "concurrency", 2);

    let overflow = llm_rate_limit_builtin(
        &[
            provider.clone(),
            build_dict(vec![("rpm", VmValue::Int(i64::from(u32::MAX) + 1))]),
        ],
        &mut out,
    )
    .expect_err("oversized rpm should error");
    match overflow {
        VmError::Runtime(message) => assert!(
            message.contains("unsigned 32-bit integer"),
            "unexpected message: {message}"
        ),
        other => panic!("expected Runtime error, got {other:?}"),
    }

    let clear = llm_rate_limit_builtin(
        &[provider.clone(), build_dict(vec![("rpm", VmValue::Int(0))])],
        &mut out,
    )
    .expect("clear rate limit");
    assert!(matches!(clear, VmValue::Bool(true)));
    let cleared = llm_rate_limit_builtin(
        &[provider, build_dict(vec![("details", VmValue::Bool(true))])],
        &mut out,
    )
    .expect("query cleared");
    assert!(matches!(cleared, VmValue::Nil));
    crate::llm::reset_llm_state();
}
