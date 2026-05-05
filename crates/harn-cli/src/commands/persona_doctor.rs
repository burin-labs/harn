use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use harn_lint::LintSeverity;
use serde::Serialize;

use crate::cli::PersonaDoctorArgs;
use crate::package::{self, PersonaManifestEntry, ResolvedPersonaManifest};
use crate::test_runner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Green,
    Yellow,
    Red,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct PersonaDoctorReport {
    pub persona: String,
    pub manifest_path: PathBuf,
    pub checks: Vec<DoctorCheck>,
}

impl PersonaDoctorReport {
    fn has_red(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Red)
    }
}

pub(crate) async fn run_doctor(
    manifest_arg: Option<&Path>,
    args: &PersonaDoctorArgs,
) -> Result<(), String> {
    let report = doctor_report(manifest_arg, args).await;
    match report {
        Ok(report) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|error| format!("failed to serialize doctor report: {error}"))?
                );
            } else {
                print_report(&report);
            }
            Ok(())
        }
        Err(error_report) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&error_report)
                        .map_err(|error| format!("failed to serialize doctor report: {error}"))?
                );
            } else {
                print_report(&error_report);
            }
            Err("persona doctor found red checks".to_string())
        }
    }
}

pub(crate) async fn doctor_report(
    manifest_arg: Option<&Path>,
    args: &PersonaDoctorArgs,
) -> Result<PersonaDoctorReport, PersonaDoctorReport> {
    let manifest_path = resolve_manifest_path(manifest_arg, &args.name);
    let mut checks = Vec::new();
    let catalog = match package::load_personas_from_manifest_path(&manifest_path) {
        Ok(catalog) => {
            checks.push(check(
                "manifest",
                DoctorStatus::Green,
                format!("{} validates", catalog.manifest_path.display()),
            ));
            catalog
        }
        Err(errors) => {
            checks.push(check(
                "manifest",
                DoctorStatus::Red,
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
            return Err(PersonaDoctorReport {
                persona: args.name.clone(),
                manifest_path,
                checks,
            });
        }
    };

    let Some(persona) = catalog
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(args.name.as_str()))
        .or_else(|| {
            catalog
                .personas
                .iter()
                .find(|persona| persona.name.as_deref() == Some(path_name(&args.name).as_str()))
        })
    else {
        checks.push(check(
            "manifest-persona",
            DoctorStatus::Red,
            format!(
                "persona '{}' not found in {}",
                args.name,
                catalog.manifest_path.display()
            ),
        ));
        return Err(PersonaDoctorReport {
            persona: args.name.clone(),
            manifest_path: catalog.manifest_path,
            checks,
        });
    };
    let persona_name = persona.name.clone().unwrap_or_else(|| args.name.clone());

    let entry_source = resolve_entry_source(&catalog, persona);
    checks.push(source_shape_check(&entry_source));
    checks.push(lint_check(&catalog, &entry_source));
    checks.push(prompt_asset_check(&catalog, &entry_source));
    checks.push(step_metadata_check(persona));
    checks.push(cost_check(persona));
    checks.push(smoke_check(&catalog, &persona_name, args.timeout_ms).await);

    let report = PersonaDoctorReport {
        persona: persona_name,
        manifest_path: catalog.manifest_path,
        checks,
    };
    if report.has_red() {
        Err(report)
    } else {
        Ok(report)
    }
}

pub async fn doctor_report_for_persona(
    manifest_arg: Option<&Path>,
    name: &str,
    timeout_ms: u64,
) -> Result<PersonaDoctorReport, PersonaDoctorReport> {
    let args = PersonaDoctorArgs {
        name: name.to_string(),
        json: false,
        timeout_ms,
    };
    doctor_report(manifest_arg, &args).await
}

fn print_report(report: &PersonaDoctorReport) {
    println!(
        "persona doctor: {} ({})",
        report.persona,
        report.manifest_path.display()
    );
    for check in &report.checks {
        println!(
            "  {:<6} {:<20} {}",
            check.status.label(),
            check.name,
            check.message
        );
    }
}

fn check(name: &str, status: DoctorStatus, message: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status,
        message: message.into(),
    }
}

