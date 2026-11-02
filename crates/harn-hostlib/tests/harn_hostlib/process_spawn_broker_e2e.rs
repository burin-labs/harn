//! Real-child coverage for the embedder spawn broker.
//!
//! The broker is process-global and installable once, so every scenario runs
//! in one test, in order: the unbrokered baseline first, then the same argv
//! after the broker is installed. It admits only marker argvs (and, to prove
//! the host refuses them first, shells and one catastrophic argv), so a sibling
//! test sharing this process under a threaded runner never matches.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_hostlib::process::{
    spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, ProcessError, SpawnSpec,
};
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::process_sandbox::{
    install_spawn_broker, SpawnBroker, SpawnBrokerAlreadyInstalled, SpawnBrokerGrant,
    SpawnBrokerRequest,
};
use harn_vm::value::{DictMap, VmDictExt as _};
use harn_vm::VmValue;

use super::process_tools_e2e::call;
use super::support::*;

const BROKER_ID: &str = "probe-broker";
const ENV_MARKER: &str = "HARN_SPAWN_BROKER_PROBE=1";
const DECLINED_MARKER: &str = "HARN_SPAWN_BROKER_DECLINED=1";
const CWD_MARKER: &str = "spawn-broker-grant-cwd-marker";
const WRITE_MARKER: &str = "spawn-broker-write-outside-workspace";
const INHERITED_PLAIN: &str = "PAGER";
const INHERITED_CREDENTIAL: &str = "EXAMPLE_CLI_TOKEN";

struct ProbeBroker {
    grant_cwd: PathBuf,
    consulted: Mutex<Vec<Vec<String>>>,
}

impl ProbeBroker {
    fn grant(&self) -> SpawnBrokerGrant {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        env.insert("GRANT_ONLY".to_string(), "granted".to_string());
        SpawnBrokerGrant {
            broker_id: BROKER_ID.to_string(),
            cwd: self.grant_cwd.clone(),
            env,
        }
    }

    fn consulted_programs(&self) -> Vec<String> {
        self.consulted
            .lock()
            .unwrap()
            .iter()
            .map(|argv| argv[0].clone())
            .collect()
    }

    fn was_consulted_for(&self, argv: &[&str]) -> bool {
        self.consulted
            .lock()
            .unwrap()
            .iter()
            .any(|seen| seen.iter().map(String::as_str).eq(argv.iter().copied()))
    }
}

impl SpawnBroker for ProbeBroker {
    fn admit(&self, request: &SpawnBrokerRequest<'_>) -> Option<SpawnBrokerGrant> {
        let mut argv = vec![request.program.to_string()];
        argv.extend(request.args.iter().cloned());
        self.consulted.lock().unwrap().push(argv);
        let args: Vec<&str> = request.args.iter().map(String::as_str).collect();
        let admitted = match (request.program, args.as_slice()) {
            ("env", [ENV_MARKER, "env"]) | ("ls", [CWD_MARKER]) => true,
            ("touch", [path]) => path.ends_with(WRITE_MARKER),
            // Would admit if asked: proves the refusal comes from the host.
            ("sh", _) | ("git", ["reset", "--hard"]) => true,
            _ => false,
        };
        admitted.then(|| self.grant())
    }
}

fn run(argv: &[&str], cwd: &std::path::Path, background: bool) -> DictMap {
    let mut request = dict_map();
    request.insert("argv".into(), vlist_str(argv));
    request.insert("cwd".into(), vm_string(&path_str(cwd)));
    request.insert("env_mode".into(), vm_string("patch"));
    let mut env = dict_map();
    env.insert("SPEC_ONLY".into(), vm_string("from-spec"));
    request.put("env", VmValue::dict(env));
    request.put("env_remove", VmValue::list(vec![vm_string("GRANT_ONLY")]));
    if background {
        request.put_bool("background", true);
        let started = call("hostlib_tools_run_command", request).expect("start background");
        assert_response_matches_schema("run_command", &started);
        let started = require_dict(started);
        let mut wait = dict_map();
        wait.insert(
            "handle_id".into(),
            vm_string(&require_str(&started, "handle_id")),
        );
        wait.put_int("timeout_ms", 10_000);
        let waited = call("hostlib_tools_wait_command", wait).expect("wait background");
        assert_response_matches_schema("wait_command", &waited);
        let waited = require_dict(waited);
        assert_eq!(
            broker_of(&started),
            broker_of(&waited),
            "a background handle keeps its spawn-time sandbox receipt"
        );
        waited
    } else {
        let response = call("hostlib_tools_run_command", request).expect("run command");
        assert_response_matches_schema("run_command", &response);
        require_dict(response)
    }
}

