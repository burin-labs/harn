//! Tests for [`super::session_environment`].
//!
//! Split out of `session_environment.rs` so that file stays under the
//! source-length cap. They are the same tests, in the same module, reached
//! through `#[path]` exactly as `stdlib/process_tests.rs` is.

use super::*;

#[test]
fn host_inference_ceiling_survives_isolation_and_withholds_child_exposure() {
    use crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV;
    use crate::llm::api::{InferenceBoundary, InferenceReach};
    let host = InferenceBoundary {
        reach: InferenceReach::LocalOnly,
        allow_training_discounts: false,
    };
    let snapshot = BTreeMap::from([
        (HOST_BOUNDARY_ENV.into(), "untrusted ambient value".into()),
        ("UNCHANGED_SENTINEL".into(), "retained".into()),
    ]);
    let parent = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Inherited,
        vec![],
        snapshot,
        &no_env,
    )
    .unwrap()
    .with_host_inference_boundary(Some(host));
    let child_env = super::super::resolve_env(&parent, &no_env, &|_, _| None).unwrap();
    assert_eq!(
        child_env.get("UNCHANGED_SENTINEL").map(String::as_str),
        Some("retained")
    );
    assert!(!child_env.contains_key(HOST_BOUNDARY_ENV));
    let isolated = parent
        .narrow(EnvironmentPolicyKind::Isolated, vec![])
        .unwrap();
    let raw = isolated
        .env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<InferenceBoundary>(&raw).unwrap(),
        host
    );
    assert!(isolated.grants().is_empty());
    let fork = isolated
        .narrow(EnvironmentPolicyKind::Isolated, vec![])
        .unwrap();
    assert_eq!(
        fork.env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
            .unwrap(),
        Some(raw)
    );
    assert!(isolated
        .narrow(
            EnvironmentPolicyKind::Isolated,
            vec![GrantSpec {
                name: "additional-authority".into(),
                source: GrantSourceSpec::Literal {
                    value: "refused".into()
                },
                expose_as_env: Some("EXTRA".into()),
                for_command: None,
                expose_to: GrantAudience::Session,
            }]
        )
        .is_err());
    assert_eq!(
        isolated.receipts().last().unwrap().exposed_to,
        GrantAudience::InProcess
    );
    assert!(!isolated
        .admitted_environment_names()
        .iter()
        .any(|name| name == HOST_BOUNDARY_ENV));
}

#[test]
fn session_grant_cannot_widen_or_expose_host_inference_ceiling() {
    use crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV;
    use crate::llm::api::{InferenceBoundary, InferenceReach};
    let host = InferenceBoundary {
        reach: InferenceReach::HostedOpenWeight,
        allow_training_discounts: false,
    };
    for requested in [InferenceReach::AnyHosted, InferenceReach::LocalOnly] {
        let grant = GrantSpec {
            name: "client-boundary".into(),
            source: GrantSourceSpec::Literal {
                value: serde_json::to_string(&InferenceBoundary {
                    reach: requested,
                    allow_training_discounts: true,
                })
                .unwrap(),
            },
            expose_as_env: Some(HOST_BOUNDARY_ENV.into()),
            for_command: None,
            expose_to: GrantAudience::Session,
        };
        let environment = SessionEnvironment::launch_from_snapshot(
            EnvironmentPolicyKind::Granted,
            vec![grant],
            BTreeMap::new(),
            &no_env,
        )
        .unwrap()
        .with_host_inference_boundary(Some(host));
        let value = environment
            .env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
            .unwrap()
            .unwrap();
        let effective: InferenceBoundary = serde_json::from_str(&value).unwrap();
        assert_eq!(
            effective.reach,
            if requested == InferenceReach::AnyHosted {
                host.reach
            } else {
                requested
            }
        );
        assert!(!effective.allow_training_discounts);
        assert!(environment.env_exposure(&|_, _| None).unwrap().is_empty());
        let child = environment
            .narrow(EnvironmentPolicyKind::Isolated, vec![])
            .unwrap();
        let child_value = child
            .env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<InferenceBoundary>(&child_value).unwrap(),
            effective
        );
        assert_eq!(
            environment.receipts().len(),
            1,
            "only effective host authority is receipted"
        );
    }
}

