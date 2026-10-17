use harn_vm::security::{
    lookup_env_read, EnvironmentPolicyKind, EnvironmentRead, SessionEnvironment,
};
use std::collections::BTreeMap;

#[test]
fn bounded_read_matches_platform_admission_for_set_and_unset_names() {
    let never_secret = |_: &str, _: &str| -> Option<String> {
        panic!("an allowlist read must not resolve a secret")
    };
    for policy in [
        EnvironmentPolicyKind::Isolated,
        EnvironmentPolicyKind::Granted,
    ] {
        for value in [None, Some("admitted-locale-sentinel")] {
            let snapshot: BTreeMap<String, String> = value
                .map(|value| ("LC_MESSAGES".into(), value.into()))
                .into_iter()
                .collect();
            let environment = SessionEnvironment::launch_from_snapshot(
                policy,
                vec![],
                snapshot.clone(),
                &|name| snapshot.get(name).cloned(),
            )
            .unwrap();

            match lookup_env_read(&environment, "LC_MESSAGES", &never_secret).unwrap() {
                EnvironmentRead::Permitted(actual) => assert_eq!(actual.as_deref(), value),
                EnvironmentRead::Refused(_) => panic!("canonical name must be admitted"),
            }
            match lookup_env_read(&environment, "lc_messages", &never_secret).unwrap() {
                EnvironmentRead::Permitted(actual) if cfg!(windows) => {
                    assert_eq!(actual.as_deref(), value);
                }
                EnvironmentRead::Refused(actual) if !cfg!(windows) => assert_eq!(actual, policy),
                _ => panic!("read admission disagrees with platform name matching"),
            }
            assert!(matches!(
                lookup_env_read(&environment, "UNGRANTED_LOCALE", &never_secret).unwrap(),
                EnvironmentRead::Refused(actual) if actual == policy
            ));
        }
    }
}