fn broker_of(response: &DictMap) -> Option<String> {
    require_nested_dict(response, "sandbox")
        .get("broker")
        .map(|value| as_string(value).to_string())
}

fn env_lines(response: &DictMap) -> Vec<String> {
    require_str(response, "stdout")
        .lines()
        .map(str::to_string)
        .collect()
}

fn assert_unbrokered(response: &DictMap, expected_enforced: bool) {
    let sandbox = require_nested_dict(response, "sandbox");
    assert!(sandbox.get("broker").is_none(), "{sandbox:?}");
    assert_eq!(require_bool(&sandbox, "enforced"), expected_enforced);
}

fn assert_brokered_env(response: &DictMap) {
    assert_eq!(require_int(response, "exit_code"), 0, "{response:?}");
    // The child's environment is exactly the grant, plus the marker the
    // `env NAME=value` argv sets and Harn's private process-tree tokens.
    let mut names: Vec<String> = env_lines(response)
        .iter()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
        .collect();
    names.sort();
    let mut expected = vec![
        "GRANT_ONLY".to_string(),
        "HARN_SPAWN_BROKER_PROBE".to_string(),
        "PATH".to_string(),
        harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV.to_string(),
    ];
    let owner_token = harn_vm::op_interrupt::PROCESS_OWNER_TOKEN_ENV.to_string();
    if names.contains(&owner_token) {
        expected.push(owner_token);
    }
    expected.sort();
    assert_eq!(names, expected, "{response:?}");
    let lines = env_lines(response);
    assert!(
        lines.iter().any(|line| line == "GRANT_ONLY=granted"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "PATH=/usr/bin:/bin"),
        "{lines:?}"
    );
    let sandbox = require_nested_dict(response, "sandbox");
    assert!(!require_bool(&sandbox, "enforced"), "{sandbox:?}");
    assert_eq!(require_str(&sandbox, "denial_reporting"), "not_enforced");
    assert_eq!(require_str(&sandbox, "broker"), BROKER_ID);
}