#[test]
fn host_inference_reserved_name_obeys_platform_case_semantics() {
    use super::super::environment_policy::{
        environment_names_equal, environment_names_equal_for_platform,
    };
    use crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV;
    use crate::llm::api::{InferenceBoundary, InferenceReach};
    let lowercase = HOST_BOUNDARY_ENV.to_ascii_lowercase();
    assert!(environment_names_equal_for_platform(
        HOST_BOUNDARY_ENV,
        &lowercase,
        true
    ));
    assert!(!environment_names_equal_for_platform(
        HOST_BOUNDARY_ENV,
        &lowercase,
        false
    ));
    assert_eq!(
        environment_names_equal(HOST_BOUNDARY_ENV, &lowercase),
        cfg!(windows)
    );
    let host = InferenceBoundary {
        reach: InferenceReach::LocalOnly,
        allow_training_discounts: false,
    };
    let snapshot = BTreeMap::from([(lowercase.clone(), "case control".into())]);
    let environment = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Inherited,
        vec![],
        snapshot,
        &no_env,
    )
    .unwrap()
    .with_host_inference_boundary(Some(host));
    let child = super::super::resolve_env(&environment, &no_env, &|_, _| None).unwrap();
    assert_eq!(child.contains_key(&lowercase), !cfg!(windows));
    let grant = GrantSpec {
        name: "client-case-control".into(),
        source: GrantSourceSpec::Literal {
            value: serde_json::to_string(&host).unwrap(),
        },
        expose_as_env: Some(lowercase),
        for_command: None,
        expose_to: GrantAudience::Session,
    };
    let granted = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Granted,
        vec![grant],
        BTreeMap::new(),
        &no_env,
    )
    .unwrap()
    .with_host_inference_boundary(Some(host));
    assert_eq!(
        granted.env_exposure(&|_, _| None).unwrap().is_empty(),
        cfg!(windows)
    );
    assert_eq!(granted.receipts().len(), if cfg!(windows) { 1 } else { 2 });
}

#[test]
fn malformed_session_boundary_cannot_replace_host_floor() {
    use crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV;
    use crate::llm::api::{InferenceBoundary, InferenceReach};
    let grant = GrantSpec {
        name: "malformed-boundary".into(),
        source: GrantSourceSpec::Literal {
            value: "invalid private value".into(),
        },
        expose_as_env: Some(HOST_BOUNDARY_ENV.into()),
        for_command: None,
        expose_to: GrantAudience::InProcess,
    };
    let environment = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Granted,
        vec![grant],
        BTreeMap::new(),
        &no_env,
    )
    .unwrap()
    .with_host_inference_boundary(Some(InferenceBoundary {
        reach: InferenceReach::LocalOnly,
        allow_training_discounts: false,
    }));
    let error = environment
        .env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
        .unwrap_err();
    assert_eq!(error.code(), "inference_boundary.host_boundary_malformed");
    assert!(!error
        .to_json()
        .to_string()
        .contains("invalid private value"));
    assert_eq!(
        environment
            .narrow(EnvironmentPolicyKind::Isolated, vec![])
            .unwrap_err(),
        error
    );
}

#[test]
fn dispatch_scope_keeps_stricter_session_and_restores_previous_authority() {
    use crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV;
    use crate::llm::api::{InferenceBoundary, InferenceReach};
    use crate::stdlib::process::{
        current_session_environment, declare_session_environment_if_absent,
    };
    crate::reset_thread_local_state();
    let strict = InferenceBoundary {
        reach: InferenceReach::LocalOnly,
        allow_training_discounts: false,
    };
    let parent = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Isolated,
        vec![],
        BTreeMap::new(),
        &no_env,
    )
    .unwrap()
    .with_host_inference_boundary(Some(strict));
    let outer = declare_session_environment_if_absent(parent.clone());
    let inner = declare_session_environment_if_absent(SessionEnvironment::inherited())
        .with_host_inference_boundary(Some(InferenceBoundary {
            reach: InferenceReach::AnyHosted,
            allow_training_discounts: true,
        }));
    let active = current_session_environment().unwrap();
    assert!(active.is_isolated());
    let value = active
        .env_exposure_for(HOST_BOUNDARY_ENV, &|_, _| None)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<InferenceBoundary>(&value).unwrap(),
        strict
    );
    drop(inner);
    assert_eq!(current_session_environment(), Some(parent));
    drop(outer);
    assert!(current_session_environment().is_none());
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn env_from(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |var: &str| {
        pairs
            .iter()
            .find(|(name, _)| *name == var)
            .map(|(_, value)| value.to_string())
    }
}

fn env_grant(name: &str, var: &str, expose: Option<&str>) -> GrantSpec {
    GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::Env {
            var: var.to_string(),
        },
        expose_as_env: expose.map(str::to_string),
        for_command: None,
        expose_to: Default::default(),
    }
}

