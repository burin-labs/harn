//! CALLS edges must not depend on the order files reach the index.

use harn_vm::value::VmDictExt as _;
use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::code_index::{EdgeKind, IndexState, NodeKind};

fn write(root: &Path, path: &str, body: &str) {
    let abs = root.join(path);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

/// `caller path -> declaring path` for every CALLS edge into a function
/// named `name`, sorted.
fn calls(state: &IndexState, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for id in state.symbols.nodes_named(name) {
        let Some(decl) = state.symbols.node(*id) else {
            continue;
        };
        if decl.kind != NodeKind::Function {
            continue;
        }
        for edge in state.symbols.incoming(*id) {
            if edge.kind != EdgeKind::Calls {
                continue;
            }
            let site = state.symbols.node(edge.from).unwrap();
            out.push(format!("{} -> {}", site.path, decl.path));
        }
    }
    out.sort();
    out
}

const DECL: &str = "def fetch_rows():\n    return []\n";

fn call_importing(module: &str) -> String {
    format!("from {module} import fetch_rows\n\nfetch_rows()\n")
}

const BARE_CALL: &str = "fetch_rows()\n";

#[test]
fn a_call_reaches_a_declaration_indexed_before_it() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", DECL);
    write(root, "z_use.py", &call_importing("a_decl"));
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_decl.py"]);
}

#[test]
fn a_call_reaches_a_declaration_indexed_after_it() {
    // The walk is sorted, so `a_use.py` is indexed before `z_decl.py`.
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_use.py", &call_importing("z_decl"));
    write(root, "z_decl.py", DECL);
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&state, "fetch_rows"), vec!["a_use.py -> z_decl.py"]);
}

#[test]
fn a_unique_declaration_indexed_after_an_unimporting_call_reaches_it() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_use.py", BARE_CALL);
    write(root, "z_decl.py", DECL);
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&state, "fetch_rows"), vec!["a_use.py -> z_decl.py"]);
}

#[test]
fn a_later_duplicate_declaration_removes_the_unique_name_edge() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", DECL);
    write(root, "z_use.py", BARE_CALL);
    let (mut state, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_decl.py"]);

    // Two declarations and no import: the call cannot be resolved.
    write(root, "b_decl.py", DECL);
    state.reindex_file(&root.join("b_decl.py")).unwrap();
    assert!(calls(&state, "fetch_rows").is_empty());
}

#[test]
fn a_later_unique_declaration_adds_the_edge() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "z_use.py", BARE_CALL);
    let (mut state, _) = IndexState::build_from_root(root);
    assert!(calls(&state, "fetch_rows").is_empty());

    write(root, "a_decl.py", DECL);
    state.refresh_from_root(None);
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_decl.py"]);
}

#[test]
fn removing_a_duplicate_makes_the_survivor_unique_again() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", DECL);
    write(root, "b_decl.py", DECL);
    write(root, "z_use.py", BARE_CALL);
    let (mut state, _) = IndexState::build_from_root(root);
    assert!(calls(&state, "fetch_rows").is_empty());

    write(root, "b_decl.py", "def other():\n    return []\n");
    state.reindex_file(&root.join("b_decl.py")).unwrap();
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_decl.py"]);
}

#[test]
fn reindexing_the_declaring_file_keeps_its_callers() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", DECL);
    write(root, "z_use.py", &call_importing("a_decl"));
    let (mut state, _) = IndexState::build_from_root(root);

    write(root, "a_decl.py", "def fetch_rows():\n    return [1]\n");
    state.reindex_file(&root.join("a_decl.py")).unwrap();
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_decl.py"]);
}

