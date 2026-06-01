//! Whole-project scan → accumulate → edit lifecycle (#2836).
//!
//! Adapted from OpenRewrite's `ScanningRecipe`: a rule can read the *whole*
//! fileset into a typed accumulator before it edits, and can emit new files
//! or delete existing ones — not just edit in place. That whole-project view
//! is what import insertion, codegen, and cross-file dead-code removal need.
//!
//! A run is two deterministic passes over the (path-sorted) files:
//!
//! 1. **scan** — each file updates a typed accumulator; no edits.
//! 2. **generate** — the accumulator + the files produce a set of
//!    [`FileChange`]s (edit / create / delete).
//!
//! Per-file declarative codemods plug in via [`RuleRecipe`], which needs no
//! scan state; richer recipes implement [`ScanningRecipe`] directly.

use std::path::{Path, PathBuf};

use harn_hostlib::ast::Language;

use crate::engine::CompiledRule;
use crate::error::RulesError;

/// One source file handed to a recipe.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The file's path (used for ordering and change attribution).
    pub path: PathBuf,
    /// The file's language.
    pub language: Language,
    /// The file's contents.
    pub source: String,
}

impl SourceFile {
    /// Construct a source file, detecting the language from the path's
    /// extension. Returns `None` if no grammar matches.
    pub fn detect(path: impl Into<PathBuf>, source: impl Into<String>) -> Option<Self> {
        let path = path.into();
        let language = Language::detect(&path, None)?;
        Some(SourceFile {
            path,
            language,
            source: source.into(),
        })
    }
}

/// A change a recipe wants to make to the project. The runner returns these;
/// the caller (a CLI / the staged filesystem) decides whether to write them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// Replace an existing file's contents.
    Edit {
        /// The file to rewrite.
        path: PathBuf,
        /// The new contents.
        contents: String,
    },
    /// Create a new file.
    Create {
        /// The path to create.
        path: PathBuf,
        /// The new file's contents.
        contents: String,
    },
    /// Delete an existing file.
    Delete {
        /// The path to delete.
        path: PathBuf,
    },
}

impl FileChange {
    /// The path this change targets.
    pub fn path(&self) -> &Path {
        match self {
            FileChange::Edit { path, .. }
            | FileChange::Create { path, .. }
            | FileChange::Delete { path } => path,
        }
    }
}

/// A two-phase, whole-project rule.
pub trait ScanningRecipe {
    /// The typed accumulator threaded from `scan` into `generate`.
    type Acc: Default;

    /// Read one file and update the accumulator. Called once per file, in
    /// path-sorted order, before any `generate`.
    fn scan(&self, file: &SourceFile, acc: &mut Self::Acc) -> Result<(), RulesError>;

    /// Produce the project's changes from the accumulated state and the
    /// (path-sorted) files.
    fn generate(
        &self,
        files: &[SourceFile],
        acc: &Self::Acc,
    ) -> Result<Vec<FileChange>, RulesError>;
}

/// The result of running a recipe over a project.
#[derive(Debug, Clone)]
pub struct RecipeRun {
    /// The changes the recipe produced, sorted by path for determinism.
    pub changes: Vec<FileChange>,
}

impl RecipeRun {
    /// Files this run edits.
    pub fn edits(&self) -> impl Iterator<Item = &FileChange> {
        self.changes
            .iter()
            .filter(|c| matches!(c, FileChange::Edit { .. }))
    }

    /// Files this run creates.
    pub fn creations(&self) -> impl Iterator<Item = &FileChange> {
        self.changes
            .iter()
            .filter(|c| matches!(c, FileChange::Create { .. }))
    }
}

/// Run `recipe` over `files`: a `scan` pass (path-sorted) followed by a
/// `generate` pass. The returned changes are path-sorted for a stable,
/// reproducible result.
pub fn run_recipe<R: ScanningRecipe>(
    recipe: &R,
    mut files: Vec<SourceFile>,
) -> Result<RecipeRun, RulesError> {
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut acc = R::Acc::default();
    for file in &files {
        recipe.scan(file, &mut acc)?;
    }

    let mut changes = recipe.generate(&files, &acc)?;
    changes.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(RecipeRun { changes })
}