fn secret_grant(name: &str, account: &str, key: &str, expose: Option<&str>) -> GrantSpec {
    GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::SecretStore {
            account: account.to_string(),
            key: key.to_string(),
        },
        expose_as_env: expose.map(str::to_string),
        for_command: None,
        expose_to: Default::default(),
    }
}

fn command_grant(
    name: &str,
    account: &str,
    key: &str,
    expose: &str,
    for_command: &str,
) -> GrantSpec {
    GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::SecretStore {
            account: account.to_string(),
            key: key.to_string(),
        },
        expose_as_env: Some(expose.to_string()),
        for_command: Some(for_command.to_string()),
        expose_to: Default::default(),
    }
}

fn literal_grant(name: &str, value: &str, expose: Option<&str>) -> GrantSpec {
    GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::Literal {
            value: value.to_string(),
        },
        expose_as_env: expose.map(str::to_string),
        for_command: None,
        expose_to: Default::default(),
    }
}

/// The case the source exists for: a value the launcher does not hold, which
/// the child must nevertheless see, and which is the empty string.
///
/// Several developer tools read an empty variable as an explicit "off" that
/// differs from the variable being absent. A snapshot source cannot express
/// that for a name the launcher has never set, because it resolves by reading
/// the launcher and fails when there is nothing to read.
///
/// The pairing with the absent name is the point. Without it this would pass
/// on an implementation that dropped the grant entirely, since both readings
/// would then be "not there".
#[test]
fn a_literal_empty_value_reaches_the_child_and_an_absent_name_does_not() {
    let specs = vec![literal_grant("wrapper_off", "", Some("TOOL_WRAPPER"))];
    let environment = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect("granted accepts a literal");
    let resolved =
        crate::security::environment_policy::resolve_env(&environment, &no_env, &|_, _| None)
            .expect("resolve");
    assert_eq!(
        resolved.get("TOOL_WRAPPER").map(String::as_str),
        Some(""),
        "a literal empty value must reach the child as an empty string"
    );
    assert!(
        !resolved.contains_key("TOOL_WRAPPER_NOT_DECLARED"),
        "a name nobody declared must not appear, or the case above proves nothing"
    );
}

/// A stated value is used verbatim, including whitespace the other sources
/// would trim. `Env` and `SecretStore` carry names, where whitespace is a
/// typo; this one carries a value, where it may be the point.
#[test]
fn a_literal_value_is_used_verbatim() {
    let specs = vec![
        literal_grant("plain", "off", Some("PLAIN")),
        literal_grant("spaced", "  two  ", Some("SPACED")),
    ];
    let environment = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect("granted accepts literals");
    let resolved =
        crate::security::environment_policy::resolve_env(&environment, &no_env, &|_, _| None)
            .expect("resolve");
    assert_eq!(resolved.get("PLAIN").map(String::as_str), Some("off"));
    assert_eq!(resolved.get("SPACED").map(String::as_str), Some("  two  "));
}

/// The two sources stay distinct. A snapshot grant still resolves by reading
/// the launcher, and still fails when the launcher has nothing, so adding a
/// stated value has not turned every source into one.
#[test]
fn a_snapshot_grant_still_cannot_carry_a_stated_value() {
    let specs = vec![env_grant("from_launcher", "ABSENT_VAR", Some("EXPOSED"))];
    let err = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect_err("a snapshot of nothing must still fail");
    assert_eq!(
        err,
        EnvironmentPolicyError::MissingEnv {
            name: "from_launcher".to_string(),
            var: "ABSENT_VAR".to_string(),
        }
    );
}

/// A credential cannot be declared as a constant. There is no registry of
/// names the secret vocabulary claims, so the guard keys on the reference
/// scheme, which is the one structural marker that exists.
///
/// The second half is the control: an ordinary value that merely mentions the
/// scheme later in the string is not a reference and must still be allowed,
/// or the guard would be a substring match on anything.
#[test]
fn a_literal_stating_a_secret_reference_is_refused() {
    let reference = format!("{}vault/key", crate::secrets::SECRET_REF_SCHEME);
    let specs = vec![literal_grant("smuggled", &reference, Some("TOKEN"))];
    let err = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect_err("a literal secret reference must be refused");
    assert_eq!(
        err,
        EnvironmentPolicyError::LiteralSecretReference {
            name: "smuggled".to_string(),
        }
    );

    let mentions = format!("see {} for details", crate::secrets::SECRET_REF_SCHEME);
    let specs = vec![literal_grant("prose", &mentions, Some("NOTE"))];
    let environment = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect("a value that merely mentions the scheme is not a reference");
    let resolved =
        crate::security::environment_policy::resolve_env(&environment, &no_env, &|_, _| None)
            .expect("resolve");
    assert_eq!(
        resolved.get("NOTE").map(String::as_str),
        Some(mentions.as_str())
    );
}

