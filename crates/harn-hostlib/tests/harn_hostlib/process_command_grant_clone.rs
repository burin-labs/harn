//! The real grant factory and background waiter share one clone handoff.
//!
//! Only the child is mocked: it stays live until the test writes the exact
//! separate-Git-directory layout that Git produces and signals its exit.
//! Native confined Git transfer and credential isolation have their own VM
//! product-path tests.

use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner,
};
use harn_hostlib::tools::long_running::register_result_notifier;

use super::*;

struct FinishMock(MockHandleController);

impl Drop for FinishMock {
    fn drop(&mut self) {
        self.0.complete_with(ExitStatus::from_code(0));
    }
}

fn background_clone_case(destination_appeared: bool, cancelled: bool) {
    let _env_guard = super::super::process_tools_e2e::lock_env();
    // Unix-domain socket addresses have a small fixed byte limit. Keep the
    // real credential socket out of the much longer Cargo target temp path.
    let short_home = tempfile::Builder::new()
        .prefix(".h-git-")
        .tempdir_in(std::env::var_os("HOME").expect("real home"))
        .unwrap();
    let fixture = Fixture::with(|_| Paths {
        planted_path: None,
        harn_home: Some(short_home.path().to_path_buf()),
    });
    let environment = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![GrantSpec {
            name: "git_clone_probe".to_string(),
            source: GrantSourceSpec::SecretEnv {
                var: GRANTED.to_string(),
            },
            expose_as_env: Some(GRANTED.to_string()),
            for_command: Some("git".to_string()),
            expose_to: Default::default(),
        }],
        &|name| match name {
            GRANTED => Some(PROBE.to_string()),
            _ => std::env::var(name).ok(),
        },
    )
    .expect("launch a command-bound Git grant");
    harn_vm::stdlib::process::set_session_environment(Some(environment));
    let _session = in_session();
    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(MockProcessConfig::running());
    let _finish = FinishMock(controller.clone());
    let _spawner = install_spawner(spawner.clone());

    let mut request = fixture.request(fixture.workspace.path());
    request.put(
        "argv",
        vlist_str(&[
            "git",
            "clone",
            "--depth",
            "1",
            "https://github.com/burin-labs/probe.git",
            "checkout",
        ]),
    );
    request.put_bool("background", true);
    let started = require_dict(call_tool("hostlib_tools_run_command", request).unwrap());
    assert_eq!(require_str(&started, "status"), "running");
    let handle_id = require_str(&started, "handle_id");
    let result = register_result_notifier(&handle_id).expect("live clone result notifier");
    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    let spec = &captured[0];
    let repository = PathBuf::from(spec.env.get("GIT_DIR").expect("private Git directory"));
    assert!(
        spec.args
            .contains(&format!("--separate-git-dir={}", repository.display())),
        "background spawn must use the grant's prepared clone arguments"
    );
    assert!(!spec.env.contains_key(GRANTED));
    assert!(repository
        .parent()
        .unwrap()
        .join("credential.sock")
        .exists());

    let checkout = fixture.workspace("checkout");
    assert!(!checkout.exists(), "transfer must remain private");
    let marker = checkout.join(".git");
    let worktree = repository.with_file_name("worktree");
    assert_eq!(spec.args.last(), Some(&worktree.display().to_string()));
    std::fs::create_dir_all(&repository).unwrap();
    std::fs::write(repository.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(repository.join("config"), "[core]\n\tbare = false\n").unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", repository.display()),
    )
    .unwrap();
    if destination_appeared {
        std::fs::create_dir(&checkout).unwrap();
        std::fs::write(&marker, "foreign marker\n").unwrap();
    }
    assert!(
        result.try_recv().is_err(),
        "live child cannot publish completion"
    );

    if cancelled {
        let mut cancel = dict_map();
        cancel.insert("handle_id".into(), vm_string(&handle_id));
        let response = require_dict(call_tool("hostlib_tools_cancel_handle", cancel).unwrap());
        assert!(require_bool(&response, "cancelled"));
    } else {
        controller.complete_with(ExitStatus::from_code(0));
    }
    let terminal = require_dict(harn_clock::test_support::recv_within(
        "background clone finalization",
        &result,
    ));
    assert!(
        !repository.parent().unwrap().exists(),
        "grant custody ends after reaping"
    );
    if cancelled {
        assert_eq!(require_str(&terminal, "status"), "killed");
        assert!(
            !checkout.exists(),
            "cancelled clone never creates a public destination"
        );
    } else if destination_appeared {
        assert_eq!(require_str(&terminal, "status"), "blocked");
        assert_eq!(
            require_int(&terminal, "exit_code"),
            0,
            "preserve the observed child exit"
        );
        assert!(require_str(&terminal, "stderr").contains("command completion refused"));
        assert!(!terminal.contains_key("verifier_receipt"));
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "foreign marker\n"
        );
    } else {
        assert_eq!(require_str(&terminal, "status"), "completed");
        assert_eq!(require_int(&terminal, "exit_code"), 0);
        assert!(
            marker.is_dir(),
            "ordinary repository exists before success is published"
        );
        assert_eq!(
            std::fs::read_to_string(marker.join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert_eq!(
            std::fs::read_to_string(marker.join("config")).unwrap(),
            "[core]\n\tbare = false\n"
        );
    }
}

#[test]
fn background_clone_hands_off_git_directory_before_publishing_success() {
    background_clone_case(false, false);
}

#[test]
fn background_clone_new_destination_refuses_success_without_rewriting_child_exit() {
    background_clone_case(true, false);
}

#[test]
fn cancelled_background_clone_reaps_before_retiring_private_grant_state() {
    background_clone_case(false, true);
}
