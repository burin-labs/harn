use crate::security::session_environment::declare_session_environment_if_absent;
use crate::security::{EnvironmentPolicyKind, LauncherEnvironment};

#[test]
fn captured_sdk_empty_tokens_are_absent_and_granted_discovery_stays_disabled() {
    std::thread::spawn(|| {
        let launcher = LauncherEnvironment::from_snapshot(super::tests::map(&[
            ("AWS_ACCESS_KEY_ID", "synthetic-policy-access"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-policy-secret"),
            ("AWS_SESSION_TOKEN", " "),
            ("AWS_SECURITY_TOKEN", ""),
        ]));
        let inherited = launcher
            .launch(EnvironmentPolicyKind::Inherited, Vec::new())
            .expect("inherited positive");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("policy runtime");
        {
            let _environment = declare_session_environment_if_absent(inherited);
            let credentials = runtime
                .block_on(super::super::resolve_aws_credentials("us-east-1"))
                .expect("actual SDK static positive");
            assert_eq!(credentials.access_key_id, "synthetic-policy-access");
            assert_eq!(
                credentials.session_token, None,
                "empty legacy and canonical tokens stay absent"
            );
        }
        let granted = launcher
            .launch(EnvironmentPolicyKind::Granted, Vec::new())
            .expect("granted negative");
        let _environment = declare_session_environment_if_absent(granted);
        assert!(!super::super::implicit_discovery_allowed());
        assert!(
            runtime
                .block_on(super::super::resolve_aws_credentials("us-east-1"))
                .is_err(),
            "the same available inputs cannot bypass declared grants through SDK discovery"
        );
    })
    .join()
    .expect("policy control thread");
}