/// A literal is a grant, so the policy kind that forbids grants forbids this
/// one too. Without this the new source would be a way around `Isolated`.
#[test]
fn isolated_rejects_a_literal_like_any_other_grant() {
    let specs = vec![literal_grant("wrapper_off", "", Some("TOOL_WRAPPER"))];
    let err = SessionEnvironment::launch(EnvironmentPolicyKind::Isolated, specs, &no_env)
        .expect_err("isolated must reject a literal grant");
    assert!(
        matches!(err, EnvironmentPolicyError::PolicyForbidsGrants { .. }),
        "a literal must be refused as a grant, got {err:?}"
    );
}

/// The receipt says `literal`, so a reader can tell a stated value from a
/// snapshotted one without reading the declaration.
#[test]
fn a_literal_grant_names_its_source_in_the_receipt() {
    let specs = vec![literal_grant("wrapper_off", "", Some("TOOL_WRAPPER"))];
    let environment = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect("granted accepts a literal");
    let receipts = environment.receipts();
    let receipt = receipts
        .iter()
        .find(|receipt| receipt.name == "wrapper_off")
        .expect("the grant must be receipted");
    assert_eq!(receipt.source_kind, "literal");
}

#[test]
fn isolated_rejects_any_grant_at_launch() {
    let specs = vec![secret_grant("gh_token", "gh", "token", None)];
    let err = SessionEnvironment::launch(EnvironmentPolicyKind::Isolated, specs, &no_env)
        .expect_err("isolated must reject grants");
    assert_eq!(
        err,
        EnvironmentPolicyError::PolicyForbidsGrants {
            policy: EnvironmentPolicyKind::Isolated,
            attempted: 1
        }
    );

    // Both isolated constructors are structurally empty. The overall
    // session default remains inherited.
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Isolated, vec![], &no_env).unwrap();
    assert!(environment.is_isolated());
    assert!(environment.grants().is_empty());
    assert!(environment.receipts().is_empty());
    assert!(SessionEnvironment::isolated().grants().is_empty());
    assert_eq!(
        EnvironmentPolicyKind::default(),
        EnvironmentPolicyKind::Inherited
    );
}

#[test]
fn granted_policy_resolves_once_into_typed_record() {
    let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
    let specs = vec![
        env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
        secret_grant("gh_token", "gh", "token", Some("GH_TOKEN")),
    ];
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &env).unwrap();

    let grants = environment.grants();
    assert_eq!(grants.len(), 2);
    // Downstream reads the typed record without re-branching on the spec.
    assert_eq!(grants[0].name(), "fireworks");
    assert_eq!(grants[0].source_kind(), GrantSource::Env);
    assert_eq!(grants[0].exposed_env_var(), Some("FIREWORKS_API_KEY"));
    assert_eq!(grants[1].name(), "gh_token");
    assert_eq!(grants[1].source_kind(), GrantSource::SecretStore);
    assert_eq!(grants[1].exposed_env_var(), Some("GH_TOKEN"));

    // Exposure materializes uniform (VAR, value) pairs. The secret store
    // pointer is resolved here, once, through the embedder closure.
    let resolve_secret = |account: &str, key: &str| -> Option<String> {
        (account == "gh" && key == "token").then(|| "ghp-secret-token".to_string())
    };
    let mut pairs = environment.env_exposure(&resolve_secret).unwrap();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            (
                "FIREWORKS_API_KEY".to_string(),
                "fw-secret-value".to_string()
            ),
            ("GH_TOKEN".to_string(), "ghp-secret-token".to_string()),
        ]
    );
}

#[test]
fn secret_pointer_is_not_resolved_at_launch() {
    // Resolution of a secret_store grant must not read the value at launch.
    // A panicking secret resolver proves exposure is lazy, and an unexposed
    // grant never calls the resolver at all.
    let specs = vec![secret_grant("gh_token", "gh", "token", None)];
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env).unwrap();
    let never = |_: &str, _: &str| -> Option<String> {
        panic!("secret resolver must not run for an unexposed grant")
    };
    assert!(environment.env_exposure(&never).unwrap().is_empty());
}

