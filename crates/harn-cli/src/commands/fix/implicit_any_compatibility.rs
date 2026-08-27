//! Behavior-preserving migration for Harn's former implicit-`any` parameters.
//!
//! This is deliberately separate from the authoring repair in
//! `parameter_annotations`: authoring inference narrows signatures and falls
//! back to `unknown`, so it remains surface-changing. A version bump needs the
//! old unchecked contract verbatim. This module therefore has one interface:
//! migrate every checker-owned omitted annotation to explicit `any`, audit the
//! rewritten AST, and return a non-vacuous census.

use std::path::{Path, PathBuf};

use harn_parser::param_annotations::{self, DeclaredParam, UnannotatedParam};
use harn_parser::TypeExpr;
use serde::Serialize;

use crate::commands;

pub(super) const IMPLICIT_ANY_MIGRATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct ImplicitAnyParameterSite {
    pub(super) file: String,
    pub(super) declaration_kind: String,
    pub(super) owner: String,
    pub(super) parameter: String,
    pub(super) index: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct ImplicitAnyMigrationFinding {
    pub(super) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) site: Option<ImplicitAnyParameterSite>,
    pub(super) reason: String,
}

/// Machine-readable proof that the compatibility migration actually observed
/// source and that no eligible parameter was skipped or narrowed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ImplicitAnyMigrationReport {
    pub(super) schema_version: u32,
    pub(super) mode: &'static str,
    pub(super) dry_run: bool,
    pub(super) scanned_file_count: usize,
    pub(super) scanned_files: Vec<String>,
    pub(super) excluded_file_count: usize,
    pub(super) excluded_files: Vec<String>,
    pub(super) observed_count: usize,
    pub(super) observed: Vec<ImplicitAnyParameterSite>,
    pub(super) changed_count: usize,
    pub(super) changed: Vec<ImplicitAnyParameterSite>,
    pub(super) pending_count: usize,
    pub(super) pending: Vec<ImplicitAnyParameterSite>,
    pub(super) unresolved_count: usize,
    pub(super) unresolved: Vec<ImplicitAnyMigrationFinding>,
    pub(super) changed_semantics_count: usize,
    pub(super) changed_semantics: Vec<ImplicitAnyMigrationFinding>,
}

impl ImplicitAnyMigrationReport {
    pub(super) fn is_complete(&self) -> bool {
        self.unresolved_count == 0
            && self.changed_semantics_count == 0
            && self.pending_count == 0
            && self.changed_count == self.observed_count
    }

    fn refresh_counts(&mut self) {
        self.scanned_file_count = self.scanned_files.len();
        self.excluded_file_count = self.excluded_files.len();
        self.observed_count = self.observed.len();
        self.changed_count = self.changed.len();
        self.pending_count = self.pending.len();
        self.unresolved_count = self.unresolved.len();
        self.changed_semantics_count = self.changed_semantics.len();
    }
}

struct CandidateFile {
    path: PathBuf,
    original: String,
    migrated: String,
    sites: Vec<ImplicitAnyParameterSite>,
}

/// Translate every old implicit parameter contract to explicit `any`.
///
/// Candidate files are parsed and audited before any write. Writes are one
/// transaction across the target set: a filesystem failure restores every
/// file already written and returns a failing census.
pub(super) fn migrate(
    targets: &[PathBuf],
    dry_run: bool,
) -> Result<ImplicitAnyMigrationReport, String> {
    migrate_with_writer(targets, dry_run, |path, source| {
        std::fs::write(path, source)
    })
}

