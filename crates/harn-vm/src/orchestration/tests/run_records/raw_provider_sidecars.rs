use crate::orchestration::*;
use serde_json::{json, Value};
use std::path::PathBuf;

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    run_path: PathBuf,
    canonical: PathBuf,
    previous: Vec<u8>,
    run: RunRecord,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/llm_transcript.jsonl");
        let run_path = temp.path().join("bundle/run.json");
        let canonical = temp.path().join("bundle/run-llm/llm_transcript.jsonl");
        std::fs::create_dir_all(source.parent().unwrap().join("raw-provider")).unwrap();
        let previous =
            b"{\"type\":\"system_prompt\",\"content\":\"previous valid snapshot\"}\n".to_vec();
        std::fs::write(&source, &previous).unwrap();
        let run = RunRecord {
            id: "raw-provider-sidecars".into(),
            status: "completed".into(),
            ..Default::default()
        };
        save_run_record_with_transcript(&run, Some(run_path.to_str().unwrap()), Some(&source))
            .unwrap();
        let run = load_run_record(&run_path).unwrap();
        assert_eq!(
            verified_llm_transcript_pointer_path(&run, &run_path).unwrap(),
            canonical
        );
        Self {
            _temp: temp,
            source,
            run_path,
            canonical,
            previous,
            run,
        }
    }

    fn pointer(&self, path: Value) {
        let row = json!({"type":"provider_raw_capture", "path":path});
        std::fs::write(&self.source, format!("{row}\n")).unwrap();
    }

    fn assert_unavailable(&self, reason: &str) {
        save_run_record_with_transcript(
            &self.run,
            Some(self.run_path.to_str().unwrap()),
            Some(&self.source),
        )
        .unwrap();
        let stored: RunRecord =
            serde_json::from_slice(&std::fs::read(&self.run_path).unwrap()).unwrap();
        for run in [stored, load_run_record(&self.run_path).unwrap()] {
            assert_eq!(run.status, "completed");
            let pointer = run
                .observability
                .as_ref()
                .unwrap()
                .transcript_pointers
                .iter()
                .find(|pointer| pointer.kind == "llm_jsonl")
                .unwrap();
            assert!(!pointer.available);
            assert_eq!(pointer.verification_status, "unavailable");
            assert_eq!(pointer.verification_error.as_deref(), Some(reason));
            assert!(pointer.descriptor.is_none());
            assert!(verified_llm_transcript_pointer_path(&run, &self.run_path).is_err());
        }
        assert_eq!(std::fs::read(&self.canonical).unwrap(), self.previous);
    }
}