#[test]
fn restored_module_imports_keep_callers_when_only_the_declaration_changes() {
    use harn_vm::VmValue;

    use crate::code_index::CodeIndexCapability;
    use crate::registry::{BuiltinRegistry, HostlibCapability};

    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Sources/Core/Decl.swift",
        "func fetchRows() -> Int { 1 }\n",
    );
    // A competing declaration prevents the workspace-wide unique-name
    // fallback from hiding lost imported-module visibility after recovery.
    write(
        root,
        "Sources/Other/Decl.swift",
        "func fetchRows() -> Int { 9 }\n",
    );
    write(
        root,
        "Sources/App/Main.swift",
        "import Core\nfunc main() { _ = fetchRows() }\n",
    );
    let (state, outcome) = IndexState::build_from_root(root);
    assert_eq!(outcome.files_indexed, 3);
    let app = state.lookup_path("Sources/App/Main.swift").unwrap();
    let core = state.lookup_path("Sources/Core/Decl.swift").unwrap();
    assert_eq!(
        state
            .symbols
            .nodes_named("fetchRows")
            .iter()
            .filter(|id| state.symbols.node(**id).unwrap().kind == NodeKind::Function)
            .count(),
        2,
        "the competing declaration must defeat unique-name fallback"
    );
    assert_eq!(state.imports_of(app), vec![core]);
    assert_eq!(state.importers_of(core), vec![app]);
    assert_eq!(
        calls(&state, "fetchRows"),
        vec!["Sources/App/Main.swift -> Sources/Core/Decl.swift"]
    );
    state.snapshot().save(root).unwrap();

    let capability = CodeIndexCapability::new();
    assert!(capability.restore_from_disk(root).unwrap());
    let shared = capability.shared();
    {
        let guard = shared.lock().unwrap();
        let restored = guard.as_ref().unwrap();
        assert_eq!(restored.imports_of(app), vec![core]);
        assert_eq!(restored.importers_of(core), vec![app]);
        assert_eq!(
            calls(restored, "fetchRows"),
            vec!["Sources/App/Main.swift -> Sources/Core/Decl.swift"]
        );
    }

    // Leave the importing source untouched: reindexing it would repair its
    // dependency row and conceal the restoration defect.
    write(
        root,
        "Sources/Core/Decl.swift",
        "func fetchRows() -> Int { 2 }\n",
    );
    let mut registry = BuiltinRegistry::new();
    capability.register_builtins(&mut registry);
    let mut request = harn_vm::value::DictMap::new();
    request.put_str("path", "Sources/Core/Decl.swift");
    let result = (registry
        .find("hostlib_code_index_reindex_file")
        .unwrap()
        .handler)(&[VmValue::dict(request)])
    .unwrap();
    let VmValue::Dict(result) = result else {
        panic!("reindex must return a record");
    };
    assert!(matches!(result.get("indexed"), Some(VmValue::Bool(true))));
    assert!(matches!(result.get("file_id"), Some(VmValue::Int(id)) if *id == i64::from(core)));
    let guard = shared.lock().unwrap();
    let restored = guard.as_ref().unwrap();
    assert_eq!(restored.imports_of(app), vec![core]);
    assert_eq!(restored.importers_of(core), vec![app]);
    assert_eq!(
        calls(restored, "fetchRows"),
        vec!["Sources/App/Main.swift -> Sources/Core/Decl.swift"]
    );
}

#[test]
fn a_local_declaration_still_shadows_an_imported_one() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "a_decl.py", DECL);
    write(
        root,
        "z_use.py",
        "from a_decl import fetch_rows\n\n\ndef fetch_rows():\n    return [2]\n\n\nfetch_rows()\n",
    );
    let (state, _) = IndexState::build_from_root(root);
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> z_use.py"]);
}

#[test]
fn reindexing_a_new_file_resolves_an_import_that_was_dangling() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, "other.py", DECL);
    write(root, "z_use.py", &call_importing("a_new"));
    let (mut state, _) = IndexState::build_from_root(root);
    // `a_new` does not exist yet, so the unique declaration is the target.
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> other.py"]);

    write(root, "a_new.py", DECL);
    state.reindex_file(&root.join("a_new.py")).unwrap();
    assert_eq!(calls(&state, "fetch_rows"), vec!["z_use.py -> a_new.py"]);
}
