//! A `go.mod` edit changes which declaration a Go call reaches even when
//! no source file changes (issue #9745). The workspace below has two
//! competing `Helper` declarations, so a unique-name fallback cannot hide
//! a call that is still linked through the old manifest.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::time::Duration;

use crate::code_index::reconcile::CheckpointPolicy;
use crate::code_index::{CodeIndexCapability, CodeIndexSnapshot, EdgeKind, IndexState, NodeKind};

const ROOT_MODULE: &str = "module example.com/lib\n\ngo 1.26.1\n";
const V2_MODULE: &str = "module example.com/lib/v2\n\ngo 1.26.1\n";
/// Under [`ROOT_MODULE`] this import names `v2/pkg`; under [`V2_MODULE`]
/// it names `pkg`.
const CALLER: &str =
    "package app\n\nimport \"example.com/lib/v2/pkg\"\n\nfunc Main() { pkg.Helper() }\n";
const HELPER: &str = "package pkg\n\nfunc Helper() {}\n";
const VIA_V2: &str = "app/main.go -> v2/pkg/helper.go";
const VIA_ROOT: &str = "app/main.go -> pkg/helper.go";

fn write(root: &Path, path: &str, body: &str) {
    let abs = root.join(path);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

fn workspace(manifest: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    if let Some(manifest) = manifest {
        write(root, "go.mod", manifest);
    }
    write(root, "app/main.go", CALLER);
    write(root, "pkg/helper.go", HELPER);
    write(root, "v2/pkg/helper.go", HELPER);
    dir
}

/// `caller path -> declaring path` for every CALLS edge into `Helper`.
fn helper_calls(state: &IndexState) -> Vec<String> {
    let mut out = Vec::new();
    let declarations = state.symbols.nodes_named("Helper");
    let functions: Vec<_> = declarations
        .iter()
        .filter_map(|id| state.symbols.node(*id).map(|node| (*id, node)))
        .filter(|(_, node)| node.kind == NodeKind::Function)
        .collect();
    assert_eq!(
        functions.len(),
        2,
        "both competing declarations are indexed"
    );
    for (id, decl) in functions {
        for edge in state.symbols.incoming(id) {
            if edge.kind == EdgeKind::Calls {
                let site = state.symbols.node(edge.from).unwrap();
                out.push(format!("{} -> {}", site.path, decl.path));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn restore_relinks_calls_after_a_manifest_only_change() {
    let dir = workspace(Some(ROOT_MODULE));
    let root = dir.path();
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);
    state.snapshot().save(root).unwrap();

    write(root, "go.mod", V2_MODULE);
    let capability = CodeIndexCapability::new();
    assert!(capability.restore_from_disk(root).unwrap());
    let shared = capability.shared();
    let guard = shared.lock().unwrap();
    let restored = guard.as_ref().expect("restored index");
    assert_eq!(helper_calls(restored), vec![VIA_ROOT]);
    drop(guard);
    capability.wait_until_idle();

    // The reconciled graph was persisted with the manifests it now
    // reflects, so the next restore needs no relink to be correct.
    let persisted = CodeIndexSnapshot::load(root).unwrap().unwrap();
    let mut next = IndexState::from_snapshot(persisted).unwrap();
    assert_eq!(helper_calls(&next), vec![VIA_ROOT]);
    assert!(next.refresh_from_root(None).is_noop());
}

#[test]
fn resident_refresh_relinks_only_files_the_manifest_affects() {
    let dir = workspace(Some(ROOT_MODULE));
    let root = dir.path();
    write(root, "tools/gen.go", "package tools\n\nfunc Gen() {}\n");
    let (mut state, _) = IndexState::build_from_root(root);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);

    write(root, "go.mod", V2_MODULE);
    let outcome = state.refresh_from_root(None);
    assert_eq!(outcome.files_reindexed, 0, "no source file changed");
    assert_eq!(outcome.files_reresolved, 1, "only the importing file");
    assert!(!outcome.is_noop());
    assert_eq!(helper_calls(&state), vec![VIA_ROOT]);

    // A declaration-preserving edit is the unchanged-input fast path.
    write(
        root,
        "go.mod",
        &format!("{V2_MODULE}\nrequire example.com/x v1.0.0\n"),
    );
    assert!(state.refresh_from_root(None).is_noop());
    assert_eq!(helper_calls(&state), vec![VIA_ROOT]);
}

#[test]
fn created_and_removed_root_manifests_change_resolution() {
    let dir = workspace(None);
    let root = dir.path();
    let (mut state, _) = IndexState::build_from_root(root);
    assert!(
        helper_calls(&state).is_empty(),
        "ambiguous without a module"
    );

    write(root, "go.mod", ROOT_MODULE);
    assert_eq!(state.refresh_from_root(None).files_reresolved, 1);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);

    fs::remove_file(root.join("go.mod")).unwrap();
    assert_eq!(state.refresh_from_root(None).files_reresolved, 1);
    assert!(helper_calls(&state).is_empty());
}

#[test]
fn a_nested_manifest_takes_the_longer_module_path() {
    let dir = workspace(Some(ROOT_MODULE));
    let root = dir.path();
    let (mut state, _) = IndexState::build_from_root(root);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);

    // The nested module declares the imported path outright.
    write(root, "pkg/go.mod", "module example.com/lib/v2/pkg\n");
    assert_eq!(state.refresh_from_root(None).files_reresolved, 1);
    assert_eq!(helper_calls(&state), vec![VIA_ROOT]);

    fs::remove_file(root.join("pkg/go.mod")).unwrap();
    assert_eq!(state.refresh_from_root(None).files_reresolved, 1);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);
}