#[test]
fn env_grant_snapshots_value_at_launch() {
    // The launcher env yields "live-at-launch"; the snapshot must hold that
    // value afterward — the child never reads the live environment.
    let at_launch = env_from(&[("TOKEN", "live-at-launch")]);
    let specs = vec![env_grant("t", "TOKEN", Some("TOKEN"))];
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &at_launch).unwrap();

    let never_secret = |_: &str, _: &str| -> Option<String> { None };
    let pairs = environment.env_exposure(&never_secret).unwrap();
    assert_eq!(
        pairs,
        vec![("TOKEN".to_string(), "live-at-launch".to_string())]
    );
    // The resolved grant is unaffected by any later env — it holds the
    // launch-time snapshot.
    assert_eq!(
        environment.env_exposure(&never_secret).unwrap(),
        vec![("TOKEN".to_string(), "live-at-launch".to_string())]
    );
}

#[test]
fn restricted_policies_do_not_retain_unrelated_launcher_values() {
    let snapshot = BTreeMap::from([
        ("PATH".to_string(), "/bin".to_string()),
        (
            "UNRELATED_SECRET".to_string(),
            "must-not-be-retained".to_string(),
        ),
    ]);
    let granted = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Granted,
        Vec::new(),
        snapshot,
        &no_env,
    )
    .unwrap();
    assert_eq!(granted.launcher_value("PATH"), Some("/bin"));
    assert_eq!(granted.launcher_value("UNRELATED_SECRET"), None);
}

/// The receipt must name what the run exposed, and never more than that.
/// A reader auditing a run cannot get this from the policy kind: two
/// granted sessions differ entirely in what they handed to a child.
#[test]
fn admitted_names_report_the_allowlisted_set_plus_grants_and_no_values() {
    let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
    let specs = vec![env_grant(
        "fireworks",
        "FIREWORKS_API_KEY",
        Some("FIREWORKS_API_KEY"),
    )];
    let snapshot = BTreeMap::from([
        ("PATH".to_string(), "/bin".to_string()),
        (
            "HARN_PROBE_FAKE_API_KEY".to_string(),
            "must-not-be-retained".to_string(),
        ),
    ]);
    let granted = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Granted,
        specs,
        snapshot,
        &env,
    )
    .unwrap();

    let names = granted.admitted_environment_names();
    assert!(names.contains(&"PATH".to_string()));
    assert!(
        names.contains(&"FIREWORKS_API_KEY".to_string()),
        "a grant that exposes a variable must appear in the receipt",
    );
    assert!(
        !names.contains(&"HARN_PROBE_FAKE_API_KEY".to_string()),
        "a name the allowlist refused must not be reported as admitted",
    );
    assert!(
        !names
            .iter()
            .any(|name| name.contains("fw-secret-value") || name.contains("must-not-be-retained")),
        "the receipt must carry names, never values",
    );
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted, "names must be sorted and deduplicated");
}

#[test]
fn receipts_record_shape_and_never_the_value() {
    let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
    let specs = vec![
        env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
        secret_grant("gh_token", "gh", "token", None),
    ];
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &env).unwrap();

    let receipts = environment.receipts();
    assert_eq!(
        receipts,
        vec![
            GrantReceipt {
                name: "fireworks".to_string(),
                source_kind: "env".to_string(),
                exposed_as_env: Some("FIREWORKS_API_KEY".to_string()),
                for_command: None,
                exposed_to: GrantAudience::Session,
            },
            GrantReceipt {
                name: "gh_token".to_string(),
                source_kind: "secret_store".to_string(),
                exposed_as_env: None,
                for_command: None,
                exposed_to: GrantAudience::Session,
            },
        ]
    );

    // The serialized receipts must never contain the snapshotted value or
    // the secret pointer. (SessionGrant/SessionEnvironment are not Serialize,
    // so this is also enforced at compile time; assert it at runtime too.)
    let json = serde_json::to_string(&receipts).unwrap();
    assert!(
        !json.contains("fw-secret-value"),
        "receipt leaked env value"
    );
    assert!(!json.contains("gh/token"), "receipt leaked secret pointer");
    assert!(json.contains("\"source_kind\":\"env\""));
    assert!(json.contains("\"source_kind\":\"secret_store\""));
}

#[test]
fn grant_spec_is_value_free_over_the_wire() {
    // A GrantSpec (the config contract) carries the env var NAME and the
    // secret pointer, never a value — safe to serialize into a config.
    let spec = env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY"));
    let json = serde_json::to_string(&spec).unwrap();
    let round: GrantSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(round, spec);
    assert!(json.contains("\"env\""));
    assert!(json.contains("FIREWORKS_API_KEY"));

    // Policy kind is a typed, defaulted config field (inherited by default).
    assert_eq!(
        serde_json::from_str::<EnvironmentPolicyKind>("\"granted\"").unwrap(),
        EnvironmentPolicyKind::Granted
    );
    assert_eq!(
        EnvironmentPolicyKind::default(),
        EnvironmentPolicyKind::Inherited
    );
}

