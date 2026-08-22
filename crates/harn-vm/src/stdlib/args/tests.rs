use super::*;

fn string(text: &str) -> VmValue {
    VmValue::string(text)
}

fn dict_value(pairs: &[(&str, VmValue)]) -> VmValue {
    VmValue::dict(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
}

fn message(error: &VmError) -> String {
    match error {
        VmError::Runtime(text) | VmError::TypeError(text) => text.clone(),
        VmError::Thrown(VmValue::String(text)) => text.to_string(),
        other => panic!("unexpected error shape: {other:?}"),
    }
}

#[test]
fn missing_and_wrong_type_read_differently() {
    let values = vec![VmValue::Int(3)];
    let args = Args::new("jwt_sign", &values);

    assert_eq!(
        message(&args.string(0, "alg").unwrap_err()),
        "jwt_sign: `alg` must be a string, got int"
    );
    assert_eq!(
        message(&args.string(1, "claims").unwrap_err()),
        "jwt_sign: `claims` is required"
    );
}

/// The bug the old per-module helpers hid: a `.display()`-based reader
/// stringifies a dict instead of rejecting it, so a mistyped call reaches
/// the network rather than the type error.
#[test]
fn a_dict_is_not_a_string() {
    let values = vec![dict_value(&[("a", VmValue::Bool(true))])];
    let args = Args::new("connector_call", &values);
    assert_eq!(
        message(&args.string(0, "name").unwrap_err()),
        "connector_call: `name` must be a string, got dict"
    );
}

#[test]
fn optional_arguments_treat_nil_as_absent() {
    let values = vec![VmValue::Nil, string("kept")];
    let args = Args::new("http_choose", &values);
    assert_eq!(args.opt_string(0, "accept").unwrap(), None);
    assert_eq!(args.opt_string(1, "default").unwrap(), Some("kept"));
    assert_eq!(args.opt_string(9, "missing").unwrap(), None);
}

#[test]
fn optional_wrong_type_says_or_nil() {
    let values = vec![VmValue::Int(1)];
    let args = Args::new("http_choose", &values);
    assert_eq!(
        message(&args.opt_string(0, "accept").unwrap_err()),
        "http_choose: `accept` must be a string or nil, got int"
    );
}

#[test]
fn non_empty_string_trims_and_rejects_blank() {
    let values = vec![string("  name  "), string("   ")];
    let args = Args::new("git_log", &values);
    assert_eq!(args.non_empty_string(0, "path").unwrap(), "name");
    assert_eq!(
        message(&args.non_empty_string(1, "rev").unwrap_err()),
        "git_log: `rev` must not be empty"
    );
}

#[test]
fn floats_are_not_ints() {
    let values = vec![VmValue::Float(3.0)];
    let args = Args::new("round", &values);
    assert_eq!(
        message(&args.int(0, "digits").unwrap_err()),
        "round: `digits` must be an int, got float"
    );
    assert_eq!(args.number(0, "digits").unwrap(), 3.0);
}

#[test]
fn usize_rejects_negatives_with_the_constraint_not_the_type() {
    let values = vec![VmValue::Int(-1)];
    let args = Args::new("bytes_slice", &values);
    assert_eq!(
        message(&args.usize(0, "start").unwrap_err()),
        "bytes_slice: `start` must be >= 0"
    );
}

#[test]
fn string_list_names_the_offending_element_type() {
    let values = vec![VmValue::List(std::sync::Arc::new(vec![
        string("a"),
        VmValue::Int(2),
    ]))];
    let args = Args::new("git_add", &values);
    assert_eq!(
        message(&args.string_list(0, "paths").unwrap_err()),
        "git_add: `paths` must be a list<string>, got int"
    );
}

#[test]
fn enum_string_lists_what_was_allowed() {
    let values = vec![string("HS256")];
    let args = Args::new("jwt_sign", &values);
    assert_eq!(
        message(&args.enum_string(0, "alg", &["ES256", "RS256"]).unwrap_err()),
        "jwt_sign: `alg` must be one of `ES256`, `RS256`; got `HS256`"
    );
}

#[test]
fn millis_accepts_duration_int_and_finite_float() {
    let values = vec![
        VmValue::Duration(1_500),
        VmValue::Int(250),
        VmValue::Float(42.9),
        VmValue::Float(f64::INFINITY),
        VmValue::Int(-1),
        string("5s"),
    ];
    let args = Args::new("waitpoint_wait", &values);
    assert_eq!(args.millis(0, "timeout").unwrap(), 1_500);
    assert_eq!(args.millis(1, "timeout").unwrap(), 250);
    assert_eq!(args.millis(2, "timeout").unwrap(), 42);
    assert_eq!(
        message(&args.millis(3, "timeout").unwrap_err()),
        "waitpoint_wait: `timeout` must be a finite millisecond count >= 0"
    );
    assert_eq!(
        message(&args.millis(4, "timeout").unwrap_err()),
        "waitpoint_wait: `timeout` must be >= 0"
    );
    assert_eq!(
        message(&args.millis(5, "timeout").unwrap_err()),
        "waitpoint_wait: `timeout` must be a duration or an int, got string"
    );
}

#[test]
fn arity_reports_the_range_it_wanted() {
    let values = vec![VmValue::Int(1)];
    let args = Args::new("jwt_sign", &values);
    assert_eq!(
        message(&args.arity(3, 3).unwrap_err()),
        "jwt_sign: expected 3 argument(s), got 1"
    );
    assert_eq!(
        message(&args.arity(2, 4).unwrap_err()),
        "jwt_sign: expected 2-4 argument(s), got 1"
    );
    args.arity(1, 1).unwrap();
}

#[test]
fn error_kind_selects_the_variant() {
    let values = vec![VmValue::Int(1)];
    assert!(matches!(
        Args::new("f", &values).string(0, "a").unwrap_err(),
        VmError::TypeError(_)
    ));
    assert!(matches!(
        Args::runtime("f", &values).string(0, "a").unwrap_err(),
        VmError::Runtime(_)
    ));
    assert!(matches!(
        Args::thrown("f", &values).string(0, "a").unwrap_err(),
        VmError::Thrown(VmValue::String(_))
    ));
}

// ---- option bags ---------------------------------------------------------

#[test]
fn an_absent_option_bag_reads_as_empty() {
    let values: Vec<VmValue> = Vec::new();
    let args = Args::new("agent_spawn", &values);
    let mut options = args.options(0, "options").unwrap();
    assert!(options.is_empty());
    assert_eq!(options.opt_string("task").unwrap(), None);
    assert!(options.bool_or("wait", true).unwrap());
    options.finish(&[]).unwrap();
}

#[test]
fn option_bag_rejects_unknown_keys_it_was_not_told_to_forward() {
    let values = vec![dict_value(&[
        ("task", string("ship it")),
        ("timout", VmValue::Int(5)),
        ("forwarded", VmValue::Bool(true)),
    ])];
    let args = Args::new("agent_spawn", &values);
    let mut options = args.options(0, "options").unwrap();
    options.opt_string("task").unwrap();
    assert_eq!(
        message(&options.finish(&["forwarded"]).unwrap_err()),
        "agent_spawn: unknown option(s): timout"
    );
}

#[test]
fn option_bag_errors_carry_the_key_and_the_builtin() {
    let values = vec![dict_value(&[("limit", string("ten"))])];
    let args = Args::new("event_log_read", &values);
    let mut options = args.options(0, "options").unwrap();
    assert_eq!(
        message(&options.opt_int("limit").unwrap_err()),
        "event_log_read: `limit` must be an int or nil, got string"
    );
}

#[test]
fn a_non_dict_option_bag_is_rejected_before_any_key_is_read() {
    let values = vec![string("not a bag")];
    let args = Args::new("agent_spawn", &values);
    assert_eq!(
        message(&args.options(0, "options").unwrap_err()),
        "agent_spawn: `options` must be a dict or nil, got string"
    );
}

#[test]
fn nested_option_bags_keep_the_builtin_name() {
    let values = vec![dict_value(&[(
        "retry",
        dict_value(&[("attempts", string("three"))]),
    )])];
    let args = Args::new("http_get", &values);
    let mut options = args.options(0, "options").unwrap();
    let mut retry = options.opt_options("retry").unwrap();
    assert_eq!(
        message(&retry.opt_int("attempts").unwrap_err()),
        "http_get: `attempts` must be an int or nil, got string"
    );
}
