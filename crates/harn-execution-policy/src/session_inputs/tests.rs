use super::*;

fn snapshot(inputs: &mut SessionInputs, now: u64) -> PrivateInputSnapshot {
    source_snapshot(inputs, "test-host", now)
}

fn source_snapshot(inputs: &mut SessionInputs, source: &str, now: u64) -> PrivateInputSnapshot {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    inputs
        .write_private_snapshot(source, now, &mut *bytes)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn session_input_reset_rejects_delayed_grants_from_the_previous_conversation() {
    let mut owner = SessionInputs::default();
    owner
        .put(secret("KEY"), "reset-canary".into(), false, 0)
        .unwrap();
    let mut receiver = SessionInputs::default();
    receiver
        .apply_private_snapshot(snapshot(&mut owner, 0), 0)
        .unwrap();
    receiver.reset();
    assert!(!receiver
        .apply_private_snapshot(snapshot(&mut owner, 0), 0)
        .unwrap());
    assert!(receiver.list(0).is_empty());
    owner
        .put(secret("KEY"), "new-grant-canary".into(), true, 0)
        .unwrap();
    assert!(receiver
        .apply_private_snapshot(snapshot(&mut owner, 0), 0)
        .unwrap());
    assert_eq!(receiver.list(0).len(), 1);
}

#[test]
fn each_host_updates_only_its_grants_and_cannot_overwrite_another_source() {
    let mut runtime = SessionInputs::default();
    runtime
        .put(secret("PRECONFIGURED"), "server-canary".into(), false, 0)
        .unwrap();
    let mut first = SessionInputs::default();
    first
        .put(secret("FIRST"), "first-canary".into(), false, 0)
        .unwrap();
    let stale = source_snapshot(&mut first, "first-window", 0);
    runtime
        .apply_private_snapshot(source_snapshot(&mut first, "first-window", 0), 0)
        .unwrap();
    let mut second = SessionInputs::default();
    second
        .put(secret("SECOND"), "second-canary".into(), false, 0)
        .unwrap();
    runtime
        .apply_private_snapshot(source_snapshot(&mut second, "second-window", 0), 0)
        .unwrap();
    assert_eq!(runtime.list(0).len(), 3);
    first.remove("FIRST");
    runtime
        .apply_private_snapshot(source_snapshot(&mut first, "first-window", 0), 0)
        .unwrap();
    assert_eq!(
        runtime.apply_private_snapshot(stale, 0),
        Err(InputError::StaleSnapshot)
    );
    assert_eq!(
        runtime
            .list(0)
            .iter()
            .map(|entry| entry.spec.name.as_str())
            .collect::<Vec<_>>(),
        ["PRECONFIGURED", "SECOND"]
    );
    first
        .put(secret("SECOND"), "conflicting-canary".into(), false, 0)
        .unwrap();
    assert_eq!(
        runtime.apply_private_snapshot(source_snapshot(&mut first, "first-window", 0), 0),
        Err(InputError::AlreadyExists)
    );
    assert_eq!(
        runtime
            .with_value("SECOND", InputUse::Command, 0, str::to_owned)
            .unwrap(),
        "second-canary"
    );
    assert_eq!(
        runtime.redact("server-canary first-canary second-canary"),
        "[redacted] [redacted] [redacted]"
    );
    // An explicit trusted replacement becomes independent of the old source.
    runtime
        .put(secret("SECOND"), "operator-canary".into(), true, 0)
        .unwrap();
    second.remove("SECOND");
    runtime
        .apply_private_snapshot(source_snapshot(&mut second, "second-window", 0), 0)
        .unwrap();
    assert_eq!(
        runtime
            .with_value("SECOND", InputUse::Command, 0, str::to_owned)
            .unwrap(),
        "operator-canary"
    );
}

#[test]
fn private_source_history_is_bounded_and_rejects_invalid_identifiers() {
    let mut runtime = SessionInputs::default();
    for index in 0..MAX_INPUTS {
        runtime
            .apply_private_snapshot(
                source_snapshot(&mut SessionInputs::default(), &format!("host-{index}"), 0),
                0,
            )
            .unwrap();
    }
    assert_eq!(
        runtime.apply_private_snapshot(
            source_snapshot(&mut SessionInputs::default(), "extra-host", 0),
            0
        ),
        Err(InputError::InvalidSnapshot)
    );
    let invalid = serde_json::from_value(
        serde_json::json!({"sourceId":"invalid/source", "revision":0,"inputs":[],"redactions":[]}),
    )
    .unwrap();
    assert_eq!(
        runtime.apply_private_snapshot(invalid, 0),
        Err(InputError::InvalidSnapshot)
    );
}

#[test]
fn private_snapshots_are_atomic_ordered_and_preserve_retired_value_filtering() {
    let mut host = SessionInputs::default();
    let mut runtime = SessionInputs::default();
    host.put(secret("KEY"), "original-canary".into(), false, 0)
        .unwrap();
    let stale = snapshot(&mut host, 0);
    assert!(runtime
        .apply_private_snapshot(snapshot(&mut host, 0), 0)
        .unwrap());
    assert!(!runtime
        .apply_private_snapshot(snapshot(&mut host, 0), 0)
        .unwrap());
    host.put(secret("KEY"), "replacement-canary".into(), true, 0)
        .unwrap();
    host.remove("KEY");
    assert!(runtime
        .apply_private_snapshot(snapshot(&mut host, 0), 0)
        .unwrap());
    assert_eq!(
        runtime.apply_private_snapshot(stale, 0),
        Err(InputError::StaleSnapshot)
    );
    assert!(runtime.list(0).is_empty());
    assert_eq!(
        runtime.redact("original-canary replacement-canary"),
        "[redacted] [redacted]"
    );
    host.put(secret("OTHER"), "available-canary".into(), false, 0)
        .unwrap();
    runtime
        .apply_private_snapshot(snapshot(&mut host, 0), 0)
        .unwrap();
    let invalid = serde_json::from_value(serde_json::json!({
        "sourceId":"test-host", "revision":100, "inputs":[{"spec":{"name":"INVALID-NAME"},"value":"new-canary"}], "redactions":[],
    })).unwrap();
    assert_eq!(
        runtime.apply_private_snapshot(invalid, 0),
        Err(InputError::InvalidName)
    );
    assert_eq!(
        runtime
            .with_value("OTHER", InputUse::Command, 0, str::to_owned)
            .unwrap(),
        "available-canary"
    );
}

fn secret(name: &str) -> InputSpec {
    InputSpec {
        name: name.into(),
        kind: InputKind::Secret,
        lifetime: InputLifetime::Session,
        usage: InputUsage::SessionTools,
    }
}

#[test]
fn descriptions_and_debug_omit_private_values_but_preserve_variables() {
    let mut inputs = SessionInputs::default();
    let receipt = inputs
        .put(secret("SERVICE_KEY"), "private-canary".into(), false, 10)
        .unwrap();
    let mut variable = secret("REGION");
    variable.kind = InputKind::Variable;
    inputs.put(variable, "west".into(), false, 10).unwrap();
    let serialized = serde_json::to_string(&inputs.list(10)).unwrap();
    assert!(serialized.contains("SERVICE_KEY"));
    assert!(serialized.contains("west"));
    assert!(!serialized.contains("private-canary"));
    assert!(receipt.value.is_none());
    assert!(!format!("{inputs:?}").contains("private-canary"));
    let value = inputs
        .with_value("SERVICE_KEY", InputUse::Command, 10, str::to_owned)
        .unwrap();
    assert_eq!(
        value, "private-canary",
        "trusted consumer actually received the grant"
    );
}

#[test]
fn website_restriction_checks_exact_origin_and_refuses_commands_before_effects() {
    let mut inputs = SessionInputs::default();
    let mut spec = secret("SERVICE_KEY");
    spec.usage = InputUsage::Websites {
        origins: vec!["https://API.example.test:443/".into()],
    };
    let receipt = inputs
        .put(spec, "private-canary".into(), false, 10)
        .unwrap();
    assert_eq!(
        receipt.spec.usage,
        InputUsage::Websites {
            origins: vec!["https://api.example.test".into()]
        }
    );
    let mut calls = 0;
    inputs
        .with_value(
            "SERVICE_KEY",
            InputUse::Http("https://api.example.test/v1"),
            10,
            |value| {
                assert_eq!(value, "private-canary");
                calls += 1;
            },
        )
        .unwrap();
    for usage in [
        InputUse::Command,
        InputUse::Http("https://api.example.test.evil.test/"),
        InputUse::Http("https://sub.api.example.test/"),
        InputUse::Http("https://api.example.test:444/"),
        InputUse::Http("http://api.example.test/"),
        InputUse::Http("https://user@api.example.test/"),
    ] {
        assert_eq!(
            inputs.with_value("SERVICE_KEY", usage, 10, |_| calls += 1),
            Err(InputError::UsageDenied)
        );
    }
    assert_eq!(calls, 1, "only the allowed request can reach the effect");
}

#[test]
fn expiry_revocation_and_close_refuse_effects_and_cover_late_output() {
    let mut inputs = SessionInputs::default();
    let mut spec = secret("SERVICE_KEY");
    spec.lifetime = InputLifetime::Until { unix_ms: 20 };
    inputs
        .put(spec, "expired-canary".into(), false, 10)
        .unwrap();
    inputs
        .with_value("SERVICE_KEY", InputUse::Command, 19, |_| ())
        .unwrap();
    assert_eq!(
        inputs.with_value("SERVICE_KEY", InputUse::Command, 20, |_| panic!(
            "expired effect"
        )),
        Err(InputError::Unavailable)
    );
    assert!(inputs.list(20).is_empty());
    inputs
        .put(secret("SERVICE_KEY"), "replaced-canary".into(), false, 20)
        .unwrap();
    inputs
        .put(secret("SERVICE_KEY"), "revoked-canary".into(), true, 20)
        .unwrap();
    assert!(inputs.remove("SERVICE_KEY"));
    assert!(!inputs.remove("SERVICE_KEY"));
    assert_eq!(
        inputs.with_value("SERVICE_KEY", InputUse::Command, 20, |_| panic!(
            "revoked effect"
        )),
        Err(InputError::Unavailable)
    );
    inputs
        .put(secret("OTHER"), "closed-canary".into(), false, 20)
        .unwrap();
    inputs.close();
    assert_eq!(
        inputs.with_value("OTHER", InputUse::Command, 20, |_| panic!("closed effect")),
        Err(InputError::Closed)
    );
    assert_eq!(
        inputs.put(secret("NEW"), "new-canary".into(), false, 20),
        Err(InputError::Closed)
    );
    assert!(inputs.list(20).is_empty());
    assert_eq!(
        inputs.redact("expired-canary replaced-canary revoked-canary closed-canary"),
        "[redacted] [redacted] [redacted] [redacted]"
    );
}

#[test]
fn invalid_specs_fail_without_echoing_submitted_values() {
    for name in ["", "1KEY", "KEY=VALUE", "KEY\nVALUE", "KEY-NAME", "é"] {
        let mut inputs = SessionInputs::default();
        let error = inputs
            .put(secret(name), "private-canary".into(), false, 10)
            .unwrap_err();
        assert_eq!(error, InputError::InvalidName);
        assert!(!error.to_string().contains("private-canary"));
    }
    for origin in [
        "http://api.test",
        "https://api.test/path",
        "https://u:p@api.test",
        "https://api.test?x=1",
        "https://api.test#part",
        "https://*.api.test",
        "*",
    ] {
        let mut spec = secret("KEY");
        spec.usage = InputUsage::Websites {
            origins: vec![origin.into()],
        };
        assert_eq!(spec.validate(10), Err(InputError::InvalidWebsites));
    }
    let mut spec = secret("KEY");
    spec.lifetime = InputLifetime::Until { unix_ms: 10 };
    assert_eq!(spec.validate(10), Err(InputError::InvalidExpiry));
}

#[test]
fn names_are_unique_and_empty_variables_are_real_values() {
    let mut inputs = SessionInputs::default();
    assert_eq!(
        inputs.put(secret("KEY"), String::new(), false, 0),
        Err(InputError::InvalidValue)
    );
    let mut spec = secret("KEY");
    spec.kind = InputKind::Variable;
    assert_eq!(
        inputs.put(spec, String::new(), false, 0).unwrap().value,
        Some(String::new())
    );
    assert_eq!(
        inputs.put(secret("KEY"), "other".into(), false, 0),
        Err(InputError::AlreadyExists)
    );
    assert_eq!(
        inputs
            .with_value("KEY", InputUse::Command, 0, str::to_owned)
            .unwrap(),
        ""
    );
}

#[test]
fn sessions_never_share_values_or_redaction_sets() {
    let mut first = SessionInputs::default();
    let mut second = SessionInputs::default();
    first
        .put(secret("KEY"), "first-canary".into(), false, 0)
        .unwrap();
    second
        .put(secret("KEY"), "second-canary".into(), false, 0)
        .unwrap();
    first.close();
    assert_eq!(
        second
            .with_value("KEY", InputUse::Command, 0, str::to_owned)
            .unwrap(),
        "second-canary"
    );
    assert_eq!(
        first.redact("first-canary second-canary"),
        "[redacted] second-canary"
    );
}

#[test]
fn retained_secrets_are_bounded_even_after_replacement_and_removal() {
    let mut inputs = SessionInputs::default();
    for index in 0..16 {
        inputs
            .put(
                secret("KEY"),
                format!("{index:016}{}", "x".repeat(MAX_VALUE_BYTES - 16)),
                index > 0,
                0,
            )
            .unwrap();
    }
    inputs.remove("KEY");
    assert_eq!(
        inputs.put(secret("KEY"), "next".into(), false, 0),
        Err(InputError::LimitReached)
    );
}

#[test]
fn filtering_uses_original_text_and_preserves_overlapping_matches() {
    let mut inputs = SessionInputs::default();
    for (name, value) in [("FIRST", "zab"), ("OVERLAP", "aba"), ("MARKER_PART", "red")] {
        inputs.put(secret(name), value.into(), false, 0).unwrap();
    }
    assert_eq!(inputs.redact("zabababa"), "[redacted]");
    assert_eq!(inputs.redact("red zab"), "[redacted] [redacted]");
    assert_eq!(inputs.redact("ordinary text"), "ordinary text");
    inputs.put(secret("DENSE"), "a".into(), false, 0).unwrap();
    assert_eq!(
        inputs.redact(&"a".repeat(MAX_REDACTION_MATCHES + 1)),
        "[redacted]"
    );
}