#[test]
fn malformed_raw_pointers_cannot_reuse_a_previous_valid_transcript() {
    for (path, reason) in [
        (Value::Null, "invalid_raw_provider_pointer"),
        (json!(7), "invalid_raw_provider_pointer"),
        (json!(""), "invalid_raw_provider_path"),
        (
            json!("/raw-provider/capture.json"),
            "invalid_raw_provider_path",
        ),
        (
            json!("raw-provider/../capture.json"),
            "invalid_raw_provider_path",
        ),
        (json!("other/capture.json"), "invalid_raw_provider_path"),
        (
            json!("raw-provider/capture\\outside.json"),
            "invalid_raw_provider_path",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.pointer(path);
        fixture.assert_unavailable(reason);
    }
    let fixture = Fixture::new();
    std::fs::write(&fixture.source, "{\"type\":\"provider_raw_capture\"}\n").unwrap();
    fixture.assert_unavailable("invalid_raw_provider_pointer");
}

#[test]
fn nonregular_raw_artifact_is_refused_without_reading_it() {
    let fixture = Fixture::new();
    fixture.pointer(json!("raw-provider/directory.json"));
    std::fs::create_dir(
        fixture
            .source
            .parent()
            .unwrap()
            .join("raw-provider/directory.json"),
    )
    .unwrap();
    fixture.assert_unavailable("invalid_raw_provider_artifact");
}

#[test]
fn missing_raw_artifact_cannot_reuse_a_previous_valid_transcript() {
    let fixture = Fixture::new();
    fixture.pointer(json!("raw-provider/missing.json"));
    fixture.assert_unavailable("raw_provider_artifact_unavailable");
    assert_eq!(
        describe_llm_transcript_sidecar(&fixture.run, &fixture.run_path, &fixture.source)
            .unwrap_err()
            .kind,
        "raw_provider_artifact_unavailable"
    );
}

#[test]
fn partial_copy_does_not_publish_a_transcript_or_reuse_its_previous_descriptor() {
    let fixture = Fixture::new();
    for name in ["a.json", "b.json"] {
        std::fs::write(
            fixture
                .source
                .parent()
                .unwrap()
                .join("raw-provider")
                .join(name),
            name,
        )
        .unwrap();
    }
    let destination = fixture.canonical.parent().unwrap().join("raw-provider");
    std::fs::create_dir_all(destination.join("b.json")).unwrap();
    std::fs::write(
        &fixture.source,
        concat!(
            "{\"type\":\"provider_raw_capture\",\"path\":\"raw-provider/a.json\"}\n",
            "{\"type\":\"provider_raw_capture\",\"path\":\"raw-provider/b.json\"}\n",
        ),
    )
    .unwrap();
    fixture.assert_unavailable("invalid_raw_provider_artifact");
    assert_eq!(
        std::fs::read(destination.join("a.json")).unwrap(),
        b"a.json"
    );
}

#[cfg(unix)]
#[test]
fn raw_artifact_source_and_destination_symlinks_are_refused() {
    use std::os::unix::fs::symlink;
    for source in [true, false] {
        for directory in [true, false] {
            let fixture = Fixture::new();
            fixture.pointer(json!("raw-provider/capture.json"));
            let outside = tempfile::tempdir().unwrap();
            let outside_file = outside.path().join("capture.json");
            std::fs::write(&outside_file, "outside protected").unwrap();
            let source_root = fixture.source.parent().unwrap().join("raw-provider");
            std::fs::write(source_root.join("capture.json"), "owned capture").unwrap();
            let root = if source {
                source_root
            } else {
                fixture.canonical.parent().unwrap().join("raw-provider")
            };
            if directory {
                if source {
                    std::fs::remove_dir_all(&root).unwrap();
                }
                symlink(outside.path(), &root).unwrap();
            } else {
                std::fs::create_dir_all(&root).unwrap();
                let leaf = root.join("capture.json");
                if source {
                    std::fs::remove_file(&leaf).unwrap();
                }
                symlink(&outside_file, leaf).unwrap();
            }
            fixture.assert_unavailable("raw_provider_artifact_symlink");
            assert_eq!(
                std::fs::read_to_string(&outside_file).unwrap(),
                "outside protected"
            );
        }
    }
}

#[test]
fn descriptor_digest_failure_is_not_replaced_by_rediscovering_the_same_path() {
    let fixture = Fixture::new();
    std::fs::write(
        &fixture.canonical,
        "{\"type\":\"terminal\",\"terminal\":true}\n",
    )
    .unwrap();
    let loaded = load_run_record(&fixture.run_path).unwrap();
    let pointer = loaded
        .observability
        .unwrap()
        .transcript_pointers
        .into_iter()
        .find(|pointer| pointer.kind == "llm_jsonl")
        .unwrap();
    assert!(!pointer.available);
    assert_eq!(
        pointer.verification_error.as_deref(),
        Some("digest_mismatch")
    );
}

#[test]
fn a_new_valid_active_snapshot_recovers_from_recorded_unavailability() {
    let fixture = Fixture::new();
    fixture.pointer(json!("raw-provider/capture.json"));
    fixture.assert_unavailable("raw_provider_artifact_unavailable");
    let failed = load_run_record(&fixture.run_path).unwrap();
    std::fs::write(
        fixture
            .source
            .parent()
            .unwrap()
            .join("raw-provider/capture.json"),
        "captured bytes",
    )
    .unwrap();
    save_run_record_with_transcript(
        &failed,
        Some(fixture.run_path.to_str().unwrap()),
        Some(&fixture.source),
    )
    .unwrap();
    let recovered = load_run_record(&fixture.run_path).unwrap();
    let canonical = verified_llm_transcript_pointer_path(&recovered, &fixture.run_path).unwrap();
    assert_eq!(
        std::fs::read(
            canonical
                .parent()
                .unwrap()
                .join("raw-provider/capture.json")
        )
        .unwrap(),
        b"captured bytes"
    );
}
