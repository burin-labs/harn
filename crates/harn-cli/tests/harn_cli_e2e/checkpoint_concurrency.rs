//! Concurrent CLI processes must retain one shared initial checkpoint value.
use crate::test_util::process::harn_e2e_command;

#[test]
fn concurrent_processes_retain_one_checkpoint_candidate() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("campaign");
    std::fs::create_dir(&directory).unwrap();
    let aliases = vec![
        "campaign".to_owned(),
        "./campaign".to_owned(),
        directory.to_string_lossy().into_owned(),
        directory.join(".").to_string_lossy().into_owned(),
    ];
    #[cfg(unix)]
    let aliases = {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        [aliases, vec![alias.to_string_lossy().into_owned()]].concat()
    };
    std::fs::write(
        root.path().join("candidate.harn"),
        r#"fn main(harness: Harness) {
          const identity = harness.fs.canonicalize_existing(argv[1])
          const receipt = harness.runtime.checkpoint_insert(sha256(identity), json_parse(argv[0]))
          harness.stdio.println(json_stringify(receipt))
        }"#,
    )
    .unwrap();
    let ready = std::sync::Arc::new(std::sync::Barrier::new(8));
    let receipts = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|candidate| {
                let root = root.path();
                let ready = ready.clone();
                let alias = &aliases[candidate % aliases.len()];
                scope.spawn(move || {
                    let mut command = harn_e2e_command();
                    command
                        .current_dir(root)
                        .env("HARN_STATE_DIR", root.join("state"))
                        .env("HARN_SECRET_PROVIDERS", "env")
                        .env("HARN_LLM_CALLS_DISABLED", "1")
                        .env_remove("HARN_SPEND_POLICY")
                        .args(["run", "candidate.harn", "--", &candidate.to_string(), alias]);
                    ready.wait();
                    let output = command.output().unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(receipts.len(), 8);
    assert_eq!(receipts.iter().filter(|r| r["inserted"] == true).count(), 1);
    let retained = &receipts[0]["value"];
    assert!(retained
        .as_i64()
        .is_some_and(|value| (0..8).contains(&value)));
    assert!(receipts.iter().all(|receipt| &receipt["value"] == retained));
}
