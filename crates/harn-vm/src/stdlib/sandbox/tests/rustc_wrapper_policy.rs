use super::*;

const WRAPPER_KEYS: [&str; 4] = [
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

#[cfg(unix)]
#[derive(Default)]
struct Applications(std::sync::Mutex<Vec<crate::run_events::RunEvent>>);

#[cfg(unix)]
impl crate::run_events::RunEventSink for Applications {
    fn emit(&self, event: crate::run_events::RunEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[cfg(unix)]
#[test]
fn static_shell_cargo_keeps_the_wrapper_for_the_real_fixture_compilation() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().canonicalize().unwrap();
    std::fs::create_dir(cwd.join("src")).unwrap();
    std::fs::create_dir(cwd.join(".cargo")).unwrap();
    std::fs::write(cwd.join("Cargo.toml"),
        "[package]\nname = \"wrapper-shell-10122\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    std::fs::write(
        cwd.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"wrapper-shell-10122\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        cwd.join("src/lib.rs"),
        "#[test]\nfn wrapped_shell_fixture_reaches_assertion() { assert_eq!(40 + 2, 42); }\n",
    )
    .unwrap();
    let wrapper = cwd.join("record-wrapper");
    let marker = cwd.join("wrapper-arguments");
    // Cargo can run several wrappers at once; each records its own arguments.
    std::fs::create_dir(&marker).unwrap();
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}/\"$$\"\nexec \"$@\"\n",
            shell_words::quote(&marker.display().to_string()),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        cwd.join(".cargo/config.toml"),
        format!(
            "[build]\nrustc-wrapper = {}\n",
            serde_json::to_string(&wrapper.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let policy = CapabilityPolicy {
        workspace_roots: vec![cwd.display().to_string()],
        ..CapabilityPolicy::default()
    };
    let applications = Arc::new(Applications::default());
    crate::orchestration::push_execution_policy(policy);
    let output =
        futures::executor::block_on(crate::run_events::scope(applications.clone(), async {
            command_output(
                "/bin/sh",
                &[
                    "-c".into(),
                    "cargo test --offline --locked -- --nocapture".into(),
                ],
                &ProcessCommandConfig {
                    cwd: Some(cwd.clone()),
                    env: ["PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME"]
                        .into_iter()
                        .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value)))
                        .chain([
                            (
                                "CARGO_TARGET_DIR".into(),
                                cwd.join("target").display().to_string(),
                            ),
                            (
                                "CARGO_BUILD_BUILD_DIR".into(),
                                cwd.join("target/build").display().to_string(),
                            ),
                        ])
                        .collect(),
                    closed_env: true,
                    ..ProcessCommandConfig::default()
                },
            )
        }));
    crate::orchestration::pop_execution_policy();
    if crate::sandbox_host::assert_unavailable_result(&output) {
        return;
    }
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("test wrapped_shell_fixture_reaches_assertion ... ok"));
    let arguments: Vec<String> = std::fs::read_dir(marker)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert!(
        arguments
            .iter()
            .any(|invocation| invocation.contains("--crate-name\nwrapper_shell_10122\n")),
        "the wrapper must reach the actual fixture compiler, not only a probe: {arguments:#?}"
    );
    let applied = applications
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            crate::run_events::RunEvent::CompilerWrapperApplied { payload } => {
                Some(rustc_wrapper::RustcWrapperApplication::decode(payload.clone()).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        applied.len(),
        1,
        "the actual shell application must be reported"
    );
    assert_eq!(applied[0].program, "/bin/sh");
    assert_eq!(applied[0].decision.cwd, cwd.display().to_string());
    assert_eq!(
        applied[0].decision.disposition,
        rustc_wrapper::RustcWrapperDisposition::Kept
    );
    assert_eq!(
        applied[0].decision.wrapper.as_deref(),
        Some(wrapper.to_str().unwrap())
    );
    eprintln!("static-shell wrapper proof: actual fixture compilation, named assertion and kept application observed");
}