/// Adapter that runs a declarative [`CompiledRule`] as a recipe: a per-file
/// codemod with no scan state. Each file matching the rule's language is
/// rewritten via [`CompiledRule::apply`]; only changed files yield an edit.
pub struct RuleRecipe<'a> {
    /// The compiled codemod rule.
    pub rule: &'a CompiledRule,
}

impl ScanningRecipe for RuleRecipe<'_> {
    type Acc = ();

    fn scan(&self, _file: &SourceFile, _acc: &mut ()) -> Result<(), RulesError> {
        Ok(())
    }

    fn generate(&self, files: &[SourceFile], _acc: &()) -> Result<Vec<FileChange>, RulesError> {
        let mut changes = Vec::new();
        for file in files {
            if file.language != self.rule.language() {
                continue;
            }
            let result = self.rule.apply(&file.source)?;
            if result.changed {
                changes.push(FileChange::Edit {
                    path: file.path.clone(),
                    contents: result.rewritten,
                });
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn ts(path: &str, source: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            source: source.to_string(),
        }
    }

    /// A recipe that counts call expressions across the project and, if any
    /// exist, emits a single generated report file — exercising both the
    /// accumulator and `FileChange::Create`.
    struct CallCounter;
    impl ScanningRecipe for CallCounter {
        type Acc = usize;
        fn scan(&self, file: &SourceFile, acc: &mut usize) -> Result<(), RulesError> {
            *acc += file.source.matches("()").count();
            Ok(())
        }
        fn generate(
            &self,
            _files: &[SourceFile],
            acc: &usize,
        ) -> Result<Vec<FileChange>, RulesError> {
            if *acc == 0 {
                return Ok(vec![]);
            }
            Ok(vec![FileChange::Create {
                path: PathBuf::from("report.txt"),
                contents: format!("calls: {acc}\n"),
            }])
        }
    }

    #[test]
    fn recipe_accumulates_then_generates_a_new_file() {
        let files = vec![ts("a.ts", "foo();\n"), ts("b.ts", "bar(); baz();\n")];
        let run = run_recipe(&CallCounter, files).unwrap();
        assert_eq!(run.changes.len(), 1);
        assert_eq!(
            run.changes[0],
            FileChange::Create {
                path: PathBuf::from("report.txt"),
                contents: "calls: 3\n".into(),
            }
        );
    }

    #[test]
    fn scan_runs_in_path_sorted_order() {
        // The accumulator records the order files are scanned in; the runner
        // must sort by path regardless of input order.
        struct OrderRecorder;
        impl ScanningRecipe for OrderRecorder {
            type Acc = Vec<String>;
            fn scan(&self, file: &SourceFile, acc: &mut Vec<String>) -> Result<(), RulesError> {
                acc.push(file.path.to_string_lossy().to_string());
                Ok(())
            }
            fn generate(
                &self,
                _f: &[SourceFile],
                acc: &Vec<String>,
            ) -> Result<Vec<FileChange>, RulesError> {
                Ok(vec![FileChange::Create {
                    path: PathBuf::from("order.txt"),
                    contents: acc.join(","),
                }])
            }
        }
        let files = vec![ts("z.ts", ""), ts("a.ts", ""), ts("m.ts", "")];
        let run = run_recipe(&OrderRecorder, files).unwrap();
        match &run.changes[0] {
            FileChange::Create { contents, .. } => assert_eq!(contents, "a.ts,m.ts,z.ts"),
            other => panic!("expected create, got {other:?}"),
        }
    }

    #[test]
    fn rule_recipe_edits_matching_files_only() {
        let rule = crate::engine::CompiledRule::compile(
            &Rule::from_toml_str(
                r#"
                id = "rename-foo"
                language = "typescript"
                safety = "behavior-preserving"
                fix = "bar()"
                [rule]
                pattern = "foo()"
                "#,
            )
            .unwrap(),
        )
        .unwrap();
        let files = vec![
            ts("a.ts", "foo();\n"),
            ts("b.ts", "noMatch();\n"),
            SourceFile {
                path: PathBuf::from("c.rs"),
                language: Language::Rust,
                source: "fn f() { foo(); }".into(),
            },
        ];
        let run = run_recipe(&RuleRecipe { rule: &rule }, files).unwrap();
        // Only a.ts changes: b.ts has no match, c.rs is a different language.
        assert_eq!(run.changes.len(), 1);
        assert_eq!(run.changes[0].path(), Path::new("a.ts"));
    }
}
