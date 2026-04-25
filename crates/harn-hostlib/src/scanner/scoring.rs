//! Reference counting + importance scoring + churn analysis.
//!
//! Mirrors the relevant phases in `CoreRepoScanner.swift`:
//!
//! * `computeReferenceCounts` — count how many times each symbol name
//!   appears as the trailing component of an import path, plus 1 per extra
//!   file that re-defines the same name.
//! * `gitChurnScores` — `git log --since=90.days --name-only --pretty=format:`
//!   normalized to `[0, 1]`.
//! * `computeImportanceScores` —
//!   `importance = 3*ref_count + 2*type_def + churn`, halved when the
//!   symbol is contained in another type.
//!
//! These are deliberate, well-documented heuristics — burin-code consumers
//! depend on the exact numeric output, so any changes here must be
//! coordinated with the Swift implementation.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

use crate::scanner::result::{FileRecord, SymbolRecord};

/// Compute per-symbol reference counts derived from the cross-file import graph.
pub fn compute_reference_counts(symbols: &mut [SymbolRecord], files: &[FileRecord]) {
    let symbol_names: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

    let mut ref_counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in files {
        for imp in &file.imports {
            let module = imp.rsplit('/').next().unwrap_or(imp);
            if symbol_names.contains(module) {
                *ref_counts.entry(module.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut symbol_files: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    for sym in symbols.iter() {
        symbol_files
            .entry(&sym.name)
            .or_default()
            .insert(&sym.file_path);
    }
    for (name, file_set) in symbol_files {
        if file_set.len() > 1 {
            *ref_counts.entry(name.to_string()).or_insert(0) += file_set.len() - 1;
        }
    }

    for sym in symbols.iter_mut() {
        sym.reference_count = ref_counts.get(&sym.name).copied().unwrap_or(0);
    }
}

/// Compute per-file churn scores from the last 90 days of git history.
/// Returns the mapping; callers apply it to [`FileRecord::churn_score`].
/// Returns an empty map if `git` is unavailable, the call fails, or the
/// repo has no commits in the window.
pub fn compute_churn_scores(root: &Path) -> BTreeMap<String, f64> {
    let output = Command::new("git")
        .args([
            "-C",
            match root.to_str() {
                Some(s) => s,
                None => return BTreeMap::new(),
            },
            "log",
            "--since=90.days",
            "--name-only",
            "--pretty=format:",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return BTreeMap::new(),
    };
    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return BTreeMap::new(),
    };

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        *counts.entry(trimmed.to_string()).or_insert(0) += 1;
    }

    let max = counts.values().copied().max().unwrap_or(1).max(1) as f64;
    counts
        .into_iter()
        .map(|(file, count)| (file, count as f64 / max))
        .collect()
}

/// Apply the churn map to a slice of file records (mutates in place).
pub fn apply_churn(files: &mut [FileRecord], churn: &BTreeMap<String, f64>) {
    if churn.is_empty() {
        return;
    }
    for file in files {
        if let Some(&score) = churn.get(&file.relative_path) {
            file.churn_score = score;
        }
    }
}

/// Compute importance scores for every symbol given the file churn map.
pub fn compute_importance_scores(symbols: &mut [SymbolRecord], files: &[FileRecord]) {
    let mut churn_by_file: BTreeMap<&str, f64> = BTreeMap::new();
    for file in files {
        churn_by_file.insert(&file.relative_path, file.churn_score);
    }
    for sym in symbols.iter_mut() {
        let mut score = sym.reference_count as f64 * 3.0;
        if sym.kind.is_type_definition() {
            score += 2.0;
        }
        score += churn_by_file
            .get(sym.file_path.as_str())
            .copied()
            .unwrap_or(0.0);
        if sym.container.is_some() {
            score *= 0.6;
        }
        sym.importance_score = score;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::result::SymbolKind;

    fn file(path: &str, imports: &[&str]) -> FileRecord {
        FileRecord {
            id: path.to_string(),
            relative_path: path.to_string(),
            file_name: path.to_string(),
            language: "rs".to_string(),
            line_count: 1,
            size_bytes: 1,
            last_modified_unix_ms: 0,
            imports: imports.iter().map(|s| s.to_string()).collect(),
            churn_score: 0.0,
            corresponding_test_file: None,
        }
    }

    fn symbol(name: &str, kind: SymbolKind, file: &str, container: Option<&str>) -> SymbolRecord {
        SymbolRecord {
            id: format!("{file}:{name}:1"),
            name: name.to_string(),
            kind,
            file_path: file.to_string(),
            line: 1,
            signature: String::new(),
            container: container.map(|s| s.to_string()),
            reference_count: 0,
            importance_score: 0.0,
        }
    }

    #[test]
    fn ref_counts_pick_up_trailing_module_segment() {
        // Swift `computeReferenceCounts` splits only on "/" — so "std::Foo"
        // never matches because its trailing segment is `std::Foo`, not
        // `Foo`. Trailing segments from "/" splits do match.
        let files = vec![
            file("a.rs", &["std::Foo", "Foo"]),
            file("b.rs", &["bar/Foo"]),
        ];
        let mut symbols = vec![symbol("Foo", SymbolKind::StructDecl, "z.rs", None)];
        compute_reference_counts(&mut symbols, &files);
        assert_eq!(symbols[0].reference_count, 2);
    }

    #[test]
    fn duplicate_definitions_inflate_ref_count() {
        let files: Vec<FileRecord> = Vec::new();
        let mut symbols = vec![
            symbol("Foo", SymbolKind::StructDecl, "a.rs", None),
            symbol("Foo", SymbolKind::StructDecl, "b.rs", None),
        ];
        compute_reference_counts(&mut symbols, &files);
        // 2 files defining `Foo` → +1 reference count.
        assert_eq!(symbols[0].reference_count, 1);
        assert_eq!(symbols[1].reference_count, 1);
    }

    #[test]
    fn importance_halves_when_contained() {
        let files = vec![FileRecord {
            churn_score: 0.5,
            ..file("a.rs", &[])
        }];
        let mut symbols = vec![
            symbol("Outer", SymbolKind::ClassDecl, "a.rs", None),
            symbol("inner", SymbolKind::Method, "a.rs", Some("Outer")),
        ];
        symbols[0].reference_count = 2; // 2 * 3 = 6
        symbols[1].reference_count = 0;
        compute_importance_scores(&mut symbols, &files);
        // Outer: 6 + 2 (type def) + 0.5 churn = 8.5
        assert!((symbols[0].importance_score - 8.5).abs() < 1e-9);
        // inner: (0*3 + 0.5) * 0.6 = 0.3
        assert!((symbols[1].importance_score - 0.3).abs() < 1e-9);
    }
}
