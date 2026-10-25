//! `doas` as a privilege wrapper: the floor looks through it and the scan labels it.

use super::*;

#[test]
fn floor_does_not_block_a_doas_permit_check_that_launches_nothing() {
    for cmd in [
        "doas -C /etc/doas.conf rm -rf /",
        "doas -C/etc/doas.conf rm -rf /",
        "doas -nC /etc/doas.conf rm -rf /",
        "doas -nC/etc/doas.conf rm -rf /",
    ] {
        assert!(!is_cat_root(cmd), "{cmd}");
    }
    for cmd in [
        "doas -uroot rm -rf /",
        "doas -nuroot rm -rf /",
        "doas -n -u root rm -rf /",
    ] {
        assert!(is_cat_root(cmd), "{cmd}");
    }
    assert!(is_cat_root("doas -u root rm -rf /"));
}

#[test]
fn the_privilege_label_matches_the_doas_word_not_a_prefix() {
    for command in ["doas-helper --version", "echo doasdoas", "ls doas.conf"] {
        let scan = command_risk_scan_json(&shell_ctx(command), None);
        assert!(!labels(&scan).contains(&"sudo".to_string()), "{command}");
    }
    for command in [
        "doas ls",
        "true && doas ls",
        "echo x; doas ls",
        "/usr/bin/doas ls",
        "d\\oas ls",
        "env doas ls",
    ] {
        let scan = command_risk_scan_json(&shell_ctx(command), None);
        assert!(labels(&scan).contains(&"sudo".to_string()), "{command}");
    }
}

#[test]
fn deterministic_scan_projects_privilege_wrappers_to_the_launched_command() {
    for (command, argv) in [
        (
            "sudo -u builder git stash",
            serde_json::json!(["git", "stash"]),
        ),
        (
            "doas -u builder git stash",
            serde_json::json!(["git", "stash"]),
        ),
        (
            "doas -n -- git reset --hard",
            serde_json::json!(["git", "reset", "--hard"]),
        ),
    ] {
        let scan = command_risk_scan_json(&shell_ctx(command), None);
        assert_eq!(
            scan["shell_command_groups"][0][0]["argv"], argv,
            "{command}"
        );
        assert!(labels(&scan).contains(&"sudo".to_string()), "{command}");
    }
}
