use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};

use crate::security::session_environment::declare_session_environment_if_absent;
use crate::security::{EnvironmentPolicyKind, LauncherEnvironment};

pub(super) fn map(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn run_pair(contexts: [(BTreeMap<String, String>, &str, &str, &str); 2]) {
    let barrier = Arc::new(Barrier::new(2));
    crate::runtime_stack::scope(|scope| {
        for (snapshot, access_key, token, region) in contexts {
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                let environment = LauncherEnvironment::from_snapshot(snapshot)
                    .launch(EnvironmentPolicyKind::Inherited, Vec::new())
                    .expect("captured inherited launch");
                let _environment = declare_session_environment_if_absent(environment);
                assert!(super::super::implicit_discovery_allowed());
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("SDK runtime");
                barrier.wait();
                for _ in 0..2 {
                    let credentials = runtime
                        .block_on(super::super::resolve_aws_credentials(region))
                        .expect("actual captured SDK credentials");
                    assert_eq!(credentials.access_key_id, access_key);
                    assert_eq!(credentials.secret_access_key, "synthetic-secret");
                    assert_eq!(credentials.session_token.as_deref(), Some(token));
                    assert_eq!(
                        runtime
                            .block_on(super::super::resolve_live_region(None))
                            .expect("actual captured SDK region"),
                        region
                    );
                }
            });
        }
    });
}

#[test]
fn captured_sdk_static_inputs_remain_distinct_in_parallel() {
    run_pair([
        (
            map(&[
                ("AWS_ACCESS_KEY_ID", "synthetic-first"),
                ("AWS_SECRET_ACCESS_KEY", "synthetic-secret"),
                ("AWS_SESSION_TOKEN", "synthetic-first-token"),
                ("AWS_SECURITY_TOKEN", "synthetic-ignored-token"),
                ("AWS_REGION", "us-east-1"),
            ]),
            "synthetic-first",
            "synthetic-first-token",
            "us-east-1",
        ),
        (
            map(&[
                ("AWS_ACCESS_KEY_ID", "synthetic-second"),
                ("AWS_SECRET_ACCESS_KEY", "synthetic-secret"),
                ("AWS_SECURITY_TOKEN", "synthetic-second-token"),
                ("AWS_REGION", "us-west-2"),
            ]),
            "synthetic-second",
            "synthetic-second-token",
            "us-west-2",
        ),
    ]);
}

#[test]
fn captured_sdk_profiles_remain_distinct_in_parallel() {
    let files = tempfile::tempdir().expect("isolated AWS profile files");
    let credentials = files.path().join("credentials");
    let config = files.path().join("config");
    std::fs::write(
        &credentials,
        "[synthetic-first]\naws_access_key_id=synthetic-first\naws_secret_access_key=synthetic-secret\naws_session_token=synthetic-first-token\n\
         [synthetic-second]\naws_access_key_id=synthetic-second\naws_secret_access_key=synthetic-secret\naws_session_token=synthetic-second-token\n",
    )
    .expect("synthetic credential fixture");
    std::fs::write(
        &config,
        "[profile synthetic-first]\nregion=us-east-1\n[profile synthetic-second]\nregion=us-west-2\n",
    )
    .expect("synthetic config fixture");
    let context = |profile: &str| {
        map(&[
            ("AWS_PROFILE", profile),
            ("AWS_CONFIG_FILE", config.to_str().expect("config path")),
            (
                "AWS_SHARED_CREDENTIALS_FILE",
                credentials.to_str().expect("credentials path"),
            ),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ])
    };
    run_pair([
        (
            context("synthetic-first"),
            "synthetic-first",
            "synthetic-first-token",
            "us-east-1",
        ),
        (
            context("synthetic-second"),
            "synthetic-second",
            "synthetic-second-token",
            "us-west-2",
        ),
    ]);
}