#[cfg(unix)]
#[test]
fn git_inventory_defers_wrapper_probe_and_cargo_reuses_it_across_tool_policies() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    let applications = Arc::new(Applications::default());

    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().canonicalize().unwrap();
    let wrapper = cwd.join("count-wrapper");
    let count = cwd.join("wrapper-count");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", count.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut policy = CapabilityPolicy {
        workspace_roots: vec![cwd.display().to_string()],
        ..CapabilityPolicy::default()
    };
    let config = ProcessCommandConfig {
        cwd: Some(cwd.clone()),
        env: vec![
            ("RUSTC_WRAPPER".into(), wrapper.display().to_string()),
            (
                "CARGO_TARGET_DIR".into(),
                cwd.join("target").display().to_string(),
            ),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                cwd.join("target/build").display().to_string(),
            ),
        ],
        ..ProcessCommandConfig::default()
    };
    crate::orchestration::push_execution_policy(policy.clone());
    let initialized = command_output("/usr/bin/git", &["init".into(), "-q".into()], &config);
    crate::orchestration::pop_execution_policy();
    if crate::sandbox_host::assert_unavailable_result(&initialized) {
        assert!(
            !count.exists(),
            "a refused launch must not run the wrapper probe"
        );
        return;
    }
    assert!(initialized.unwrap().status.success());
    let run = |policy: &CapabilityPolicy, program: &str, args: &[&str]| {
        crate::orchestration::push_execution_policy(policy.clone());
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let output =
            futures::executor::block_on(crate::run_events::scope(applications.clone(), async {
                command_output(program, &args, &config)
            }));
        crate::orchestration::pop_execution_policy();
        let output = output.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    std::fs::write(cwd.join("main.py"), "print('hello')\n").unwrap();
    let inventory = run(
        &policy,
        "/usr/bin/git",
        &["ls-files", "--others", "--exclude-standard"],
    );
    assert!(String::from_utf8_lossy(&inventory.stdout).contains("main.py"));
    let version = run(&policy, "cargo", &["--offline", "--version"]);
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("cargo "));
    assert!(
        !count.exists(),
        "Git inventory and Cargo version checks must not run the compiler wrapper probe"
    );
    assert!(!rustc_wrapper::rustc_wrapper_decisions()
        .iter()
        .any(|decision| decision.cwd == cwd.display().to_string()));

    std::fs::create_dir(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("Cargo.toml"), "[package]\nname = \"wrapper-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    std::fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
    run(&policy, "cargo", &["build", "--offline"]);
    assert!(
        cwd.join("target/debug/wrapper-fixture").is_file(),
        "the actual confined Cargo build must produce its executable"
    );
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "x",
        "the first Cargo launch must actually measure the wrapper"
    );
    policy.tools = vec!["different_tool".into()];
    policy.recursion_limit = Some(0);
    run(&policy, "cargo", &["build", "--offline"]);
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "x",
        "tool policy changes must reuse the measurement"
    );
    let applied = applications
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            crate::run_events::RunEvent::CompilerWrapperApplied { payload } => {
                Some(rustc_wrapper::RustcWrapperApplication::decode(payload.clone()).unwrap())
            }
            _ => None,
        })
        .filter(|application| {
            application.program == "cargo"
                && application.decision.disposition
                    == rustc_wrapper::RustcWrapperDisposition::Disabled
        })
        .collect::<Vec<_>>();
    assert_eq!(
        applied.len(),
        2,
        "both initial and cached applications must be emitted"
    );
    assert_eq!(
        applied[0], applied[1],
        "a cache hit carries the actual same applied decision"
    );
    std::fs::create_dir(cwd.join("extra")).unwrap();
    policy
        .process_sandbox
        .read_roots
        .push(cwd.join("extra").display().to_string());
    run(&policy, "cargo", &["build", "--offline"]);
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "xx",
        "changed process authority must remeasure"
    );

    let mut replacement = config.clone();
    replacement.closed_env = true;
    replacement.env.extend(
        ["PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value))),
    );
    replacement.env_remove.push("RUSTC_WRAPPER".into());
    crate::orchestration::push_execution_policy(policy.clone());
    let output = command_output("cargo", &["build".into(), "--offline".into()], &replacement);
    crate::orchestration::pop_execution_policy();
    assert!(output.unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "xx",
        "removed wrappers must reach neither probe nor real build"
    );
    replacement.env_remove.clear();
    crate::orchestration::push_execution_policy(policy);
    let output = command_output("cargo", &["build".into(), "--offline".into()], &replacement);
    crate::orchestration::pop_execution_policy();
    assert!(output.unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "xxx",
        "an explicit wrapper in a replacement environment must be measured"
    );
}