fn migrate_with_writer(
    targets: &[PathBuf],
    dry_run: bool,
    mut write: impl FnMut(&Path, &str) -> std::io::Result<()>,
) -> Result<ImplicitAnyMigrationReport, String> {
    let target_strings = targets
        .iter()
        .map(|target| target.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let target_refs = target_strings
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut files = commands::check::collect_harn_targets(&target_refs);
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err("no .harn files found under the given target(s)".to_string());
    }

    let mut report = ImplicitAnyMigrationReport {
        schema_version: IMPLICIT_ANY_MIGRATION_SCHEMA_VERSION,
        mode: "preserve-implicit-any",
        dry_run,
        scanned_file_count: 0,
        scanned_files: Vec::new(),
        excluded_file_count: 0,
        excluded_files: Vec::new(),
        observed_count: 0,
        observed: Vec::new(),
        changed_count: 0,
        changed: Vec::new(),
        pending_count: 0,
        pending: Vec::new(),
        unresolved_count: 0,
        unresolved: Vec::new(),
        changed_semantics_count: 0,
        changed_semantics: Vec::new(),
    };
    let mut candidates = Vec::new();

    for file in files {
        let file_name = file.to_string_lossy().into_owned();
        report.scanned_files.push(file_name.clone());
        if commands::declares_expected_invalid(&file) {
            report.excluded_files.push(file_name);
            continue;
        }
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                report.unresolved.push(file_finding(
                    &file_name,
                    format!("failed to read source: {error}"),
                ));
                continue;
            }
        };
        let program = match harn_parser::parse_source(&source) {
            Ok(program) => program,
            Err(error) => {
                report.unresolved.push(file_finding(
                    &file_name,
                    format!("source did not parse: {error:?}"),
                ));
                continue;
            }
        };
        let declared_before = param_annotations::declared_params(&program);
        let unannotated = param_annotations::unannotated_params(&program);
        let sites = unannotated
            .iter()
            .map(|param| site(&file_name, param))
            .collect::<Vec<_>>();
        report.observed.extend(sites.iter().cloned());
        if unannotated.is_empty() {
            continue;
        }

        let mut insertions = Vec::with_capacity(unannotated.len());
        let mut offset_failed = false;
        for (param, site) in unannotated.iter().zip(&sites) {
            if let Some(offset) = param_annotations::annotation_insert_offset(&source, param) {
                insertions.push((offset, ": any"));
            } else {
                offset_failed = true;
                report.unresolved.push(ImplicitAnyMigrationFinding {
                    file: file_name.clone(),
                    site: Some(site.clone()),
                    reason: "parameter span did not contain its declared name".to_string(),
                });
            }
        }
        if offset_failed {
            continue;
        }
        let Some(migrated) = render_insertions(&source, &mut insertions) else {
            for site in sites {
                report.unresolved.push(ImplicitAnyMigrationFinding {
                    file: file_name.clone(),
                    site: Some(site),
                    reason: "annotation insertion fell outside the source".to_string(),
                });
            }
            continue;
        };
        let program_after = match harn_parser::parse_source(&migrated) {
            Ok(program) => program,
            Err(error) => {
                for site in sites {
                    report.unresolved.push(ImplicitAnyMigrationFinding {
                        file: file_name.clone(),
                        site: Some(site),
                        reason: format!("rewritten source did not parse: {error:?}"),
                    });
                }
                continue;
            }
        };
        let declared_after = param_annotations::declared_params(&program_after);
        let (unresolved, changed_semantics) =
            audit_annotations(&file_name, &declared_before, &declared_after, &sites);
        if !unresolved.is_empty() || !changed_semantics.is_empty() {
            report.unresolved.extend(unresolved);
            report.changed_semantics.extend(changed_semantics);
            continue;
        }
        candidates.push(CandidateFile {
            path: file,
            original: source,
            migrated,
            sites,
        });
    }

    if report.unresolved.is_empty() && report.changed_semantics.is_empty() {
        report.changed = candidates
            .iter()
            .flat_map(|candidate| candidate.sites.iter().cloned())
            .collect();
        if !dry_run {
            if let Err((failed_path, error)) = write_candidates(&candidates, &mut write) {
                report.changed.clear();
                report.unresolved.push(file_finding(
                    &failed_path.to_string_lossy(),
                    format!("failed to write migration transaction: {error}"),
                ));
            }
        }
    }
    report.pending = report
        .observed
        .iter()
        .filter(|site| !report.changed.contains(site))
        .cloned()
        .collect();
    report.refresh_counts();
    Ok(report)
}

fn site(file: &str, param: &UnannotatedParam) -> ImplicitAnyParameterSite {
    ImplicitAnyParameterSite {
        file: file.to_string(),
        declaration_kind: param.kind.as_str().to_string(),
        owner: param.owner.clone(),
        parameter: param.name.clone(),
        index: param.index,
    }
}

