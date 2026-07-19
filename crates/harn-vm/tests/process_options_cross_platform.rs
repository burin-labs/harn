mod support;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use harn_vm::orchestration::RunExecutionRecord;

#[test]
fn exec_opts_merges_env_with_parent_by_default() {
    let _parent = support::EnvironmentGuard::set("HARN_EXEC_OPTS_PARENT", "from-parent");
    let command = support::helper_command(&["--env", "HARN_EXEC_OPTS_PARENT", "CHILD"]);
    let source = format!(
        "\nconst result = exec_opts({command}, {{env: {{CHILD: \"from-child\"}}}})\nlog(result.stdout)\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["from-parent|from-child"]
    );
}

#[test]
fn exec_opts_replace_env_clears_parent() {
    let _parent = support::EnvironmentGuard::set("HARN_EXEC_OPTS_PARENT2", "from-parent");
    let command = support::helper_command(&["--env", "HARN_EXEC_OPTS_PARENT2", "CHILD"]);
    let source = format!(
        "\nconst result = exec_opts({command}, {{\n  env: {{CHILD: \"from-child\"}},\n  env_mode: \"replace\",\n}})\nlog(result.stdout)\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["|from-child"]
    );
}

#[test]
fn exec_at_opts_honors_directory() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command = support::helper_command(&["--cwd"]);
    let directory_arg = support::harn_quote(&directory.path().to_string_lossy());
    let source = format!(
        "\nconst result = exec_at_opts({directory_arg}, {command}, {{}})\nlog(result.stdout)\n"
    );

    let expected = std::fs::canonicalize(directory.path()).expect("canonical directory");
    let output = support::logged(&source).expect("exec_at_opts result");
    assert_eq!(output, vec![expected.to_string_lossy().into_owned()]);
}

#[test]
fn exec_uses_execution_context_cwd_and_env() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command = support::harn_quote(support::PROCESS_HELPER);
    let source = format!(
        "\nconst env_result = exec({command}, \"--env\", \"HARN_PROCESS_TEST\")\nconst cwd_result = exec({command}, \"--cwd\")\nlog(env_result.stdout + \"|\" + cwd_result.stdout)\n"
    );
    let context = RunExecutionRecord {
        cwd: Some(directory.path().to_string_lossy().into_owned()),
        env: BTreeMap::from([(String::from("HARN_PROCESS_TEST"), String::from("present"))]),
        ..Default::default()
    };
    let expected_cwd = std::fs::canonicalize(directory.path()).expect("canonical cwd");

    let output =
        support::logged_with_execution_context(&source, context).expect("execution-context result");
    assert_eq!(
        output,
        vec![format!("present|{}", expected_cwd.to_string_lossy())]
    );
}

#[test]
fn exec_at_resolves_relative_to_execution_cwd() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");
    let source = format!(
        "\nconst result = exec_at(\"nested\", {}, \"--cwd\")\nlog(result.stdout)\n",
        support::harn_quote(support::PROCESS_HELPER),
    );
    let context = RunExecutionRecord {
        cwd: Some(directory.path().to_string_lossy().into_owned()),
        ..Default::default()
    };

    let output =
        support::logged_with_execution_context(&source, context).expect("relative exec_at result");
    assert_eq!(
        output,
        vec![std::fs::canonicalize(nested)
            .expect("canonical nested directory")
            .to_string_lossy()
            .into_owned()]
    );
}

#[test]
fn exec_opts_enforces_timeout() {
    let command = support::helper_command(&["--sleep-ms", "5000"]);
    let source = format!(
        "\nconst result = exec_opts({command}, {{timeout: 50}})\nlog(result.timed_out)\nlog(result.success)\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["true", "false"]
    );
}

#[test]
fn exec_opts_rejects_invalid_commands() {
    assert!(support::run("const result = exec_opts([], {})").is_err());
    assert!(support::run("const result = exec_opts(\"not-a-list\", {})").is_err());
}

#[test]
fn exec_opts_interrupts_a_sleeping_child() {
    let cancel = Arc::new(AtomicBool::new(true));
    let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);
    // The Unix-only process-group test covers descendant cleanup; this probe
    // only needs a child that remains alive until the interrupt is observed.
    let command = support::helper_command(&["--sleep-ms", "30000"]);
    let source = format!(
        "\nconst result = exec_opts({command}, {{}})\nlog(result.timed_out)\nlog(result.success)\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["false", "false"]
    );
}
