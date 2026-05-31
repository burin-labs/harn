use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ThreadLocalAudit {
    plans: BTreeMap<String, String>,
    #[serde(rename = "thread_local")]
    entries: Vec<AuditEntry>,
}

#[derive(Debug, Deserialize)]
struct AuditEntry {
    path: String,
    statics: Vec<String>,
    category: String,
    plan: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct ThreadLocalSite {
    path: String,
    statics: Vec<String>,
}

#[test]
fn thread_local_audit_tracks_every_vm_thread_local_site() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_dir = manifest_dir.join("src");
    let audit_path = manifest_dir.join("thread_local_audit.toml");

    let actual = scan_thread_local_sites(&manifest_dir, &source_dir);
    let audit = parse_audit(&audit_path);

    let mut audited = BTreeSet::new();
    for entry in &audit.entries {
        assert!(
            matches!(
                entry.category.as_str(),
                "logical_task" | "runtime_registry" | "thread_private"
            ),
            "unknown thread-local audit category {:?} for {}",
            entry.category,
            entry.path
        );
        assert!(
            audit
                .plans
                .get(&entry.plan)
                .is_some_and(|plan| !plan.trim().is_empty()),
            "thread-local audit entry for {} references missing or empty plan {:?}",
            entry.path,
            entry.plan
        );
        audited.insert(ThreadLocalSite {
            path: entry.path.clone(),
            statics: entry.statics.clone(),
        });
    }

    let missing: Vec<_> = actual.difference(&audited).cloned().collect();
    let stale: Vec<_> = audited.difference(&actual).cloned().collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "thread_local! audit drift\nmissing: {missing:#?}\nstale: {stale:#?}"
    );
}

#[test]
fn pool_worker_boundary_uses_task_local_registry_not_thread_local_registry() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pool_source =
        fs::read_to_string(manifest_dir.join("src/stdlib/pool/mod.rs")).expect("read pool module");

    assert!(
        thread_local_blocks(&pool_source).is_empty(),
        "pool workers run on tokio::spawn; pool registry state must not regress to thread_local!"
    );
    assert!(
        pool_source.contains("tokio::task_local!") && pool_source.contains("ACTIVE_POOL_REGISTRY"),
        "pool registry must be scoped through a Tokio task-local so it follows spawned work"
    );
    assert!(
        pool_source.contains("spawn_pool_worker"),
        "pool worker spawning should stay behind the documented work-stealing boundary helper"
    );
}

fn parse_audit(path: &Path) -> ThreadLocalAudit {
    let contents = fs::read_to_string(path).expect("read thread_local_audit.toml");
    toml::from_str(&contents).expect("parse thread_local_audit.toml")
}

fn scan_thread_local_sites(manifest_dir: &Path, source_dir: &Path) -> BTreeSet<ThreadLocalSite> {
    let mut sites = BTreeSet::new();
    for path in rust_files(source_dir) {
        let source = fs::read_to_string(&path).expect("read Rust source");
        let relative_path = path
            .strip_prefix(manifest_dir)
            .expect("source under crate")
            .to_string_lossy()
            .replace('\\', "/");
        for block in thread_local_blocks(&source) {
            sites.insert(ThreadLocalSite {
                path: relative_path.clone(),
                statics: static_names(block),
            });
        }
    }
    sites
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).expect("read source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn thread_local_blocks(source: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = source[offset..].find("thread_local!") {
        let start = offset + relative_start;
        let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
        if !source[line_start..start].trim().is_empty() {
            offset = start + "thread_local!".len();
            continue;
        }
        let Some(open_relative) = source[start..].find('{') else {
            break;
        };
        let open = start + open_relative;
        let mut depth = 0_i32;
        for (relative_index, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let close = open + relative_index;
                        blocks.push(&source[open + 1..close]);
                        offset = close + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if offset <= start {
            break;
        }
    }
    blocks
}

fn static_names(block: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in block.lines() {
        let trimmed = line.trim_start();
        let Some(static_index) = trimmed.find("static ") else {
            continue;
        };
        let rest = &trimmed[static_index + "static ".len()..];
        if let Some((name, _)) = rest.split_once(':') {
            names.push(name.trim().to_string());
        }
    }
    names
}
