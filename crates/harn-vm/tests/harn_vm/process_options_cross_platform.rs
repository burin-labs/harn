use crate::support;

use harn_vm::orchestration::RunExecutionRecord;
use std::collections::BTreeMap;

#[test]
fn exec_opts_merges_env_with_parent_by_default() {
    let _parent = support::EnvironmentGuard::set("HARN_EXEC_OPTS_PARENT", "from-parent");
    let program = support::harn_quote(&support::process_helper());
    let source = format!(
        "fn main(harness: Harness) {{\n  const result = harness.process.run({{program: {program}, args: [\"--env\", \"HARN_EXEC_OPTS_PARENT\", \"CHILD\"], env: {{CHILD: \"from-child\"}}}})\n  harness.stdio.log(result.stdout)\n}}\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["from-parent|from-child"]
    );
}

#[test]
fn exec_opts_replace_env_clears_parent() {
    let _parent = support::EnvironmentGuard::set("HARN_EXEC_OPTS_PARENT2", "from-parent");
    let program = support::harn_quote(&support::process_helper());
    let source = format!(
        "fn main(harness: Harness) {{\n  const result = harness.process.run({{program: {program}, args: [\"--env\", \"HARN_EXEC_OPTS_PARENT2\", \"CHILD\"], env: {{CHILD: \"from-child\"}}, env_mode: \"replace\"}})\n  harness.stdio.log(result.stdout)\n}}\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["|from-child"]
    );
}

#[test]
fn exec_at_opts_honors_directory() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let program = support::harn_quote(&support::process_helper());
    let directory_arg = support::harn_quote(&directory.path().to_string_lossy());
    let source = format!(
        "fn main(harness: Harness) {{\n  const result = harness.process.run({{program: {program}, args: [\"--cwd\"], cwd: {directory_arg}}})\n  harness.stdio.log(result.stdout)\n}}\n"
    );

    let expected = std::fs::canonicalize(directory.path()).expect("canonical directory");
    let output = support::logged(&source).expect("exec_at_opts result");
    assert_eq!(output, vec![expected.to_string_lossy().into_owned()]);
}

#[test]
fn exec_uses_execution_context_cwd_and_env() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command = support::harn_quote(&support::process_helper());
    let source = format!(
        "fn main(harness: Harness) {{\n  const env_result = harness.process.exec({command}, \"--env\", \"HARN_PROCESS_TEST\")\n  const cwd_result = harness.process.exec({command}, \"--cwd\")\n  harness.stdio.log(env_result.stdout + \"|\" + cwd_result.stdout)\n}}\n"
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
        "fn main(harness: Harness) {{\n  const result = harness.process.run({{program: {}, args: [\"--cwd\"], cwd: \"nested\"}})\n  harness.stdio.log(result.stdout)\n}}\n",
        support::harn_quote(&support::process_helper()),
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
    let program = support::harn_quote(&support::process_helper());
    let source = format!(
        "fn main(harness: Harness) {{\n  const result = harness.process.run({{program: {program}, args: [\"--sleep-ms\", \"5000\"], timeout_ms: 50}})\n  harness.stdio.log(result.timed_out)\n  harness.stdio.log(result.success)\n}}\n"
    );

    assert_eq!(
        support::logged(&source).expect("exec_opts result"),
        vec!["true", "false"]
    );
}

#[test]
fn exec_opts_rejects_invalid_commands() {
    assert!(
        support::run("fn main(harness: Harness) { harness.process.run({program: []}) }").is_err()
    );
    assert!(
        support::run("fn main(harness: Harness) { harness.process.run({program: 42}) }").is_err()
    );
}
