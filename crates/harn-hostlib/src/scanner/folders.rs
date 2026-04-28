//! Folder aggregates, project metadata, and the text repo map.

use std::collections::BTreeMap;
use std::path::Path;

use crate::scanner::extensions::folder_key;
use crate::scanner::result::{
    FileRecord, FolderRecord, LanguageStat, ProjectMetadata, SymbolKind, SymbolRecord,
};

#[derive(Default)]
struct FolderAggregation {
    files: usize,
    lines: usize,
    languages: BTreeMap<String, usize>,
    symbol_names: Vec<String>,
}

/// Build [`FolderRecord`]s from the file + symbol lists.
///
/// Folders are returned sorted desc by `line_count`.
pub fn build_folder_records(files: &[FileRecord], symbols: &[SymbolRecord]) -> Vec<FolderRecord> {
    let mut by_folder: BTreeMap<String, FolderAggregation> = BTreeMap::new();

    for file in files {
        let key = folder_key(&file.relative_path).to_string();
        let agg = by_folder.entry(key).or_default();
        agg.files += 1;
        agg.lines += file.line_count;
        *agg.languages.entry(file.language.clone()).or_insert(0) += 1;
    }

    let mut symbols_by_folder: BTreeMap<String, Vec<&SymbolRecord>> = BTreeMap::new();
    for sym in symbols.iter().filter(|s| s.kind.is_type_definition()) {
        let key = folder_key(&sym.file_path).to_string();
        symbols_by_folder.entry(key).or_default().push(sym);
    }
    for (folder, syms) in symbols_by_folder.iter_mut() {
        syms.sort_by(|a, b| {
            b.importance_score
                .partial_cmp(&a.importance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let names: Vec<String> = syms.iter().take(5).map(|s| s.name.clone()).collect();
        if let Some(agg) = by_folder.get_mut(folder) {
            agg.symbol_names = names;
        }
    }

    let mut records: Vec<FolderRecord> = by_folder
        .into_iter()
        .map(|(folder, data)| {
            let dominant = data
                .languages
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(lang, _)| lang.clone())
                .unwrap_or_default();
            FolderRecord {
                id: folder.clone(),
                relative_path: folder,
                file_count: data.files,
                line_count: data.lines,
                dominant_language: dominant,
                key_symbol_names: data.symbol_names,
            }
        })
        .collect();

    records.sort_by(|a, b| {
        b.line_count
            .cmp(&a.line_count)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    records
}

/// Build [`ProjectMetadata`] from the file/symbol/folder lists.
pub fn build_project_metadata(
    root_path: &Path,
    files: &[FileRecord],
    test_commands: BTreeMap<String, String>,
    code_patterns: Vec<String>,
    last_scanned_at: String,
) -> ProjectMetadata {
    let root_path_string = root_path.to_string_lossy().replace('\\', "/");
    let name = root_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let total_lines: usize = files.iter().map(|f| f.line_count).sum();
    let total_files = files.len();

    let mut lang_data: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for file in files {
        let entry = lang_data.entry(file.language.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += file.line_count;
    }
    let mut languages: Vec<LanguageStat> = lang_data
        .into_iter()
        .map(|(name, (file_count, line_count))| LanguageStat {
            name,
            file_count,
            line_count,
            percentage: if total_lines > 0 {
                (line_count as f64) / (total_lines as f64) * 100.0
            } else {
                0.0
            },
        })
        .collect();
    languages.sort_by(|a, b| {
        b.line_count
            .cmp(&a.line_count)
            .then_with(|| a.name.cmp(&b.name))
    });

    let detected_test_command =
        crate::scanner::commands::select_preferred_test_command(&test_commands);

    ProjectMetadata {
        name,
        root_path: root_path_string,
        languages,
        test_commands,
        detected_test_command,
        code_patterns,
        total_files,
        total_lines,
        last_scanned_at,
    }
}

/// Build the text repo map. Token budget is approximate — the builder caps
/// emission at `4 * tokens` characters.
pub fn build_repo_map(
    symbols: &[SymbolRecord],
    _files: &[FileRecord],
    token_budget: usize,
) -> String {
    let char_budget = token_budget.saturating_mul(4);
    if char_budget == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut remaining = char_budget;

    let mut ranked: Vec<&SymbolRecord> = symbols
        .iter()
        .filter(|s| {
            s.kind.is_type_definition()
                || matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.importance_score
            .partial_cmp(&a.importance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut file_order: Vec<&str> = Vec::new();
    let mut by_file: BTreeMap<&str, Vec<&SymbolRecord>> = BTreeMap::new();
    for sym in ranked {
        if !by_file.contains_key(sym.file_path.as_str()) {
            file_order.push(sym.file_path.as_str());
        }
        by_file.entry(sym.file_path.as_str()).or_default().push(sym);
    }

    'files: for file in file_order {
        let header = format!("{file}:\n");
        if header.len() > remaining {
            break;
        }
        output.push_str(&header);
        remaining -= header.len();

        if let Some(syms) = by_file.get(file) {
            for sym in syms {
                let line = if !sym.signature.is_empty() {
                    format!("  {}\n", sym.signature)
                } else {
                    format!("  {} {}\n", sym.kind.keyword(), sym.name)
                };
                if line.len() > remaining {
                    break 'files;
                }
                output.push_str(&line);
                remaining -= line.len();
            }
        }
        if remaining == 0 {
            break;
        }
    }

    output.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lang: &str, lines: usize) -> FileRecord {
        FileRecord {
            id: path.to_string(),
            relative_path: path.to_string(),
            file_name: path.rsplit('/').next().unwrap().to_string(),
            language: lang.to_string(),
            line_count: lines,
            size_bytes: 0,
            last_modified_unix_ms: 0,
            imports: Vec::new(),
            churn_score: 0.0,
            corresponding_test_file: None,
        }
    }

    fn symbol(name: &str, kind: SymbolKind, file: &str, score: f64) -> SymbolRecord {
        SymbolRecord {
            id: format!("{file}:{name}:1"),
            name: name.to_string(),
            kind,
            file_path: file.to_string(),
            line: 1,
            signature: String::new(),
            container: None,
            reference_count: 0,
            importance_score: score,
        }
    }

    #[test]
    fn folder_records_sort_by_line_count_desc() {
        let files = vec![
            file("a/foo.rs", "rs", 10),
            file("b/bar.rs", "rs", 100),
            file("a/qux.rs", "rs", 5),
        ];
        let records = build_folder_records(&files, &[]);
        assert_eq!(records[0].relative_path, "b");
        assert_eq!(records[0].line_count, 100);
        assert_eq!(records[1].relative_path, "a");
        assert_eq!(records[1].line_count, 15);
    }

    #[test]
    fn repo_map_includes_top_symbols_per_file() {
        let symbols = vec![
            symbol("Foo", SymbolKind::StructDecl, "a.rs", 5.0),
            symbol("bar", SymbolKind::Function, "a.rs", 2.0),
        ];
        let map = build_repo_map(&symbols, &[], 200);
        assert!(map.contains("a.rs:"));
        assert!(map.contains("struct Foo"));
        assert!(map.contains("function bar"));
    }

    #[test]
    fn project_metadata_language_breakdown_sorted_by_lines() {
        let files = vec![file("a.rs", "rs", 10), file("b.ts", "ts", 50)];
        let proj = build_project_metadata(
            Path::new("/repo/proj"),
            &files,
            BTreeMap::new(),
            Vec::new(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        assert_eq!(proj.name, "proj");
        assert_eq!(proj.total_lines, 60);
        assert_eq!(proj.languages[0].name, "ts");
        assert_eq!(proj.languages[1].name, "rs");
    }
}