#[test]
fn missing_env_source_fails_at_launch() {
    let specs = vec![env_grant("t", "ABSENT_VAR", None)];
    let err = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
        .expect_err("absent env var must fail resolution");
    assert_eq!(
        err,
        EnvironmentPolicyError::MissingEnv {
            name: "t".to_string(),
            var: "ABSENT_VAR".to_string(),
        }
    );
}

#[test]
fn resolve_rejects_empty_fields() {
    let env = env_from(&[("X", "v")]);
    assert_eq!(
        SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![env_grant("", "X", None)],
            &env
        ),
        Err(EnvironmentPolicyError::EmptyName)
    );
    assert_eq!(
        SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![env_grant("t", "", None)],
            &env
        ),
        Err(EnvironmentPolicyError::EmptyEnvVar {
            name: "t".to_string()
        })
    );
    assert_eq!(
        SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![secret_grant("t", "acct", "", None)],
            &env
        ),
        Err(EnvironmentPolicyError::EmptySecretRef {
            name: "t".to_string()
        })
    );
    assert_eq!(
        SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![env_grant("t", "X", Some(" "))],
            &env
        ),
        Err(EnvironmentPolicyError::EmptyExposeVar {
            name: "t".to_string()
        })
    );
}

#[test]
fn duplicate_names_and_targets_fail_with_stable_codes() {
    let env = env_from(&[("A", "a"), ("B", "b")]);
    let duplicate_name = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![
            env_grant("token", "A", Some("A")),
            env_grant("token", "B", Some("B")),
        ],
        &env,
    )
    .unwrap_err();
    assert_eq!(duplicate_name.code(), "environment_policy.duplicate_grant");

    let duplicate_target = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![
            env_grant("a", "A", Some("TOKEN")),
            env_grant("b", "B", Some("TOKEN")),
        ],
        &env,
    )
    .unwrap_err();
    assert_eq!(
        duplicate_target.code(),
        "environment_policy.duplicate_exposure_target"
    );
}

#[test]
fn child_policy_can_only_narrow_parent_authority() {
    let snapshot = BTreeMap::from([
        ("TOKEN".to_string(), "parent-value".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ]);
    let parent = SessionEnvironment::launch_from_snapshot(
        EnvironmentPolicyKind::Inherited,
        Vec::new(),
        snapshot.clone(),
        &|name| snapshot.get(name).cloned(),
    )
    .unwrap();
    let child = parent
        .narrow(
            EnvironmentPolicyKind::Granted,
            vec![env_grant("token", "TOKEN", Some("TOKEN"))],
        )
        .unwrap();
    assert_eq!(child.kind(), EnvironmentPolicyKind::Granted);
    assert_eq!(child.grants().len(), 1);

    let error = child
        .narrow(EnvironmentPolicyKind::Inherited, Vec::new())
        .unwrap_err();
    assert_eq!(error.code(), "environment_policy.child_exceeds_parent");
    assert_eq!(error.to_json()["parentPolicy"], "granted");
    assert_eq!(error.to_json()["requestedPolicy"], "inherited");

    let error = child
        .narrow(
            EnvironmentPolicyKind::Granted,
            vec![env_grant("other", "OTHER_TOKEN", Some("OTHER_TOKEN"))],
        )
        .unwrap_err();
    let diagnostic = error.to_json();
    assert_eq!(
        diagnostic["code"],
        "environment_policy.child_exceeds_parent"
    );
    assert_eq!(diagnostic["parentPolicy"], "granted");
    assert_eq!(diagnostic["requestedPolicy"], "granted");
    assert_eq!(diagnostic["grant"], "other");
    assert!(diagnostic["message"]
        .as_str()
        .unwrap()
        .contains("unchanged subset of the parent grants"));
}

#[test]
fn command_bound_grant_is_absent_from_session_exposure() {
    let resolve_secret = |account: &str, key: &str| -> Option<String> {
        (account == "gh" && key == "token").then(|| "ghp-secret-token".to_string())
    };
    let environment = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![
            env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
            command_grant("gh_token", "gh", "token", "GH_TOKEN", "gh"),
        ],
        &env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]),
    )
    .unwrap();

    // Ambient exposure keeps the provider key and hides the command-bound token.
    let ambient = environment.env_exposure(&resolve_secret).unwrap();
    assert_eq!(
        ambient,
        vec![(
            "FIREWORKS_API_KEY".to_string(),
            "fw-secret-value".to_string()
        )]
    );
    assert_eq!(
        environment
            .env_exposure_for("GH_TOKEN", &resolve_secret)
            .unwrap(),
        None
    );

    // Only a matching spawn sees GH_TOKEN.
    let mut for_gh = environment
        .env_exposure_for_command("gh", &resolve_secret)
        .unwrap();
    for_gh.sort();
    assert_eq!(
        for_gh,
        vec![
            (
                "FIREWORKS_API_KEY".to_string(),
                "fw-secret-value".to_string()
            ),
            ("GH_TOKEN".to_string(), "ghp-secret-token".to_string()),
        ]
    );
    let for_git = environment
        .env_exposure_for_command("/usr/bin/git", &resolve_secret)
        .unwrap();
    assert_eq!(
        for_git,
        vec![(
            "FIREWORKS_API_KEY".to_string(),
            "fw-secret-value".to_string()
        )]
    );
    assert!(environment
        .env_exposure_for_command("/usr/local/bin/gh", &resolve_secret)
        .unwrap()
        .into_iter()
        .any(|(var, _)| var == "GH_TOKEN"));
    assert_eq!(command_basename("C:\\Tools\\gh.exe"), "gh");

    let receipts = environment.receipts();
    assert_eq!(receipts[1].for_command.as_deref(), Some("gh"));
    assert_eq!(receipts[1].exposed_as_env.as_deref(), Some("GH_TOKEN"));
}

