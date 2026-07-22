//! Acceptance for #2836: the whole-project scan → accumulate → edit
//! lifecycle, including a decision that needs the whole-project view and
//! new-file generation.

use std::path::{Path, PathBuf};

use harn_rules::{run_recipe, FileChange, RulesError, ScanningRecipe, SourceFile};
use regex::Regex;

fn ts(path: &str, source: &str) -> SourceFile {
    SourceFile::detect(path, source).expect("ts file")
}

/// Insert a missing import for `symbol` at the top of every file that
/// references it but does not import it — pointing at the module that
/// *exports* it (discovered by scanning the whole project). This decision
/// is impossible per-file: the target module is only known after scanning.
struct EnsureImport {
    symbol: String,
}

#[derive(Default)]
struct ImportAcc {
    /// Module specifier (e.g. `./logger`) of the file that exports `symbol`.
    exporting_module: Option<String>,
    /// Files that reference `symbol` without importing it.
    needs_import: Vec<PathBuf>,
}

impl EnsureImport {
    fn references(&self, src: &str) -> bool {
        Regex::new(&format!(r"\b{}\b", regex::escape(&self.symbol)))
            .unwrap()
            .is_match(src)
    }
    fn exports(&self, src: &str) -> bool {
        Regex::new(&format!(
            r"export\s+(function|class|const)\s+{}\b",
            regex::escape(&self.symbol)
        ))
        .unwrap()
        .is_match(src)
    }
    fn imports(&self, src: &str) -> bool {
        src.lines()
            .any(|l| l.contains("import") && l.contains(&self.symbol) && l.contains("from"))
    }
}

impl ScanningRecipe for EnsureImport {
    type Acc = ImportAcc;

    fn scan(&self, file: &SourceFile, acc: &mut ImportAcc) -> Result<(), RulesError> {
        if self.exports(&file.source) {
            let stem = file.path.file_stem().unwrap().to_string_lossy();
            acc.exporting_module = Some(format!("./{stem}"));
            return Ok(());
        }
        if self.references(&file.source) && !self.imports(&file.source) {
            acc.needs_import.push(file.path.clone());
        }
        Ok(())
    }

    fn generate(
        &self,
        files: &[SourceFile],
        acc: &ImportAcc,
    ) -> Result<Vec<FileChange>, RulesError> {
        let Some(module) = &acc.exporting_module else {
            return Ok(vec![]); // symbol not exported anywhere → nothing to do
        };
        let mut changes = Vec::new();
        for path in &acc.needs_import {
            let file = files.iter().find(|f| &f.path == path).unwrap();
            let import = format!("import {{ {} }} from \"{module}\";\n", self.symbol);
            changes.push(FileChange::Edit {
                path: path.clone(),
                contents: format!("{import}{}", file.source),
            });
        }
        Ok(changes)
    }
}

#[test]
fn inserts_missing_import_using_whole_project_view() {
    let files = vec![
        ts("logger.ts", "export function Logger() {}\n"),
        ts("a.ts", "Logger();\n"), // needs import
        ts("b.ts", "import { Logger } from \"./logger\";\nLogger();\n"), // already imports
        ts("c.ts", "noRef();\n"),  // no reference
    ];
    let run = run_recipe(
        &EnsureImport {
            symbol: "Logger".into(),
        },
        files,
    )
    .unwrap();

    // Exactly one file (`a.ts`) gets the import; the module specifier comes
    // from scanning `logger.ts`.
    assert_eq!(run.changes.len(), 1, "changes: {:?}", run.changes);
    match &run.changes[0] {
        FileChange::Edit { path, contents } => {
            assert_eq!(path, Path::new("a.ts"));
            assert_eq!(
                contents,
                "import { Logger } from \"./logger\";\nLogger();\n"
            );
        }
        other => panic!("expected edit, got {other:?}"),
    }
}

#[test]
fn no_import_inserted_when_symbol_is_not_exported_anywhere() {
    // Without a defining module, the recipe cannot know what to import.
    let files = vec![ts("a.ts", "Logger();\n")];
    let run = run_recipe(
        &EnsureImport {
            symbol: "Logger".into(),
        },
        files,
    )
    .unwrap();
    assert!(run.changes.is_empty());
}

/// Generate a barrel file re-exporting every module that exports something —
/// a new-file-generation recipe.
struct Barrel;

impl ScanningRecipe for Barrel {
    type Acc = Vec<String>;

    fn scan(&self, file: &SourceFile, acc: &mut Vec<String>) -> Result<(), RulesError> {
        if file.source.contains("export ") {
            let stem = file.path.file_stem().unwrap().to_string_lossy();
            acc.push(format!("./{stem}"));
        }
        Ok(())
    }

    fn generate(
        &self,
        _files: &[SourceFile],
        modules: &Vec<String>,
    ) -> Result<Vec<FileChange>, RulesError> {
        if modules.is_empty() {
            return Ok(vec![]);
        }
        let body = modules
            .iter()
            .map(|m| format!("export * from \"{m}\";"))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(vec![FileChange::Create {
            path: PathBuf::from("index.ts"),
            contents: format!("{body}\n"),
        }])
    }
}

#[test]
fn emits_a_new_barrel_file() {
    let files = vec![
        ts("alpha.ts", "export const a = 1;\n"),
        ts("beta.ts", "export const b = 2;\n"),
        ts("internal.ts", "const x = 3;\n"),
    ];
    let run = run_recipe(&Barrel, files).unwrap();
    let creations: Vec<_> = run.creations().collect();
    assert_eq!(creations.len(), 1);
    match creations[0] {
        FileChange::Create { path, contents } => {
            assert_eq!(path, Path::new("index.ts"));
            // Path-sorted scan → alpha before beta; internal.ts has no export.
            assert_eq!(
                contents,
                "export * from \"./alpha\";\nexport * from \"./beta\";\n"
            );
        }
        other => panic!("expected create, got {other:?}"),
    }
}
