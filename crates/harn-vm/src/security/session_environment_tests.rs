//! Tests for [`super::session_environment`].
//!
//! Split out of `session_environment.rs` so that file stays under the
//! source-length cap. They are the same tests, in the same module, reached
//! through `#[path]` exactly as `stdlib/process_tests.rs` is.

use super::*;

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
    }
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
            },
            GrantReceipt {
                name: "gh_token".to_string(),
                source_kind: "secret_store".to_string(),
                exposed_as_env: None,
                for_command: None,
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
