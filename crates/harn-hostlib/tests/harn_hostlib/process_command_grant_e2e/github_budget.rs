use super::*;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibRegistry, RegisteredBuiltin};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct ApprovalRead {
    etag: PathBuf,
    manifest: PathBuf,
    change_policy: bool,
    marker: PathBuf,
    native_marker: PathBuf,
    calls: Arc<AtomicUsize>,
}

impl HostlibCapability for ApprovalRead {
    fn module_name(&self) -> &'static str {
        "tools"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let original = tools_registry()
            .find("hostlib_tools_read_file")
            .unwrap()
            .handler
            .clone();
        let etag = self.etag.clone();
        let manifest = self.manifest.clone();
        let change_policy = self.change_policy;
        let marker = self.marker.clone();
        let native_marker = self.native_marker.clone();
        let calls = self.calls.clone();
        registry.register(RegisteredBuiltin {
            name: "hostlib_tools_read_file",
            module: "tools",
            method: "read_file",
            handler: Arc::new(move |args| {
                let requested = args[0].as_dict().unwrap();
                assert_eq!(
                    requested.get("path").unwrap().display(),
                    marker.display().to_string()
                );
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    assert!(
                        !native_marker.exists(),
                        "approval precedes the native child"
                    );
                    if change_policy {
                        let mut owner: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
                        owner["policy_environment"] =
                            serde_json::json!({"GH_BUDGET_RESERVE": "4500"});
                        std::fs::write(&manifest, serde_json::to_vec(&owner).unwrap()).unwrap();
                    } else {
                        std::fs::write(&etag, "\"changed\"").unwrap();
                    }
                } else {
                    assert!(
                        !native_marker.exists(),
                        "stale approval started no native child"
                    );
                }
                original(args)
            }),
        });
    }
}

fn enroll(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let directory = fixture.user.path().join("bin");
    let native_directory = fixture.user.path().join("native");
    std::fs::rename(&directory, &native_directory).unwrap();
    std::fs::create_dir(&directory).unwrap();
    write_executable(&directory.join("gh"), "#!/bin/sh\nexit 97\n");
    write_executable(
        &directory.join("implementation.py"),
        "# installed ledger owner\n",
    );
    write_executable(
        &directory.join("adapter.py"),
        include_bytes!("../../fixtures/github_budget_owner.py"),
    );
    write_executable(
        &native_directory.join("gh.sh"),
        "#!/bin/sh\nset -eu\n\
         test -z \"${GH_TOKEN:-}${GITHUB_TOKEN:-}${GH_DEBUG:-}\"\n\
         test -f \"$GH_CONFIG_DIR/hosts.yml\"\n\
         grep -q oauth_token \"$GH_CONFIG_DIR/hosts.yml\"\n\
         printf '%s\\n' \"$@\" > native-argv\npwd > native-cwd\n\
         printf 'HTTP/1.1 200 OK\\r\\nETag: \"reply\"\\r\\n\\r\\n{\"reached\":true}\\n'\n",
    );
    let interpreter = std::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            "import os,sys; print(os.path.realpath(sys.executable))",
        ])
        .output()
        .expect("Python is required for the enrolled protocol peer");
    assert!(interpreter.status.success());
    let interpreter = PathBuf::from(String::from_utf8(interpreter.stdout).unwrap().trim());
    let state = fixture.user.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::write(state.join("etag"), "\"initial\"").unwrap();
    let account = fixture.user.path().join("home/.config/gh/hosts.yml");
    std::fs::write(&account, "github.com: {}\n").unwrap();
    let record = |path: &Path| {
        let path = std::fs::canonicalize(path).unwrap();
        serde_json::json!({"path": path, "sha256": format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()))})
    };
    let native = native_directory.join("gh");
    let owner = serde_json::json!({
        "schema": "github.budget-owner.v1",
        "policy_environment": {},
        "wrapper": record(&directory.join("gh")),
        "implementation": record(&directory.join("implementation.py")),
        "adapter": record(&directory.join("adapter.py")),
        "interpreter": record(&interpreter),
        "native_gh": record(&native),
        "state_directory": std::fs::canonicalize(&state).unwrap(),
        "account_config": std::fs::canonicalize(account).unwrap(),
    });
    std::fs::write(
        directory.join(".harn-github-budget-owner.json"),
        serde_json::to_vec(&owner).unwrap(),
    )
    .unwrap();
    (native, state)
}

#[test]
fn native_budget_consent_retains_argv_and_cwd_and_refuses_a_changed_etag() {
    assert_retained_consent(false);
}

