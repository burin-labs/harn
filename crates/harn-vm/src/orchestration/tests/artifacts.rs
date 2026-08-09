//! Selecting which artifacts survive into the next step.
//!
//! Selection honors the budget and priority order, duplicates are removed,
//! oversized artifacts are snipped, and adaptive selection drops stale evidence
//! once a fresher write covers the same ground.

use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn artifact_selection_honors_budget_and_priority() {
    let policy = ContextPolicy {
        max_artifacts: Some(2),
        max_tokens: Some(30),
        prefer_recent: true,
        prefer_fresh: true,
        prioritize_kinds: vec!["verification_result".to_string()],
        ..Default::default()
    };
    let artifacts = vec![
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "a".to_string(),
            kind: "summary".to_string(),
            text: Some("short".to_string()),
            relevance: Some(0.9),
            created_at: now_unix_seconds_text(),
            ..Default::default()
        }
        .normalize(),
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "b".to_string(),
            kind: "summary".to_string(),
            text: Some("this is a much larger artifact body".to_string()),
            relevance: Some(1.0),
            created_at: now_unix_seconds_text(),
            ..Default::default()
        }
        .normalize(),
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "c".to_string(),
            kind: "summary".to_string(),
            text: Some("tiny".to_string()),
            relevance: Some(0.5),
            created_at: now_unix_seconds_text(),
            ..Default::default()
        }
        .normalize(),
    ];
    let selected = select_artifacts(artifacts, &policy);
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|artifact| artifact.kind == "summary"));
}

#[test]
fn dedup_artifacts_removes_duplicates() {
    let mut artifacts = vec![
        ArtifactRecord {
            id: "a1".to_string(),
            kind: "test".to_string(),
            text: Some("duplicate content".to_string()),
            ..Default::default()
        },
        ArtifactRecord {
            id: "a2".to_string(),
            kind: "test".to_string(),
            text: Some("duplicate content".to_string()),
            ..Default::default()
        },
        ArtifactRecord {
            id: "a3".to_string(),
            kind: "test".to_string(),
            text: Some("unique content".to_string()),
            ..Default::default()
        },
    ];
    dedup_artifacts(&mut artifacts);
    assert_eq!(artifacts.len(), 2);
}

#[test]
fn microcompact_artifact_snips_oversized() {
    let mut artifact = ArtifactRecord {
        id: "a1".to_string(),
        kind: "test".to_string(),
        text: Some("x".repeat(10_000)),
        estimated_tokens: Some(2_500),
        ..Default::default()
    };
    microcompact_artifact(&mut artifact, 500);
    assert!(artifact.text.as_ref().unwrap().len() < 5_000);
    assert_eq!(artifact.estimated_tokens, Some(500));
}

#[test]
fn select_artifacts_adaptive_drops_stale_evidence_after_fresh_write() {
    let selected = select_artifacts_adaptive(
        vec![
            ArtifactRecord {
                id: "research-index".to_string(),
                kind: "summary".to_string(),
                text: Some("index.ts currently exports only authGuard".to_string()),
                freshness: Some("normal".to_string()),
                metadata: BTreeMap::from([(
                    "evidence_paths".to_string(),
                    serde_json::json!(["packages/server/src/middleware/index.ts"]),
                )]),
                ..Default::default()
            },
            ArtifactRecord {
                id: "research-api".to_string(),
                kind: "summary".to_string(),
                text: Some("api.ts currently uses withMiddleware".to_string()),
                freshness: Some("normal".to_string()),
                metadata: BTreeMap::from([(
                    "evidence_paths".to_string(),
                    serde_json::json!(["packages/server/src/routes/api.ts"]),
                )]),
                ..Default::default()
            },
            ArtifactRecord {
                id: "batch-2".to_string(),
                kind: "summary".to_string(),
                text: Some("Updated middleware/index.ts to export rateLimit".to_string()),
                freshness: Some("fresh".to_string()),
                metadata: BTreeMap::from([(
                    "changed_paths".to_string(),
                    serde_json::json!(["packages/server/src/middleware/index.ts"]),
                )]),
                ..Default::default()
            },
        ],
        &ContextPolicy::default(),
    );
    let ids: Vec<_> = selected
        .iter()
        .map(|artifact| artifact.id.as_str())
        .collect();
    assert!(!ids.contains(&"research-index"), "ids={ids:?}");
    assert!(ids.contains(&"research-api"), "ids={ids:?}");
    assert!(ids.contains(&"batch-2"), "ids={ids:?}");
}
