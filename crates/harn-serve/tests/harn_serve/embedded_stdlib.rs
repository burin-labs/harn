#[test]
fn server_assembly_installs_stdlib_in_a_fresh_host() {
    const CHILD: &str = "EMBEDDED_STDLIB_ASSEMBLY_PROBE";
    if std::env::var_os(CHILD).is_some() {
        assert!(
            harn_stdlib_catalog::installed().is_err(),
            "the child must start without another host's installation"
        );
        let _server = harn_serve::AcpServer::new(harn_serve::AcpServerConfig::new(None));
        let catalog = harn_stdlib_catalog::installed()
            .expect("the real server constructor installs the catalog");
        assert!(catalog
            .source("agent/loop")
            .is_some_and(|source| !source.is_empty()));
        let _second = harn_serve::AcpServer::new(harn_serve::AcpServerConfig::new(None));
        assert!(
            std::ptr::eq(catalog, harn_stdlib_catalog::installed().unwrap()),
            "multiple server instances reuse one catalog"
        );
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "embedded_stdlib::server_assembly_installs_stdlib_in_a_fresh_host",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .status()
        .expect("start the isolated host probe");
    assert!(
        status.success(),
        "the host assembly probe must execute and succeed"
    );
}

#[test]
fn worker_assembly_installs_stdlib_in_a_fresh_host() {
    const CHILD: &str = "EMBEDDED_STDLIB_WORKER_PROBE";
    if std::env::var_os(CHILD).is_some() {
        assert!(
            harn_stdlib_catalog::installed().is_err(),
            "the worker must initialize itself without an ACP host"
        );
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("worker.harn");
        std::fs::write(
            &script,
            r#"
import "std/triggers"
@job("echo")
pub fn echo(harness: Harness, event: TriggerEvent) -> dict {
  return {echo: event.provider_payload.raw}
}
"#,
        )
        .unwrap();
        let runtime = harn_clock::test_support::current_thread_runtime();
        runtime.block_on(tokio::task::LocalSet::new().run_until(async {
            for number in [7, 8] {
                let request = serde_json::json!({"number": number});
                let outcome = harn_serve::run_job_once_with_options(
                    &script,
                    "echo",
                    request.clone(),
                    harn_serve::JobRunOptions::fail_fast(),
                    |_| {},
                )
                .await
                .expect("a standalone worker compiles and dispatches its job");
                assert!(outcome.succeeded(), "{outcome:?}");
                assert_eq!(outcome.attempt_count, 1);
                assert_eq!(outcome.result, Some(serde_json::json!({"echo": request})));
            }
        }));
        assert!(harn_stdlib_catalog::installed()
            .unwrap()
            .source("triggers")
            .is_some_and(|source| !source.is_empty()));
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "embedded_stdlib::worker_assembly_installs_stdlib_in_a_fresh_host",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .status()
        .expect("start the isolated worker probe");
    assert!(
        status.success(),
        "the fresh worker probe must execute and succeed"
    );
}