fn file_finding(file: &str, reason: String) -> ImplicitAnyMigrationFinding {
    ImplicitAnyMigrationFinding {
        file: file.to_string(),
        site: None,
        reason,
    }
}

fn render_insertions(source: &str, insertions: &mut Vec<(usize, &'static str)>) -> Option<String> {
    insertions.sort_by_key(|(offset, _)| *offset);
    let mut rendered = String::with_capacity(source.len() + insertions.len() * 5);
    let mut cursor = 0usize;
    for (offset, annotation) in insertions {
        rendered.push_str(source.get(cursor..*offset)?);
        rendered.push_str(annotation);
        cursor = *offset;
    }
    rendered.push_str(source.get(cursor..)?);
    Some(rendered)
}

fn audit_annotations(
    file: &str,
    before: &[DeclaredParam],
    after: &[DeclaredParam],
    sites: &[ImplicitAnyParameterSite],
) -> (
    Vec<ImplicitAnyMigrationFinding>,
    Vec<ImplicitAnyMigrationFinding>,
) {
    let mut unresolved = Vec::new();
    let mut changed_semantics = Vec::new();
    let mut site_index = 0usize;
    for (index, original) in before.iter().enumerate() {
        let site = if original.requires_annotation {
            let Some(site) = sites.get(site_index).cloned() else {
                unresolved.push(file_finding(
                    file,
                    "post-rewrite audit lost an observed parameter".to_string(),
                ));
                break;
            };
            site_index += 1;
            site
        } else {
            ImplicitAnyParameterSite {
                file: file.to_string(),
                declaration_kind: original.kind.as_str().to_string(),
                owner: original.owner.clone(),
                parameter: original.name.clone(),
                index: original.index,
            }
        };
        let Some(migrated) = after.get(index) else {
            unresolved.push(ImplicitAnyMigrationFinding {
                file: file.to_string(),
                site: Some(site),
                reason: "post-rewrite declaration census is shorter than the source census"
                    .to_string(),
            });
            continue;
        };
        if migrated.kind != original.kind
            || migrated.owner != original.owner
            || migrated.index != original.index
            || migrated.name != original.name
        {
            unresolved.push(ImplicitAnyMigrationFinding {
                file: file.to_string(),
                site: Some(site),
                reason: "post-rewrite declaration identity differs from the source census"
                    .to_string(),
            });
            continue;
        }
        if original.requires_annotation {
            match migrated.type_expr.as_ref() {
                Some(TypeExpr::Named(name)) if name == "any" => {}
                None => unresolved.push(ImplicitAnyMigrationFinding {
                    file: file.to_string(),
                    site: Some(site),
                    reason: "parameter remains unannotated after migration".to_string(),
                }),
                Some(other) => changed_semantics.push(ImplicitAnyMigrationFinding {
                    file: file.to_string(),
                    site: Some(site),
                    reason: format!("migration produced a narrower annotation: {other:?}"),
                }),
            }
        } else if migrated.type_expr != original.type_expr {
            changed_semantics.push(ImplicitAnyMigrationFinding {
                file: file.to_string(),
                site: Some(site),
                reason: format!(
                    "migration changed an existing annotation from {:?} to {:?}",
                    original.type_expr, migrated.type_expr
                ),
            });
        }
    }
    if site_index != sites.len() {
        unresolved.push(file_finding(
            file,
            format!(
                "post-rewrite audit consumed {site_index} of {} observed parameters",
                sites.len()
            ),
        ));
    }
    if before.len() != after.len() {
        unresolved.push(file_finding(
            file,
            format!(
                "declaration census changed length from {} to {}",
                before.len(),
                after.len()
            ),
        ));
    }
    (unresolved, changed_semantics)
}

fn write_candidates(
    candidates: &[CandidateFile],
    write: &mut impl FnMut(&Path, &str) -> std::io::Result<()>,
) -> Result<(), (PathBuf, String)> {
    let mut written: Vec<&CandidateFile> = Vec::new();
    for candidate in candidates {
        if let Err(error) = write(&candidate.path, &candidate.migrated) {
            let mut rollback_failures = Vec::new();
            if let Err(rollback_error) = std::fs::write(&candidate.path, &candidate.original) {
                rollback_failures.push(format!("{}: {rollback_error}", candidate.path.display()));
            }
            for previous in written.iter().rev() {
                if let Err(rollback_error) = std::fs::write(&previous.path, &previous.original) {
                    rollback_failures
                        .push(format!("{}: {rollback_error}", previous.path.display()));
                }
            }
            let detail = if rollback_failures.is_empty() {
                error.to_string()
            } else {
                format!(
                    "{error}; rollback also failed for {}",
                    rollback_failures.join(", ")
                )
            };
            return Err((candidate.path.clone(), detail));
        }
        written.push(candidate);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execute(source: &str, strict: bool) -> String {
        harn_vm::reset_thread_local_state();
        let chunk = if strict {
            harn_vm::compile_source(source).expect("strict source compiles")
        } else {
            let program = harn_parser::parse_source(source).expect("legacy source parses");
            harn_vm::Compiler::new()
                .compile(&program)
                .expect("legacy source compiles without the new strict gate")
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    let mut vm = harn_vm::Vm::new();
                    harn_vm::register_vm_stdlib(&mut vm);
                    format!("{:?}", vm.execute(&chunk).await.expect("source executes"))
                })
                .await
        })
    }

    #[test]
    fn migration_preserves_scalar_object_callback_and_unused_parameter_calls() {
        let temp = tempfile::TempDir::new().unwrap();
        let source_path = temp.path().join("main.harn");
        let before = concat!(
            "fn scalar(value) { return value + 1 }\n",
            "fn object(value) { return value.name }\n",
            "fn callback(value) { return value(4) }\n",
            "fn unused(value) { return \"kept\" }\n",
            "pipeline main() {\n",
            "  return [scalar(1), object({name: \"Ada\"}), callback({n -> n * 2}), unused(false)]\n",
            "}\n",
        );
        std::fs::write(&source_path, before).unwrap();
        let (_, diagnostics) = harn_parser::check_source(before).unwrap();
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == harn_parser::DiagnosticCode::ImplicitAnyParameter
                })
                .count(),
            4,
            "the legacy source must reach and fail the strict parameter gate: {diagnostics:#?}"
        );
        assert!(harn_parser::check_source_strict(before).is_err());
        let legacy_result = execute(before, false);

        let report = migrate(std::slice::from_ref(&source_path), false).unwrap();
        assert!(report.is_complete(), "{report:#?}");
        assert_eq!(report.scanned_file_count, 1);
        assert_eq!(report.observed_count, 4);
        assert_eq!(report.changed_count, 4);
        assert_eq!(report.pending_count, 0);
        assert_eq!(report.unresolved_count, 0);
        assert_eq!(report.changed_semantics_count, 0);
        assert_eq!(
            report
                .changed
                .iter()
                .map(|site| site.parameter.as_str())
                .collect::<Vec<_>>(),
            vec!["value", "value", "value", "value"]
        );
        let after = std::fs::read_to_string(&source_path).unwrap();
        assert_eq!(after.matches(": any").count(), 4, "{after}");
        assert_eq!(execute(&after, true), legacy_result);

        let clean = migrate(std::slice::from_ref(&source_path), false).unwrap();
        assert!(clean.is_complete(), "{clean:#?}");
        assert_eq!(clean.scanned_file_count, 1);
        assert_eq!(clean.observed_count, 0);
        assert_eq!(clean.changed_count, 0);
        assert_eq!(clean.pending_count, 0);
        assert_eq!(clean.unresolved_count, 0);
        assert_eq!(clean.changed_semantics_count, 0);
    }

    #[test]
    fn audit_reports_narrower_annotations_and_unresolved_source_by_name() {
        let original = harn_parser::parse_source("fn keep(value) { return value }").unwrap();
        let narrowed =
            harn_parser::parse_source("fn keep(value: string) { return value }").unwrap();
        let sites = vec![ImplicitAnyParameterSite {
            file: "main.harn".to_string(),
            declaration_kind: "function".to_string(),
            owner: "keep".to_string(),
            parameter: "value".to_string(),
            index: 0,
        }];
        let (unresolved, changed_semantics) = audit_annotations(
            "main.harn",
            &param_annotations::declared_params(&original),
            &param_annotations::declared_params(&narrowed),
            &sites,
        );
        assert!(unresolved.is_empty());
        assert_eq!(changed_semantics.len(), 1);
        assert_eq!(
            changed_semantics[0].site.as_ref().unwrap().parameter,
            "value"
        );

        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("broken.harn"), "fn broken(value) {\n").unwrap();
        let report = migrate(&[temp.path().to_path_buf()], false).unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.scanned_file_count, 1);
        assert_eq!(report.unresolved_count, 1);
        assert!(report.unresolved[0].file.ends_with("broken.harn"));
    }

    #[test]
    fn declared_invalid_fixture_is_excluded_before_parse_and_left_unchanged() {
        let temp = tempfile::TempDir::new().unwrap();
        let source_path = temp.path().join("implicit_any_parameter.harn");
        let expected_error_path = temp.path().join("implicit_any_parameter.error");
        let source = "fn unchecked(value) {\n  return value\n}\n";
        std::fs::write(&source_path, source).unwrap();
        std::fs::write(&expected_error_path, "has no type annotation\n").unwrap();

        let report = migrate(&[temp.path().to_path_buf()], false).unwrap();
        assert!(report.is_complete(), "{report:#?}");
        assert_eq!(report.scanned_file_count, 1);
        assert_eq!(report.excluded_file_count, 1);
        assert_eq!(
            report.excluded_files,
            vec![source_path.display().to_string()]
        );
        assert_eq!(report.observed_count, 0);
        assert_eq!(report.changed_count, 0);
        assert_eq!(report.pending_count, 0);
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), source);

        let strict_error = harn_parser::check_source_strict(source).unwrap_err();
        let harn_parser::PipelineError::TypeCheck(diagnostic) = strict_error else {
            panic!("fixture must remain parse-valid and fail strict type checking");
        };
        assert_eq!(
            diagnostic.code,
            harn_parser::DiagnosticCode::ImplicitAnyParameter
        );
    }

    #[test]
    fn later_write_failure_rolls_back_every_source_and_fails_with_named_census() {
        let temp = tempfile::TempDir::new().unwrap();
        let first_path = temp.path().join("a-first.harn");
        let failed_path = temp.path().join("b-failed.harn");
        let first_before = b"fn first(value) { return value }\n";
        let failed_before = b"fn second(value) { return value }\n";
        std::fs::write(&first_path, first_before).unwrap();
        std::fs::write(&failed_path, failed_before).unwrap();

        let mut attempted = Vec::new();
        let report = migrate_with_writer(&[temp.path().to_path_buf()], false, |path, source| {
            attempted.push(path.to_path_buf());
            if path == failed_path {
                assert_eq!(
                    std::fs::read_to_string(&first_path).unwrap(),
                    "fn first(value: any) { return value }\n",
                    "the first candidate must reach disk before the later write fails"
                );
                return Err(std::io::Error::other("injected later-write failure"));
            }
            std::fs::write(path, source)
        })
        .unwrap();

        assert_eq!(attempted, vec![first_path.clone(), failed_path.clone()]);
        assert_eq!(std::fs::read(&first_path).unwrap(), first_before);
        assert_eq!(std::fs::read(&failed_path).unwrap(), failed_before);
        assert_eq!(report.scanned_file_count, 2);
        assert_eq!(report.observed_count, 2);
        assert_eq!(report.changed_count, 0);
        assert_eq!(report.pending_count, 2);
        assert_eq!(report.unresolved_count, 1);
        assert_eq!(report.changed_semantics_count, 0);
        assert_eq!(report.unresolved[0].file, failed_path.display().to_string());
        assert!(
            report.unresolved[0]
                .reason
                .contains("injected later-write failure"),
            "{report:#?}"
        );
        let error = super::super::require_complete_implicit_any_migration(&report)
            .expect_err("an incomplete typed census must fail the command");
        assert!(matches!(
            error,
            super::super::FixRunError::PartialFailure(message)
                if message.contains("pending=2, unresolved=1, changed-semantics=0")
        ));
    }
}