#[test]
fn spawn_broker_runs_admitted_argv_unconfined_and_nothing_else() {
    // Host variables a brokered child must not inherit: an ordinary one the
    // confined path does pass through, and a credential-shaped one.
    let _env_guard = super::process_tools_e2e::lock_env();
    struct RestoreEnv;
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            // SAFETY: serialized by the shared environment lock held above.
            unsafe {
                std::env::remove_var(INHERITED_PLAIN);
                std::env::remove_var(INHERITED_CREDENTIAL);
            }
        }
    }
    // SAFETY: serialized by the shared environment lock held above.
    unsafe {
        std::env::set_var(INHERITED_PLAIN, "host-value");
        std::env::set_var(INHERITED_CREDENTIAL, "host-credential");
    }
    let _restore_env = RestoreEnv;
    let _session = harn_vm::agent_sessions::enter_current_session(format!(
        "test-spawn-broker-{}",
        std::process::id()
    ));
    let workspace = tempfile::tempdir().expect("workspace");
    let grant_dir = tempfile::tempdir().expect("grant cwd");
    std::fs::write(grant_dir.path().join(CWD_MARKER), b"").expect("cwd marker");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![path_str(workspace.path())],
        ..CapabilityPolicy::default()
    });
    struct PopPolicy;
    impl Drop for PopPolicy {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let _policy = PopPolicy;
    let confined_enforced = harn_vm::process_sandbox::ProcessSandboxReportingContext::current()
        .report()
        .enforced;
    let env_argv = ["env", ENV_MARKER, "env"];
    let ls_argv = ["ls", CWD_MARKER];
    // Outside every workspace root, so the confined path cannot write it.
    let outside = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("outside dir");
    let write_target = outside.path().join(WRITE_MARKER);
    let write_target_str = path_str(&write_target);
    let touch_argv = ["touch", write_target_str.as_str()];

    // (a) No broker installed: the marker argvs take the normal path.
    let baseline = run(&env_argv, workspace.path(), false);
    assert_unbrokered(&baseline, confined_enforced);
    let lines = env_lines(&baseline);
    assert!(
        lines.iter().any(|line| line == "SPEC_ONLY=from-spec"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == &format!("{INHERITED_PLAIN}=host-value")),
        "the confined path inherits host variables, so their absence below is measured: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("GRANT_ONLY=")),
        "{lines:?}"
    );
    let baseline_ls = run(&ls_argv, workspace.path(), false);
    assert_unbrokered(&baseline_ls, confined_enforced);
    assert_ne!(
        require_int(&baseline_ls, "exit_code"),
        0,
        "the marker exists only in the grant cwd"
    );
    let baseline_touch = run(&touch_argv, workspace.path(), false);
    assert_unbrokered(&baseline_touch, confined_enforced);
    if confined_enforced {
        assert!(
            !write_target.exists(),
            "confinement must refuse a write outside the workspace: {baseline_touch:?}"
        );
    }

    let broker = Arc::new(ProbeBroker {
        grant_cwd: grant_dir.path().to_path_buf(),
        consulted: Mutex::new(Vec::new()),
    });
    install_spawn_broker(broker.clone()).expect("first install");
    assert_eq!(
        install_spawn_broker(broker.clone()),
        Err(SpawnBrokerAlreadyInstalled),
        "a second install is refused"
    );

    // (b) Admitted: only the grant env, the grant cwd, and a brokered receipt,
    // on both the direct path and the owner-death guardian path.
    assert_brokered_env(&run(&env_argv, workspace.path(), false));
    assert_brokered_env(&run(&env_argv, workspace.path(), true));
    let brokered_ls = run(&ls_argv, workspace.path(), false);
    assert_eq!(require_int(&brokered_ls, "exit_code"), 0, "{brokered_ls:?}");
    assert_eq!(require_str(&brokered_ls, "stdout").trim(), CWD_MARKER);
    assert_eq!(
        require_str(&require_nested_dict(&brokered_ls, "sandbox"), "broker"),
        BROKER_ID
    );
    let grant_name = grant_dir.path().file_name().unwrap().to_string_lossy();
    assert!(
        require_str(&brokered_ls, "cwd").ends_with(grant_name.as_ref()),
        "the receipt names the directory the child ran in: {brokered_ls:?}"
    );
    let brokered_touch = run(&touch_argv, workspace.path(), false);
    assert_eq!(
        require_int(&brokered_touch, "exit_code"),
        0,
        "{brokered_touch:?}"
    );
    assert!(
        write_target.exists(),
        "an admitted spawn runs outside confinement"
    );

    // (c) A shell is never offered to the broker, even one it would admit.
    let shell = run(&["sh", "-c", "echo shell-ran"], workspace.path(), false);
    assert_eq!(require_str(&shell, "stdout").trim(), "shell-ran");
    assert_unbrokered(&shell, confined_enforced);
    assert!(
        !broker
            .consulted_programs()
            .iter()
            .any(|program| program == "sh"),
        "{:?}",
        broker.consulted_programs()
    );

    // (d) Declined: consulted, then the normal confined path.
    let declined_argv = ["env", DECLINED_MARKER, "env"];
    let declined = run(&declined_argv, workspace.path(), false);
    assert!(broker.was_consulted_for(&declined_argv));
    assert_unbrokered(&declined, confined_enforced);
    let lines = env_lines(&declined);
    assert!(
        lines.iter().any(|line| line == "SPEC_ONLY=from-spec"),
        "{lines:?}"
    );

    // (e) The catastrophic floor refuses before the broker is asked.
    let refused = spawn_process(SpawnSpec {
        builtin: "spawn_broker_test",
        program: "git".to_string(),
        args: vec!["reset".to_string(), "--hard".to_string()],
        cwd: Some(workspace.path().to_path_buf()),
        env: BTreeMap::new(),
        env_remove: Vec::new(),
        env_mode: EnvMode::Replace,
        use_stdin: false,
        configure_process_group: false,
        owner_death: OwnerDeathPolicy::None,
        output_capture: OutputCapture::Pipe,
    });
    assert!(
        matches!(refused, Err(ProcessError::CatastrophicFloor(_))),
        "the floor must refuse first"
    );
    assert!(
        !broker
            .consulted_programs()
            .iter()
            .any(|program| program == "git"),
        "{:?}",
        broker.consulted_programs()
    );
}