#[test]
fn native_budget_consent_refuses_a_changed_installed_policy() {
    assert_retained_consent(true);
}

fn assert_retained_consent(change_policy: bool) {
    let _env = super::super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let _session = in_session();
    let (native, state) = enroll(&fixture);
    let marker = fixture.workspace("approval.txt");
    std::fs::write(&marker, "allow").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = HostlibRegistry::new()
        .with(harn_hostlib::tools::ToolsCapability)
        .with(ApprovalRead {
            etag: state.join("etag"),
            manifest: fixture
                .user
                .path()
                .join("bin/.harn-github-budget-owner.json"),
            change_policy,
            marker: marker.clone(),
            native_marker: fixture.workspace("native-argv"),
            calls: calls.clone(),
        });
    let source = include_str!("../../fixtures/github_budget_consent.harn");
    let chunk = harn_vm::compile_source(source).unwrap();
    let config = serde_json::json!({
        "cwd": fixture.workspace.path(), "native": native,
        "approval": marker,
        "changed_etag": if change_policy { "\"initial\"" } else { "\"changed\"" },
        "change_policy": change_policy,
    });
    let provider = Arc::new(
        harn_vm::secrets::MemorySecretProvider::new(SECRET_ACCOUNT).with_secret(
            harn_vm::secrets::SecretId::new(SECRET_ACCOUNT, SECRET_KEY),
            PROBE,
        ),
    );
    let mut vm = harn_vm::new_vm();
    harn_runtime::register_vm_stdlib(&mut vm);
    registry.register_into_vm(&mut vm);
    vm.set_harness(harn_vm::Harness::real());
    vm.set_global(
        "argv",
        VmValue::list(vec![VmValue::string(config.to_string())]),
    );
    let policy = harn_vm::orchestration::CapabilityPolicy {
        workspace_roots: vec![fixture.workspace.path().display().to_string()],
        sandbox_profile: harn_vm::orchestration::SandboxProfile::Worktree,
        ..Default::default()
    };
    harn_clock::test_support::current_thread_runtime()
        .block_on(harn_clock::test_support::with_local_tasks(
            harn_vm::secrets::with_active_secret_provider(
                Some(provider),
                harn_vm::orchestration::scope_execution_policy(policy, vm.execute(&chunk)),
            ),
        ))
        .expect("real consent, stale refusal and fresh native command must finish");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace("native-cwd"))
            .unwrap()
            .trim(),
        fixture.workspace.path().display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace("native-argv")).unwrap(),
        format!(
            "api\nrepos/example/repo/commits/pinned\n--include\n--header\nIf-None-Match: {}\n",
            if change_policy {
                "\"initial\""
            } else {
                "\"changed\""
            }
        )
    );
    assert!(vm.output().contains("fresh child reached"));
    assert!(!vm.output().contains(PROBE));
}

#[test]
fn native_budget_refuses_unsupported_commands_and_options_before_a_child() {
    let _env = super::super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let _session = in_session();
    enroll(&fixture);
    let mut cases = vec![
        vec!["gh", "auth", "token"],
        vec!["gh", "pr", "checks", "--watch"],
        vec!["gh", "api", "graphql"],
    ];
    for option in [
        "--paginate",
        "--slurp",
        "--include",
        "--silent",
        "--verbose",
    ] {
        cases.push(vec![
            "gh",
            "api",
            "repos/example/repo/commits/pinned",
            option,
        ]);
    }
    for (option, value) in [
        ("--jq", "."),
        ("--template", "{{.sha}}"),
        ("--method", "GET"),
        ("-X", "POST"),
        ("--field", "name=value"),
        ("--raw-field", "name=value"),
        ("--input", "-"),
        ("--cache", "1h"),
        ("--hostname", "github.com"),
        ("--header", "Accept: application/json"),
        ("--preview", "example"),
    ] {
        cases.push(vec![
            "gh",
            "api",
            "repos/example/repo/commits/pinned",
            option,
            value,
        ]);
    }
    assert_eq!(
        cases.len(),
        19,
        "every unsupported transport shape is exercised"
    );
    for argv in cases {
        let mut request = fixture.request(fixture.workspace.path());
        request.put("argv", vlist_str(&argv));
        let error = run(request)
            .err()
            .unwrap_or_else(|| panic!("unsupported budget invocation {argv:?} was accepted"));
        assert!(!error.contains(PROBE));
        assert!(
            !fixture.workspace("native-argv").exists(),
            "{argv:?} ran a native child"
        );
    }
}