#[test]
fn host_reindex_of_a_manifest_relinks_a_resident_index() {
    let dir = workspace(Some(ROOT_MODULE));
    let root = dir.path();
    let (mut state, _) = IndexState::build_from_root(root);
    let manifest = state.root.join("go.mod");

    write(root, "go.mod", V2_MODULE);
    assert_eq!(
        state.reindex_file(&manifest),
        None,
        "manifests are not files"
    );
    assert_eq!(helper_calls(&state), vec![VIA_ROOT]);

    // A newly created nested manifest is read even though no walk saw it.
    let nested = state.root.join("v2/pkg/go.mod");
    write(root, "v2/pkg/go.mod", "module example.com/lib/v2/pkg\n");
    state.reindex_file(&nested);
    assert_eq!(helper_calls(&state), vec![VIA_V2]);

    fs::remove_file(&nested).unwrap();
    state.remove_file_path(&nested);
    assert_eq!(helper_calls(&state), vec![VIA_ROOT]);
}

#[test]
fn a_checkpoint_keeps_the_manifests_its_graph_was_linked_under() {
    let dir = workspace(Some(ROOT_MODULE));
    let root = dir.path();
    for name in ["a", "b", "c"] {
        write(root, &format!("extra/{name}.go"), "package extra\n");
    }
    let (mut state, _) = IndexState::build_from_root(root);
    write(root, "go.mod", V2_MODULE);
    for name in ["a", "b", "c"] {
        write(
            root,
            &format!("extra/{name}.go"),
            "package extra\n\nfunc E() {}\n",
        );
    }
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        state.refresh_with_checkpoints(
            None,
            CheckpointPolicy {
                files: 2,
                interval: Duration::MAX,
            },
            |state| {
                state.snapshot().save(&state.root).unwrap();
                panic!("simulate process exit immediately after checkpoint");
            },
        );
    }));
    assert!(interrupted.is_err(), "the checkpoint must actually fire");

    let checkpoint = CodeIndexSnapshot::load(root).unwrap().unwrap();
    let mut restored = IndexState::from_snapshot(checkpoint).unwrap();
    assert_eq!(
        helper_calls(&restored),
        vec![VIA_V2],
        "a partial walk must not publish the new manifest"
    );
    let outcome = restored.refresh_from_root(None);
    assert_eq!(outcome.files_reresolved, 1);
    assert_eq!(helper_calls(&restored), vec![VIA_ROOT]);
}
