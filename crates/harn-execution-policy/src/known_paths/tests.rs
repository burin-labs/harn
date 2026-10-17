use super::*;

#[test]
fn the_registry_parses_and_holds_its_invariants() {
    // A non-null control on the parse: a renamed table or an emptied list
    // would otherwise read as a working registry that denies nothing.
    assert!(credential_home_paths().len() >= 28);
    assert!(credential_basename_globs().len() >= 10);
    assert!(package_config_home_paths().len() >= 30);
    assert!(toolchain_cache_home_paths().len() >= 10);
    assert!(toolchain_read_home_paths(false).count() >= 15);
    assert!(toolchain_read_home_paths(true).count() > toolchain_read_home_paths(false).count());
    for entry in credential_home_paths() {
        assert!(credential_reason(entry).is_some(), "{entry} has no reason");
    }
    validate(registry()).expect("the shipped registry is valid");
}

#[test]
fn validation_refuses_a_grant_that_names_or_sits_inside_a_credential() {
    let parse = |extra: &str| -> Registry {
        let mut registry: Registry = toml::from_str(KNOWN_PATHS_TOML).unwrap();
        registry.package_config.home.push(extra.to_string());
        registry
    };
    for contradiction in [".npmrc", ".ssh/config", ".config/gcloud/configurations"] {
        let problem = validate(&parse(contradiction)).expect_err(contradiction);
        assert!(problem.contains("package_config.home"), "{problem}");
    }
    // A grant that merely contains a credential is the designed shape.
    validate(&parse(".docker")).expect("a parent of a credential may be granted");
}

#[test]
fn credential_paths_match_anywhere_and_only_on_whole_components() {
    for path in [
        "/home/u/.ssh/id_rsa",
        "~/.docker/config.json",
        "C:\\Users\\u\\.aws\\credentials",
        "/Users/u/.config/gcloud/application_default_credentials.json",
        "'/home/u/.netrc'",
        "/srv/app/.env.production",
        "/srv/app/tls/server.key",
        "deploy/secrets.yaml",
        "/HOME/U/.SSH",
    ] {
        assert!(is_credential_path(path), "{path} should be a credential");
    }
    for path in [
        "/proj/src/main.rs",
        "/proj/.environment.ts",
        "/home/u/.docker/daemon.json",
        "/home/u/.sshconfig-notes.md",
        "/home/u/.config/gh/config.yml",
        "",
        "/",
    ] {
        assert!(
            !is_credential_path(path),
            "{path} should not be a credential"
        );
    }
}