#[test]
fn captured_sdk_profile_helpers_receive_their_session_inputs() {
    let files = tempfile::tempdir().expect("isolated AWS helper files");
    let credentials = files.path().join("credentials");
    let config = files.path().join("config");
    let helper = files.path().join(if cfg!(windows) {
        "helper.cmd"
    } else {
        "helper.sh"
    });
    let response = |name: &str| {
        format!("{{\"Version\":1,\"AccessKeyId\":\"synthetic-{name}\",\"SecretAccessKey\":\"synthetic-secret\",\"SessionToken\":\"synthetic-{name}-token\"}}")
    };
    let script = if cfg!(windows) {
        format!(
            "@echo off\r\nif \"%AWS_PROFILE%\"==\"synthetic-first\" (echo {}) else if \"%AWS_PROFILE%\"==\"synthetic-second\" (echo {}) else (echo {})\r\n",
            response("first"), response("second"), response("unmatched")
        )
    } else {
        format!(
            "case \"$AWS_PROFILE\" in\nsynthetic-first) printf '%s' '{}' ;;\nsynthetic-second) printf '%s' '{}' ;;\n*) printf '%s' '{}' ;;\nesac\n",
            response("first"), response("second"), response("unmatched")
        )
    };
    std::fs::write(&helper, script).expect("synthetic helper");
    std::fs::write(&credentials, "").expect("no ambient profile credentials");
    let command = if cfg!(windows) {
        format!("\"{}\"", helper.display())
    } else {
        format!("/bin/sh \"{}\"", helper.display())
    };
    std::fs::write(
        &config,
        format!("[profile synthetic-first]\ncredential_process={command}\n[profile synthetic-second]\ncredential_process={command}\n"),
    )
    .expect("synthetic helper profile config");
    let launcher = LauncherEnvironment::capture()
        .launch(EnvironmentPolicyKind::Inherited, Vec::new())
        .expect("ordinary captured launcher inputs");
    let context = |profile: &str, region: &str| {
        let mut inputs = map(&[
            ("AWS_PROFILE", profile),
            ("AWS_CONFIG_FILE", config.to_str().expect("config path")),
            (
                "AWS_SHARED_CREDENTIALS_FILE",
                credentials.to_str().expect("credentials path"),
            ),
            ("AWS_REGION", region),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ]);
        // Preserve the trusted ordinary launcher inputs needed by shell helpers.
        for name in ["PATH", "SystemRoot", "ComSpec"] {
            if let Some(value) = launcher.launcher_value(name) {
                inputs.insert(name.to_string(), value.to_string());
            }
        }
        inputs
    };
    run_pair([
        (
            context("synthetic-first", "us-east-1"),
            "synthetic-first",
            "synthetic-first-token",
            "us-east-1",
        ),
        (
            context("synthetic-second", "us-west-2"),
            "synthetic-second",
            "synthetic-second-token",
            "us-west-2",
        ),
    ]);
}

#[test]
fn captured_sdk_context_debug_never_contains_inputs() {
    crate::runtime_stack::spawn(|| {
        let environment = LauncherEnvironment::from_snapshot(map(&[
            ("AWS_ACCESS_KEY_ID", "synthetic-debug-access"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-debug-secret"),
            (
                "AWS_CONTAINER_AUTHORIZATION_TOKEN",
                "synthetic-debug-container",
            ),
        ]))
        .launch(EnvironmentPolicyKind::Inherited, Vec::new())
        .expect("captured SDK context");
        let _environment = declare_session_environment_if_absent(environment);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("SDK debug runtime");
        let config = super::provider_config();
        let credentials = runtime.block_on(
            aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                .configure(config.clone())
                .region(aws_config::Region::new("us-east-1"))
                .build(),
        );
        let region = aws_config::default_provider::region::DefaultRegionChain::builder()
            .configure(&config)
            .build();
        let debug = format!("{config:?} {credentials:?} {region:?}");
        for input in [
            "synthetic-debug-access",
            "synthetic-debug-secret",
            "synthetic-debug-container",
        ] {
            assert!(
                !debug.contains(input),
                "SDK debug disclosed a captured input"
            );
        }
    })
    .join()
    .expect("debug control thread");
}