#[test]
fn for_command_requires_expose_and_rejects_paths() {
    let err = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![GrantSpec {
            name: "gh_token".to_string(),
            source: GrantSourceSpec::SecretStore {
                account: "gh".to_string(),
                key: "token".to_string(),
            },
            expose_as_env: None,
            for_command: Some("gh".to_string()),
            expose_to: Default::default(),
        }],
        &no_env,
    )
    .unwrap_err();
    assert_eq!(err.code(), "environment_policy.for_without_expose");

    let err = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![GrantSpec {
            name: "gh_token".to_string(),
            source: GrantSourceSpec::SecretStore {
                account: "gh".to_string(),
                key: "token".to_string(),
            },
            expose_as_env: Some("GH_TOKEN".to_string()),
            for_command: Some("/usr/bin/gh".to_string()),
            expose_to: Default::default(),
        }],
        &no_env,
    )
    .unwrap_err();
    assert_eq!(err.code(), "environment_policy.invalid_for_command");
}

/// Exercised directly, not through `launch` (which reads this process's
/// real `std::env::vars_os`), so it holds on every host.
#[test]
fn a_case_insensitive_insert_updates_the_existing_key_not_a_second_one() {
    let mut map = BTreeMap::new();
    map.insert(
        "Path".to_string(),
        "C:\\nodejs;C:\\Windows\\System32".to_string(),
    );
    insert_env_value_case_insensitive(
        &mut map,
        "PATH",
        "C:\\nodejs;C:\\Windows\\System32;C:\\extra".to_string(),
    );
    assert_eq!(
        map.len(),
        1,
        "must update the existing 'Path' key, not add a second 'PATH' key: {map:?}"
    );
    assert_eq!(
        map.get("Path").map(String::as_str),
        Some("C:\\nodejs;C:\\Windows\\System32;C:\\extra"),
        "the original casing is preserved, only the value is refreshed: {map:?}"
    );
    assert!(
        !map.contains_key("PATH"),
        "no second key should exist under the allowlist's own casing: {map:?}"
    );
}

#[test]
fn a_case_insensitive_insert_adds_a_new_key_when_none_matches() {
    let mut map = BTreeMap::new();
    insert_env_value_case_insensitive(&mut map, "HOME", "/root".to_string());
    assert_eq!(map.get("HOME").map(String::as_str), Some("/root"));
}

fn in_process_grant(name: &str, var: &str, expose: &str) -> GrantSpec {
    GrantSpec {
        expose_to: GrantAudience::InProcess,
        ..env_grant(name, var, Some(expose))
    }
}