/// A wrapper that cannot run under the profile is switched off, and the
/// decision says which wrapper and why.
#[test]
fn sandboxed_process_config_switches_off_a_wrapper_that_cannot_run() {
    let _selector = handler_sandbox_test_guard();
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let caller_target = cwd.join("caller-target");
    let caller_build = cwd.join("caller-build");
    let config = ProcessCommandConfig {
        cwd: Some(cwd.clone()),
        env: vec![
            ("RUSTC_WRAPPER".into(), "/definitely/missing/wrapper".into()),
            (
                "CARGO_TARGET_DIR".into(),
                caller_target.display().to_string(),
            ),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                caller_build.display().to_string(),
            ),
        ],
        ..ProcessCommandConfig::default()
    };

    crate::orchestration::push_execution_policy(policy.clone());
    let resolved = sandboxed_process_config("cargo", &["build".into()], &config, &policy);
    crate::orchestration::pop_execution_policy();
    if crate::sandbox_host::unavailable_default_warn_worktree_sandbox().is_some() {
        // Preparation can disable an unusable wrapper without launching the
        // requested command. Measure the actual launch before requiring its
        // host-mechanism refusal.
        let output = match resolved {
            Ok(_) => {
                crate::orchestration::push_execution_policy(policy);
                let output = command_output("cargo", &["build".into()], &config);
                crate::orchestration::pop_execution_policy();
                output
            }
            Err(error) => Err(error),
        };
        assert!(crate::sandbox_host::assert_unavailable_result(&output));
        assert!(!caller_target.exists());
        assert!(!caller_build.exists());
        return;
    }
    let resolved = resolved.unwrap();
    let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
    let decision = rustc_wrapper::rustc_wrapper_decisions()
        .into_iter()
        .find(|decision| decision.cwd == cwd.display().to_string())
        .expect("the confined launch must record its actual environment's decision");
    assert_eq!(
        decision.disposition,
        rustc_wrapper::RustcWrapperDisposition::Disabled,
        "{decision:?}"
    );
    assert!(decision.disables(), "{decision:?}");
    assert!(
        decision.wrapper.as_deref().is_some_and(|wrapper| wrapper
            .replace('\\', "/")
            .ends_with("/definitely/missing/wrapper")),
        "{decision:?}"
    );
    for key in WRAPPER_KEYS {
        assert_eq!(env.get(key).map(String::as_str), Some(""), "{key}");
    }
    assert!(
        !caller_target.exists(),
        "the probe wrote its caller's target directory"
    );
    assert!(
        !caller_build.exists(),
        "the probe wrote its caller's build directory"
    );
}

