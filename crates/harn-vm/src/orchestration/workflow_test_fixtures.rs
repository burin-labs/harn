//! Inline workflow bundle fixtures shared across `harn-vm` tests.
//!
//! Tests in this crate must not `include_str!` outside the crate root —
//! `cargo package` rejects it because the included file is not part of
//! the published tarball. The canonical user-facing fixture lives at
//! `docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json`; we
//! duplicate it here as a string constant so in-crate tests stay
//! self-contained.
//!
//! `tests::pr_monitor_fixture_matches_canonical_docs_fixture` is the
//! drift guard: it lives in the workspace test suite (not in the
//! published crate), reads the canonical docs file at test time, and
//! asserts byte-equality after JSON normalization. Touching either
//! copy without updating the other breaks CI.

use super::workflow_bundle::WorkflowBundle;

pub(crate) const PR_MONITOR_BUNDLE_JSON: &str = r#"{
  "schema_version": 2,
  "entrypoint": "workflows/github-pr-monitor.harn",
  "transitive_modules": [
    {
      "path": "workflows/github-pr-monitor.harn",
      "source_hash_blake3": "blake3:1111111111111111111111111111111111111111111111111111111111111111",
      "harnbc_hash_blake3": "blake3:2222222222222222222222222222222222222222222222222222222222222222"
    }
  ],
  "stdlib_version": "0.8.24",
  "harn_version": "0.8.24",
  "provider_catalog_hash": "blake3:3333333333333333333333333333333333333333333333333333333333333333",
  "tool_manifest": [],
  "sbom": {
    "format": "spdx-lite",
    "version": "2.3",
    "packages": [
      {
        "name": "harn-stdlib",
        "version": "0.8.24",
        "package_hash_blake3": "blake3:4444444444444444444444444444444444444444444444444444444444444444"
      }
    ],
    "relationships": []
  },
  "signature": null,
  "parent_trust_record_id": null,
  "id": "github-pr-monitor",
  "name": "GitHub PR monitor",
  "version": "1.0.0",
  "triggers": [
    {
      "id": "github-pr-updated",
      "kind": "github",
      "provider": "github",
      "events": ["pull_request.opened", "pull_request.synchronize"],
      "node_id": "ingest"
    },
    {
      "id": "delay-log-check",
      "kind": "delay",
      "delay": "PT10M",
      "node_id": "query_logs"
    }
  ],
  "workflow": {
    "_type": "workflow_graph",
    "id": "pr_monitor_workflow",
    "name": "PR monitor",
    "version": 1,
    "entry": "ingest",
    "nodes": {
      "ingest": {
        "id": "ingest",
        "kind": "action",
        "task_label": "Normalize PR event"
      },
      "wait_for_deploy": {
        "id": "wait_for_deploy",
        "kind": "waitpoint",
        "task_label": "Wait for deploy"
      },
      "query_logs": {
        "id": "query_logs",
        "kind": "action",
        "task_label": "Query logs"
      },
      "notify": {
        "id": "notify",
        "kind": "notification",
        "task_label": "Notify user"
      }
    },
    "edges": [
      {
        "from": "ingest",
        "to": "wait_for_deploy"
      },
      {
        "from": "wait_for_deploy",
        "to": "query_logs"
      },
      {
        "from": "query_logs",
        "to": "notify"
      }
    ]
  },
  "prompt_capsules": {
    "query-logs": {
      "id": "query-logs",
      "node_id": "query_logs",
      "trigger_id": "delay-log-check",
      "prompt": "Query deploy logs for the pull request and summarize failures."
    }
  },
  "policy": {
    "autonomy_tier": "act_with_approval",
    "retry": {
      "max_attempts": 2,
      "backoff": "exponential"
    },
    "catchup": {
      "mode": "latest",
      "max_events": 1
    }
  },
  "connectors": [
    {
      "id": "github",
      "provider_id": "github",
      "scopes": ["pull_requests:read", "checks:read"],
      "setup_required": true,
      "status_required": true
    }
  ],
  "environment": {
    "repo_setup_profile": "default",
    "worktree_policy": "host_managed",
    "command_gates": ["make test"]
  },
  "receipts": {
    "run_id": "bundle_run_pr_monitor_fixture",
    "event_ids": ["github:event:42"],
    "workflow_version": 1
  }
}"#;

pub(crate) fn pr_monitor_bundle() -> WorkflowBundle {
    serde_json::from_str(PR_MONITOR_BUNDLE_JSON).expect("PR monitor fixture parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard: the in-crate fixture must stay byte-equal (after
    /// JSON normalization) to the canonical user-facing fixture under
    /// `docs/fixtures/`. The canonical file is what the workflow-authoring
    /// skill pack and the CLI tests reference; duplicating its content
    /// here is only safe if the two stay in sync.
    #[test]
    fn pr_monitor_fixture_matches_canonical_docs_fixture() {
        let canonical_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json");
        if !canonical_path.exists() {
            // The published crate is unpacked outside the workspace; the
            // docs/ tree is not part of the tarball. Skip the drift
            // check in that environment — workspace CI runs it.
            return;
        }
        let canonical_text = std::fs::read_to_string(&canonical_path)
            .unwrap_or_else(|err| panic!("read {}: {err}", canonical_path.display()));
        let canonical: serde_json::Value =
            serde_json::from_str(&canonical_text).expect("canonical fixture parses");
        let inline: serde_json::Value =
            serde_json::from_str(PR_MONITOR_BUNDLE_JSON).expect("inline fixture parses");
        assert_eq!(
            canonical,
            inline,
            "in-crate PR monitor fixture has drifted from {}",
            canonical_path.display()
        );
    }
}
