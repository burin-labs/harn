//! Checkpoint ownership across independent CLI processes and state-root aliases.
use std::path::Path;
use std::process::Command;

use crate::test_util::process::harn_e2e_command;

fn command(root: &Path) -> Command {
    let mut command = harn_e2e_command();
    command
        .current_dir(root)
        .env("HARN_STATE_DIR", root.join("state"))
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env_remove("HARN_SPEND_POLICY");
    command
}

#[test]
fn checkpoint_mutations_share_the_runtime_autonomy_boundary() {
    let source = r#"fn main(harness: Harness) {
      harness.runtime.checkpoint("existing", 7)
      const receipt = harness.runtime.with_autonomy_policy(
        {agent_id: "checkpoint-autonomy", autonomy_tier: argv[0]},
        fn() {
          harness.runtime.checkpoint("existing", 8)
          const inserted = harness.runtime.checkpoint_insert("new", 9)
          const updated = harness.runtime.checkpoint_get("existing")
          harness.runtime.checkpoint_delete("existing")
          const deleted = harness.runtime.checkpoint_get("existing")
          harness.runtime.checkpoint_clear()
          return {inserted: inserted, updated: updated, deleted: deleted}
        },
      )
      harness.stdio.println(json_stringify({
        receipt: receipt,
        existing: harness.runtime.checkpoint_get("existing"),
        new_exists: harness.runtime.checkpoint_exists("new"),
      }))
    }"#;
    for tier in ["shadow", "suggest", "act_auto"] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("autonomy.harn"), source).unwrap();
        let output = command(root.path())
            .args(["run", "autonomy.harn", "--", tier])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let durable_path = root.path().join("state/checkpoints/autonomy.json");
        if tier == "act_auto" {
            assert_eq!(
                receipt["receipt"]["inserted"],
                serde_json::json!({"inserted": true, "value": 9})
            );
            assert_eq!(receipt["receipt"]["updated"], 8);
            assert!(receipt["receipt"]["deleted"].is_null());
            assert!(receipt["existing"].is_null());
            assert!(!durable_path.exists(), "act_auto must execute the clear");
        } else {
            assert!(
                receipt["receipt"]["inserted"].is_null(),
                "{tier}: {receipt}"
            );
            assert_eq!(receipt["receipt"]["updated"], 7, "{tier}: {receipt}");
            assert_eq!(receipt["receipt"]["deleted"], 7, "{tier}: {receipt}");
            assert_eq!(receipt["existing"], 7, "{tier}: {receipt}");
            let durable: serde_json::Value =
                serde_json::from_slice(&std::fs::read(durable_path).unwrap()).unwrap();
            assert_eq!(durable, serde_json::json!({"existing": 7}));
        }
        assert_eq!(receipt["new_exists"], false, "{tier}: {receipt}");
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("needs_human.harn"),
        r#"fn main(harness: Harness) {
          harness.runtime.with_autonomy_policy(
            {agent_id: "checkpoint-human", autonomy_tier: "act_auto", requires_human: true},
            fn() { harness.runtime.checkpoint_insert("candidate", 42) },
          )
        }"#,
    )
    .unwrap();
    let output = command(root.path())
        .args(["run", "needs_human.harn"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("HARN-AUT-NEEDS-HUMAN"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root
        .path()
        .join("state/checkpoints/needs_human.json")
        .exists());
}

#[test]
fn concurrent_processes_retain_one_checkpoint_candidate() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let aliases = vec![
        "state".to_owned(),
        "./state".to_owned(),
        state.to_string_lossy().into_owned(),
        state.join(".").to_string_lossy().into_owned(),
    ];
    #[cfg(unix)]
    let aliases = {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&state, &alias).unwrap();
        [aliases, vec![alias.to_string_lossy().into_owned()]].concat()
    };
    std::fs::write(
        root.path().join("candidate.harn"),
        r#"fn main(harness: Harness) {
          const receipt = harness.runtime.checkpoint_insert("candidate", json_parse(argv[0]))
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
                    let mut command = command(root);
                    command.env("HARN_STATE_DIR", alias).args([
                        "run",
                        "candidate.harn",
                        "--",
                        &candidate.to_string(),
                    ]);
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
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt["inserted"] == true)
            .count(),
        1
    );
    let retained = &receipts[0]["value"];
    assert!(retained
        .as_i64()
        .is_some_and(|value| (0..8).contains(&value)));
    assert!(receipts.iter().all(|receipt| &receipt["value"] == retained));
    let durable: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("checkpoints/candidate.json")).unwrap())
            .unwrap();
    assert_eq!(&durable["candidate"], retained);
}

#[test]
fn stale_cli_owner_preserves_an_independent_cli_write() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("owner.harn"),
        r#"fn main(harness: Harness) {
          if argv[1] == "writer" {
            harness.runtime.checkpoint("second", 2)
            harness.stdio.println("child checkpoint committed")
            return
          }
          harness.runtime.checkpoint("first", 1)
          const child = harness.process.run({
            program: argv[0],
            args: ["run", "owner.harn", "--", argv[0], "writer"],
          })
          assert(child.success, "independent CLI writer must succeed")
          harness.runtime.checkpoint("third", 3)
          assert(
            harness.runtime.checkpoint_get("second") == 2,
            "stale CLI owner must preserve the independently committed checkpoint",
          )
          harness.stdio.println(json_stringify({child_stdout: child.stdout, keys: harness.runtime.checkpoint_list()}))
        }"#,
    ).unwrap();
    let mut command = command(root.path());
    let binary = command.get_program().to_owned();
    let output = command
        .args([
            std::ffi::OsStr::new("run"),
            std::ffi::OsStr::new("owner.harn"),
            std::ffi::OsStr::new("--"),
            &binary,
            std::ffi::OsStr::new("parent"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["child_stdout"], "child checkpoint committed\n");
    assert_eq!(
        receipt["keys"],
        serde_json::json!(["first", "second", "third"])
    );
    let durable: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.path().join("state/checkpoints/owner.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        durable,
        serde_json::json!({"first": 1, "second": 2, "third": 3})
    );
}