#[test]
fn neutralize_rustc_wrapper_overrides_caller_supplied_wrapper() {
    // Even if a caller (or inherited env) asked for sccache, the sandboxed
    // config forces it off rather than appending a duplicate entry.
    let mut env = vec![
        ("RUSTC_WRAPPER".to_string(), "sccache".to_string()),
        (
            "CARGO_BUILD_RUSTC_WRAPPER".to_string(),
            "cargo-sccache".to_string(),
        ),
        (
            "RUSTC_WORKSPACE_WRAPPER".to_string(),
            "workspace-sccache".to_string(),
        ),
        (
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER".to_string(),
            "cargo-workspace-sccache".to_string(),
        ),
        ("PATH".to_string(), "/usr/bin".to_string()),
    ];
    let mut env_remove = vec![
        "RUSTC_WRAPPER".to_string(),
        "CARGO_BUILD_RUSTC_WRAPPER".to_string(),
        "RUSTC_WORKSPACE_WRAPPER".to_string(),
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER".to_string(),
    ];
    process_config::neutralize_rustc_wrapper(&mut env, &mut env_remove);
    let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
    for key in WRAPPER_KEYS {
        assert_eq!(collected.get(key).map(String::as_str), Some(""), "{key}");
        assert_eq!(
            env.iter().filter(|(existing, _)| existing == key).count(),
            1
        );
    }
    assert_eq!(collected.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert!(
        env_remove.is_empty(),
        "caller removal must not reveal a Cargo-configured wrapper"
    );
}

/// Only a decision that took away a configured wrapper is a warning. Having no
/// wrapper to begin with is the ordinary case and must not reach stderr as one.
#[test]
fn only_a_dropped_configured_wrapper_is_a_warning() {
    use rustc_wrapper::{RustcWrapperDecision, RustcWrapperDisposition};
    let decision = |disposition| RustcWrapperDecision {
        disposition,
        wrapper: None,
        reason: String::new(),
        cwd: String::new(),
    };
    assert!(!decision(RustcWrapperDisposition::NotConfigured).drops_configured_wrapper());
    assert!(!decision(RustcWrapperDisposition::Kept).drops_configured_wrapper());
    assert!(decision(RustcWrapperDisposition::Disabled).drops_configured_wrapper());
    assert!(decision(RustcWrapperDisposition::Unmeasured).drops_configured_wrapper());
}

/// When the wrapper-free build fails too, the reason says why. Without it an
/// `unmeasured` decision cannot tell a broken toolchain from a broken wrapper.
#[test]
fn an_unmeasured_decision_carries_the_wrapper_free_build_error() {
    let _selector = handler_sandbox_test_guard();
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    // A compiler Cargo cannot run fails both builds, and only the second
    // build's error names it without the wrapper in front of it.
    std::fs::create_dir_all(cwd.join(".cargo")).expect("cargo config dir");
    std::fs::write(
        cwd.join(".cargo").join("config.toml"),
        "[build]\nrustc = \"/definitely/missing/rustc\"\n",
    )
    .expect("cargo config");
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let env = vec![(
        "RUSTC_WRAPPER".to_string(),
        "/definitely/missing/wrapper".to_string(),
    )];

    crate::orchestration::push_execution_policy(policy.clone());
    let decision = rustc_wrapper::rustc_wrapper_decision(&policy, &cwd, &env);
    crate::orchestration::pop_execution_policy();
    if let Some(refusal) = crate::sandbox_host::unavailable_default_warn_worktree_sandbox() {
        assert_eq!(
            decision.disposition,
            rustc_wrapper::RustcWrapperDisposition::NotConfigured
        );
        assert_eq!(decision.wrapper, None);
        assert_eq!(
            decision.reason,
            format!(
                "cargo is not reachable from the child: SandboxMechanismUnavailable({refusal:?})"
            )
        );
        return;
    }
    assert_eq!(
        decision.disposition,
        rustc_wrapper::RustcWrapperDisposition::Unmeasured,
        "{decision:?}"
    );
    let without = decision
        .reason
        .split_once("without it: ")
        .map(|(_, without)| without)
        .unwrap_or_else(|| panic!("the reason must carry the wrapper-free error: {decision:?}"));
    assert!(
        without.contains("/definitely/missing/rustc") && !without.contains("missing/wrapper"),
        "{decision:?}"
    );
}

#[cfg(windows)]
#[test]
fn default_warn_executes_but_explicit_enforce_refuses_the_same_command() {
    crate::reset_thread_local_state();
    let selector = handler_sandbox_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().canonicalize().unwrap();
    let marker = cwd.join("executed.txt");
    let policy = CapabilityPolicy {
        workspace_roots: vec![cwd.display().to_string()],
        ..CapabilityPolicy::default()
    };
    let config = ProcessCommandConfig {
        cwd: Some(cwd),
        ..ProcessCommandConfig::default()
    };
    let args = ["/c".into(), "echo windows-warn-control>executed.txt".into()];
    let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
    crate::events::add_event_sink(sink.clone());
    crate::orchestration::push_execution_policy(policy.clone());
    let output = command_output("cmd.exe", &args, &config);
    crate::orchestration::pop_execution_policy();
    crate::events::reset_event_sinks();
    assert!(output.unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "windows-warn-control"
    );
    assert!(sink
        .logs
        .borrow()
        .iter()
        .any(|log| log.category == "handler_sandbox"
            && log.message
                == "this platform has no OS process sandbox; child processes run unconfined"));
    std::fs::remove_file(&marker).unwrap();

    selector.set("enforce");
    crate::orchestration::push_execution_policy(policy);
    let refused = command_output("cmd.exe", &args, &config);
    crate::orchestration::pop_execution_policy();
    assert!(crate::sandbox_host::assert_unavailable_result(&refused));
    assert!(
        !marker.exists(),
        "Enforce must refuse before executing the same command"
    );
}
