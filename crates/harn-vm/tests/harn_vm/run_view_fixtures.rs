use std::path::{Path, PathBuf};

use harn_vm::orchestration::{sync_run_view_fixtures, RunViewFixtureMode, RunViewFixtureSummary};

#[test]
fn run_view_fixture_snapshots_match() {
    let mode = if std::env::var_os("HARN_REGENERATE_FIXTURES").is_some() {
        RunViewFixtureMode::Write
    } else {
        RunViewFixtureMode::Check
    };
    let summary = sync_run_view_fixtures(&repo_root(), mode)
        .expect("checked-in run/session view fixtures are current");
    assert_eq!(
        summary,
        RunViewFixtureSummary {
            case_count: 4,
            run_view_count: 5,
            snapshot_count: 9,
        }
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("harn-vm crate lives under crates/")
        .to_path_buf()
}