fn resolve_manifest_path(manifest_arg: Option<&Path>, name: &str) -> PathBuf {
    if let Some(path) = manifest_arg {
        return path.to_path_buf();
    }
    let raw = PathBuf::from(name);
    if raw.is_dir() {
        let manifest = raw.join("harn.toml");
        if manifest.exists() {
            return manifest;
        }
    }
    if raw.is_file() {
        return raw;
    }
    let normalized = name.replace('-', "_");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for candidate in [
        cwd.join("personas").join(&normalized).join("harn.toml"),
        cwd.join(&normalized).join("harn.toml"),
    ] {
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("harn.toml")
}

fn path_name(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .replace('-', "_")
}

fn resolve_entry_source(
    catalog: &ResolvedPersonaManifest,
    persona: &PersonaManifestEntry,
) -> Option<PathBuf> {
    let entry = persona.entry_workflow.as_deref()?;
    let (path, _) = entry.split_once('#')?;
    Some(catalog.manifest_dir.join(path))
}

fn source_shape_check(entry_source: &Option<PathBuf>) -> DoctorCheck {
    let Some(path) = entry_source else {
        return check(
            "entry-source",
            DoctorStatus::Red,
            "entry_workflow must point at a .harn file with #run",
        );
    };
    match fs::read_to_string(path) {
        Ok(source) => {
            let banned = [
                "__host_agent",
                "workflow_stage_agent_loop",
                "harn_vm::",
                "RustAgent",
            ];
            if let Some(token) = banned.iter().find(|token| source.contains(**token)) {
                return check(
                    "entry-source",
                    DoctorStatus::Red,
                    format!(
                        "{} references removed/private runtime token {token}",
                        path.display()
                    ),
                );
            }
            if !source.contains("@persona") {
                return check(
                    "entry-source",
                    DoctorStatus::Red,
                    format!("{} does not declare @persona", path.display()),
                );
            }
            check(
                "entry-source",
                DoctorStatus::Green,
                format!("{} is Harn-first and declares @persona", path.display()),
            )
        }
        Err(error) => check(
            "entry-source",
            DoctorStatus::Red,
            format!("failed to read {}: {error}", path.display()),
        ),
    }
}

fn lint_check(catalog: &ResolvedPersonaManifest, entry_source: &Option<PathBuf>) -> DoctorCheck {
    let Some(path) = entry_source else {
        return check("lint", DoctorStatus::Red, "entry source unavailable");
    };
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            return check(
                "lint",
                DoctorStatus::Red,
                format!("failed to read {}: {error}", path.display()),
            )
        }
    };
    let program = match harn_parser::parse_source(&source) {
        Ok(program) => program,
        Err(error) => return check("lint", DoctorStatus::Red, error.to_string()),
    };
    let files = collect_package_harn_files(&catalog.manifest_dir);
    let module_graph = crate::commands::check::build_module_graph(&files);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header: false,
        complexity_threshold: None,
        persona_step_allowlist: &[],
    };
    let diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &[],
        Some(&source),
        &HashSet::new(),
        &module_graph,
        path,
        &options,
    );
    if diagnostics.is_empty() {
        return check("lint", DoctorStatus::Green, "no issues found");
    }
    let red = diagnostics.iter().any(|diag| {
        diag.severity == LintSeverity::Error || diag.rule == "persona-body-must-call-steps"
    });
    let status = if red {
        DoctorStatus::Red
    } else {
        DoctorStatus::Yellow
    };
    let summary = diagnostics
        .iter()
        .take(3)
        .map(|diag| format!("{}: {}", diag.rule, diag.message))
        .collect::<Vec<_>>()
        .join("; ");
    check("lint", status, summary)
}