/// The in-process audience is visible to Harn's own reader and to nothing a
/// child receives. The session-scoped grant beside it is the control: the
/// same builders must still hand that one to children, or the exclusion
/// below would pass on builders that hand children nothing at all.
#[test]
fn an_in_process_grant_reaches_the_in_process_reader_and_no_child() {
    let env = env_from(&[
        ("LAUNCHER_PROVIDER_KEY", "provider-value"),
        ("LAUNCHER_SESSION_KEY", "session-value"),
    ]);
    let environment = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![
            in_process_grant("provider", "LAUNCHER_PROVIDER_KEY", "PROVIDER_KEY"),
            env_grant("session", "LAUNCHER_SESSION_KEY", Some("SESSION_KEY")),
        ],
        &env,
    )
    .unwrap();
    let never = |_: &str, _: &str| None;

    assert_eq!(
        environment
            .env_exposure_for("PROVIDER_KEY", &never)
            .unwrap(),
        Some("provider-value".to_string()),
        "Harn's own reader must see the in-process grant",
    );
    for overlay in [
        environment.env_exposure(&never).unwrap(),
        environment
            .env_exposure_for_command("/usr/bin/env", &never)
            .unwrap(),
    ] {
        let names: Vec<&str> = overlay.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["SESSION_KEY"],
            "a child overlay must carry the session grant and never the in-process one",
        );
    }
    let names = environment.admitted_environment_names();
    assert!(names.contains(&"SESSION_KEY".to_string()));
    assert!(
        !names.contains(&"PROVIDER_KEY".to_string()),
        "admitted names describe what a child could see; the in-process name is not one",
    );
}

#[test]
fn an_in_process_grant_is_receipted_with_its_audience() {
    let env = env_from(&[("LAUNCHER_PROVIDER_KEY", "provider-value")]);
    let environment = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![in_process_grant(
            "provider",
            "LAUNCHER_PROVIDER_KEY",
            "PROVIDER_KEY",
        )],
        &env,
    )
    .unwrap();
    let receipts = environment.receipts();
    assert_eq!(receipts[0].exposed_to, GrantAudience::InProcess);
    assert_eq!(receipts[0].child_visible_env(), None);
    let json = serde_json::to_string(&receipts).unwrap();
    assert!(json.contains("\"exposed_to\":\"in_process\""), "{json}");
    assert!(!json.contains("provider-value"), "receipt leaked the value");

    // The default audience stays off the wire, so a record written before
    // audiences existed and one written now read back identically.
    let session = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![env_grant(
            "provider",
            "LAUNCHER_PROVIDER_KEY",
            Some("PROVIDER_KEY"),
        )],
        &env,
    )
    .unwrap();
    let json = serde_json::to_string(&session.receipts()).unwrap();
    assert!(!json.contains("exposed_to"), "{json}");
    assert_eq!(
        session.receipts()[0].child_visible_env(),
        Some("PROVIDER_KEY")
    );
}

#[test]
fn an_in_process_grant_needs_a_name_and_no_command() {
    let env = env_from(&[("LAUNCHER_PROVIDER_KEY", "provider-value")]);
    let without_expose = GrantSpec {
        expose_to: GrantAudience::InProcess,
        ..env_grant("provider", "LAUNCHER_PROVIDER_KEY", None)
    };
    assert_eq!(
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, vec![without_expose], &env),
        Err(EnvironmentPolicyError::InProcessWithoutExpose {
            name: "provider".to_string()
        })
    );
    let with_command = GrantSpec {
        for_command: Some("gh".to_string()),
        ..in_process_grant("provider", "LAUNCHER_PROVIDER_KEY", "PROVIDER_KEY")
    };
    let refusal =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, vec![with_command], &env)
            .unwrap_err();
    assert_eq!(
        refusal,
        EnvironmentPolicyError::InProcessWithCommand {
            name: "provider".to_string()
        }
    );
    assert_eq!(refusal.code(), "environment_policy.in_process_with_command");
}

#[test]
fn an_in_process_grant_spec_round_trips_and_narrows_unchanged() {
    let spec = in_process_grant("provider", "LAUNCHER_PROVIDER_KEY", "PROVIDER_KEY");
    let json = serde_json::to_value(&spec).unwrap();
    assert_eq!(json["expose_to"], "in_process");
    let parsed: GrantSpec = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, spec);

    let env = env_from(&[("LAUNCHER_PROVIDER_KEY", "provider-value")]);
    let parent =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, vec![spec.clone()], &env)
            .unwrap();
    // A child may keep the grant as declared...
    let child = parent
        .narrow(EnvironmentPolicyKind::Granted, vec![spec.clone()])
        .unwrap();
    assert_eq!(child.grants()[0].audience(), GrantAudience::InProcess);
    // ...but may not widen it to the session audience.
    let widened = GrantSpec {
        expose_to: GrantAudience::Session,
        ..spec
    };
    assert!(matches!(
        parent.narrow(EnvironmentPolicyKind::Granted, vec![widened]),
        Err(EnvironmentPolicyError::ChildPolicyExceedsParent { .. })
    ));
}
