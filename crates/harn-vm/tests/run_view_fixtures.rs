//! Byte-exact compatibility fixtures for the public run/session view API.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    build_run_view_with_options, build_session_view_from_run_views, RunRecord, RunView,
    RunViewOptions, SessionView, SessionViewOptions, ViewProducer, RUN_VIEW_SCHEMA,
    SESSION_VIEW_SCHEMA,
};
use serde::Serialize;
use serde_json::Value as JsonValue;

const FIXTURE_ROOT: &str = "spec/run-view-fixtures";
const UPDATE_ENV: &str = "HARN_REGENERATE_RUN_VIEW_FIXTURES";
const FIXTURE_PRODUCER_VERSION: &str = "fixture";

#[test]
fn run_view_fixture_snapshots_match() {
    let root = fixture_root();
    let mut cases = read_sorted_dirs(&root.join("cases"));
    assert!(
        !cases.is_empty(),
        "expected at least one run-view fixture case"
    );

    for case_dir in cases.drain(..) {
        let case_name = case_dir
            .file_name()
            .and_then(OsStr::to_str)
            .expect("case directory has UTF-8 name");
        let mut run_views = Vec::new();
        for record_path in read_sorted_json_files(&case_dir.join("records")) {
            let record_name = record_path
                .file_stem()
                .and_then(OsStr::to_str)
                .expect("fixture record has UTF-8 stem");
            let raw = fs::read_to_string(&record_path)
                .unwrap_or_else(|err| panic!("read {}: {err}", record_path.display()));
            let run: RunRecord = serde_json::from_str(&raw).unwrap_or_else(|err| {
                panic!("decode {} as RunRecord: {err}", record_path.display())
            });
            let run_view = build_run_view_with_options(
                &run,
                RunViewOptions {
                    producer: fixture_producer(),
                    run_path: Some(repo_relative_path(&record_path)),
                    ..RunViewOptions::default()
                },
            );
            assert_snapshot(
                &case_dir
                    .join("expected")
                    .join("runs")
                    .join(format!("{record_name}.run_view.json")),
                &run_view,
                RUN_VIEW_SCHEMA,
            );
            run_views.push(run_view);
        }

        let session_view = build_session_view_from_run_views(
            run_views.clone(),
            SessionViewOptions {
                producer: fixture_producer(),
                ..SessionViewOptions::default()
            },
        );
        assert_snapshot(
            &case_dir.join("expected").join("session_view.json"),
            &session_view,
            SESSION_VIEW_SCHEMA,
        );
        assert_case_coverage(case_name, &run_views, &session_view);
    }
}

fn fixture_producer() -> ViewProducer {
    ViewProducer {
        name: "harn".to_string(),
        version: FIXTURE_PRODUCER_VERSION.to_string(),
    }
}

fn fixture_root() -> PathBuf {
    repo_root().join(FIXTURE_ROOT)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("harn-vm crate lives under crates/")
        .to_path_buf()
}

fn read_sorted_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs = fs::read_dir(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .map(|entry| entry.expect("read fixture dir entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn read_sorted_json_files(path: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .map(|entry| entry.expect("read fixture record entry").path())
        .filter(|path| path.extension() == Some(OsStr::new("json")))
        .collect::<Vec<_>>();
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one fixture record in {}",
        path.display()
    );
    files
}

fn repo_relative_path(path: &Path) -> String {
    let relative = path
        .strip_prefix(repo_root())
        .unwrap_or_else(|err| panic!("{} is not under repo root: {err}", path.display()));
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn assert_snapshot<T: Serialize>(path: &Path, value: &T, schema: &str) {
    let rendered = render_snapshot(value, schema);
    if std::env::var_os(UPDATE_ENV).is_some() {
        fs::create_dir_all(
            path.parent()
                .unwrap_or_else(|| panic!("{} has a parent", path.display())),
        )
        .unwrap_or_else(|err| panic!("create {}: {err}", path.display()));
        fs::write(path, &rendered).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
        return;
    }
    let expected =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(
        expected.replace("\r\n", "\n"),
        rendered,
        "{} is stale; run `make gen-run-view-fixtures` to refresh",
        path.display()
    );
}

fn render_snapshot<T: Serialize>(value: &T, schema: &str) -> String {
    let value = serde_json::to_value(value).expect("serialize projection");
    assert_projection_metadata(&value, schema);
    format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("pretty-print projection")
    )
}

fn assert_projection_metadata(value: &JsonValue, schema: &str) {
    assert_eq!(value["schema"], schema);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["producer"]["name"], "harn");
    assert_eq!(value["producer"]["version"], FIXTURE_PRODUCER_VERSION);
}

fn assert_case_coverage(case_name: &str, runs: &[RunView], session: &SessionView) {
    match case_name {
        "legacy-sparse" => {
            let run = only_run(runs);
            assert_eq!(run.run.run_id, "run_legacy_sparse");
            assert_eq!(run.run.session_id, None);
            assert_eq!(run.run.workflow_id, "");
            assert_eq!(
                run.failure.as_ref().map(|failure| failure.status.as_str()),
                Some("failed")
            );
        }
        "root-transcript" => {
            let run = only_run(runs);
            assert!(run.transcript.present);
            assert_eq!(run.transcript.source.as_deref(), Some("run_root"));
            assert_eq!(run.transcript.message_count, 2);
            assert_eq!(run.providers.len(), 1);
        }
        "session-lineage-failure-approval" => {
            assert_eq!(
                session.session.session_id.as_deref(),
                Some("session_lineage")
            );
            assert_eq!(session.session.status, "failed");
            assert_eq!(session.session.run_count, 2);
            assert_eq!(session.pending.approvals.len(), 1);
            assert_eq!(runs[0].run.child_runs.len(), 1);
            assert!(runs.iter().any(|run| run.failure.is_some()));
        }
        "stage-transcript-active-auth" => {
            let run = only_run(runs);
            assert_eq!(run.run.status, "running");
            assert_eq!(run.transcript.source.as_deref(), Some("stages"));
            assert_eq!(run.transcript.message_count, 2);
            assert_eq!(run.pending.auth.len(), 2);
            assert_eq!(session.session.status, "active");
        }
        other => panic!("unrecognized run-view fixture case `{other}`"),
    }
}

fn only_run(runs: &[RunView]) -> &RunView {
    assert_eq!(runs.len(), 1);
    &runs[0]
}
