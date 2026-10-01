//! The package-manager preset admits measured tool roots, not unknown XDG
//! siblings. Exercise the canonical CLI and a real OS-confined child.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use crate::test_util::process::harn_e2e_command;

#[test]
fn package_manager_preset_refuses_unlisted_xdg_siblings_and_keeps_credential_denials() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let files = [
        (".config/git/config", "CONFIG"),
        (".cache/pip/census", "CACHE"),
        (".config/unlisted-census/token", "UNLISTED_CONFIG"),
        (".cache/unlisted-census/token", "UNLISTED_CACHE"),
        (".config/composer/config.json", "COMPOSER"),
        (".config/composer/auth.json", "DUMMY_AUTH"),
        (".config/coursier/mirror.properties", "MIRROR"),
        (
            ".config/coursier/credentials.properties",
            "DUMMY_COURSIER_AUTH",
        ),
        (".config/swiftpm/configuration/registries.json", "REGISTRY"),
        (".config/swiftpm/security/token", "DUMMY_SWIFT_AUTH"),
    ];
    for (relative, content) in files {
        let path = home.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    // Git's config discovery reads this before any spawn. Keep the control a
    // valid config and verify its exact contents in the child.
    std::fs::write(
        home.join(".config/git/config"),
        "[census]\nmarker = CONFIG\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("probe.harn"),
        concat!(
            "fn main(harness: Harness) {\n",
            "  for path in argv {\n",
            "    const result = harness.process.run({program: \"/bin/cat\", args: [path]})\n",
            "    harness.stdio.println(to_string(result.success) + \":\" + trim(result.stdout))\n",
            "  }\n",
            "}\n",
        ),
    )
    .unwrap();
    let run = |explicit_parent_grants: bool| {
        let mut command = harn_e2e_command();
        command
            .current_dir(&workspace)
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_SYSTEM")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HARN_CACHE_DIR", workspace.join("package-cache"))
            .args(["run", "--standalone", "probe.harn"]);
        if explicit_parent_grants {
            command
                .arg("--sandbox-read-root")
                .arg(home.join(".config"))
                .arg("--sandbox-read-root")
                .arg(home.join(".cache"));
        }
        command.arg("--");
        let probes = if explicit_parent_grants {
            &files[..6]
        } else {
            &files[..]
        };
        for (relative, _) in probes {
            command.arg(home.join(relative));
        }
        let output = command.output().expect("run canonical Harn CLI");
        assert!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };

    let confined = run(false);
    assert_eq!(
        confined,
        "true:[census]\nmarker = CONFIG\ntrue:CACHE\nfalse:\nfalse:\ntrue:COMPOSER\nfalse:\ntrue:MIRROR\nfalse:\ntrue:REGISTRY\nfalse:\n",
        "known roots must be readable, unknown siblings and credentials refused"
    );

    // Restore the old broad authority explicitly. Unknown files must become
    // readable, proving that their refusal came from the narrowed grants.
    // Composer auth stays denied even inside an explicitly admitted parent.
    let broad = run(true);
    assert_eq!(
        broad,
        "true:[census]\nmarker = CONFIG\ntrue:CACHE\ntrue:UNLISTED_CONFIG\ntrue:UNLISTED_CACHE\ntrue:COMPOSER\nfalse:\n"
    );
}