fn prompt_asset_check(
    catalog: &ResolvedPersonaManifest,
    entry_source: &Option<PathBuf>,
) -> DoctorCheck {
    let prompt_dir = catalog.manifest_dir.join("prompts");
    let prompt_files = collect_prompt_files(&prompt_dir);
    if prompt_files.is_empty() {
        return check(
            "prompt-assets",
            DoctorStatus::Yellow,
            "no .harn.prompt assets found",
        );
    }
    for path in &prompt_files {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                return check(
                    "prompt-assets",
                    DoctorStatus::Red,
                    format!("failed to read {}: {error}", path.display()),
                )
            }
        };
        if let Err(error) = harn_vm::stdlib::template::validate_template_syntax(&source) {
            return check(
                "prompt-assets",
                DoctorStatus::Red,
                format!("{}: {error}", path.display()),
            );
        }
    }
    let uses_prompt_asset = entry_source
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|source| source.contains("render_prompt(") || source.contains("render("));
    let status = if uses_prompt_asset {
        DoctorStatus::Green
    } else {
        DoctorStatus::Yellow
    };
    check(
        "prompt-assets",
        status,
        format!("{} prompt asset(s) validate", prompt_files.len()),
    )
}

fn step_metadata_check(persona: &PersonaManifestEntry) -> DoctorCheck {
    if persona.steps.is_empty() {
        return check(
            "step-metadata",
            DoctorStatus::Red,
            "entry source did not expose typed @step metadata",
        );
    }
    let missing_receipt = persona
        .steps
        .iter()
        .filter(|step| step.receipt.as_deref().unwrap_or_default().is_empty())
        .count();
    if missing_receipt > 0 {
        return check(
            "step-metadata",
            DoctorStatus::Yellow,
            format!(
                "{} step(s) found, {missing_receipt} without explicit receipt policy",
                persona.steps.len()
            ),
        );
    }
    check(
        "step-metadata",
        DoctorStatus::Green,
        format!("{} typed step(s) found", persona.steps.len()),
    )
}

fn cost_check(persona: &PersonaManifestEntry) -> DoctorCheck {
    let step_token_budget: u64 = persona
        .steps
        .iter()
        .filter_map(|step| step.budget.as_ref()?.max_tokens)
        .sum();
    let Some(max_tokens) = persona.budget.max_tokens else {
        return check(
            "cost-budget",
            DoctorStatus::Yellow,
            "manifest has no max_tokens budget",
        );
    };
    if step_token_budget == 0 {
        return check(
            "cost-budget",
            DoctorStatus::Yellow,
            format!("manifest max_tokens={max_tokens}, no per-step token budgets"),
        );
    }
    if step_token_budget > max_tokens {
        return check(
            "cost-budget",
            DoctorStatus::Red,
            format!("per-step max_tokens sum {step_token_budget} exceeds manifest max_tokens {max_tokens}"),
        );
    }
    check(
        "cost-budget",
        DoctorStatus::Green,
        format!(
            "per-step max_tokens sum {step_token_budget} within manifest max_tokens {max_tokens}"
        ),
    )
}

async fn smoke_check(
    catalog: &ResolvedPersonaManifest,
    persona_name: &str,
    timeout_ms: u64,
) -> DoctorCheck {
    let test_path = catalog
        .manifest_dir
        .join("tests")
        .join(format!("{persona_name}_smoke.harn"));
    if !test_path.exists() {
        return check(
            "smoke-test",
            DoctorStatus::Yellow,
            format!("{} not found", test_path.display()),
        );
    }
    let summary = test_runner::run_tests(&test_path, None, timeout_ms, false).await;
    if summary.failed > 0 {
        let first_error = summary
            .results
            .iter()
            .find(|result| !result.passed)
            .and_then(|result| result.error.as_deref())
            .unwrap_or("smoke test failed");
        return check(
            "smoke-test",
            DoctorStatus::Red,
            format!("{first_error} ({} failed)", summary.failed),
        );
    }
    if summary.total == 0 {
        return check(
            "smoke-test",
            DoctorStatus::Yellow,
            "no test pipelines found",
        );
    }
    check(
        "smoke-test",
        DoctorStatus::Green,
        format!("{} smoke test(s) passed", summary.passed),
    )
}

fn collect_package_harn_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    crate::commands::collect_harn_files(dir, &mut files);
    files
}

fn collect_prompt_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_prompt_files_inner(dir, &mut files);
    files.sort();
    files
}

fn collect_prompt_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_prompt_files_inner(&path, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".harn.prompt"))
        {
            out.push(path);
        }
    }
}
