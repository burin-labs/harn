//! Live rule-engine diagnostics for harn-lsp.
//!
//! The normal Harn-language diagnostics still come from the parser,
//! typechecker, and harn-lint. This module adds the language-agnostic rule
//! engine path: load configured TOML rules, run rules that target the opened
//! document's language, and expose their fixes through the same LSP
//! `repair_id`/`safety` envelope as native Harn repairs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_hostlib::ast::Language;
use harn_rules::{CompiledRule, Rule, Safety, Severity};
use serde::Deserialize;
use serde_json::Value;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range, TextEdit, Url};

use crate::source_text::SourceText;

const RULE_DIAGNOSTIC_CODE: &str = "HARN-RUL-001";
const RULE_CONFIG_CODE: &str = "HARN-RUL-CONFIG";
const RULE_SOURCE: &str = "harn-rules";

#[derive(Clone)]
pub(crate) struct RuleWorkspace {
    root: Option<PathBuf>,
    enabled: bool,
    specs: Arc<Vec<RuleSpec>>,
    load_errors: Arc<Vec<RuleLoadError>>,
    _package_snapshot: Option<Arc<harn_modules::package_snapshot::PackageSnapshot>>,
}

impl Default for RuleWorkspace {
    fn default() -> Self {
        Self {
            root: None,
            enabled: true,
            specs: Arc::new(Vec::new()),
            load_errors: Arc::new(Vec::new()),
            _package_snapshot: None,
        }
    }
}

impl RuleWorkspace {
    pub(crate) fn from_initialize(params: &tower_lsp::lsp_types::InitializeParams) -> Self {
        let root = workspace_root(params);
        let settings = RuleSettings::from_value(params.initialization_options.as_ref());
        Self::load(root, settings)
    }

    pub(crate) fn reconfigure(&mut self, value: Option<&Value>) {
        let root = self.root.clone();
        let settings = RuleSettings::from_value(value);
        *self = Self::load(root, settings);
    }

    #[cfg(test)]
    pub(crate) fn from_root(root: impl Into<PathBuf>) -> Self {
        Self::load(Some(root.into()), RuleSettings::default())
    }

    pub(crate) fn diagnostics_for_document(
        &self,
        uri: &Url,
        language_id: &str,
        source: &SourceText,
    ) -> Vec<RuleDiagnostic> {
        if !self.enabled {
            return Vec::new();
        }

        let language = document_language(uri, language_id);
        let mut diagnostics = self
            .load_errors
            .iter()
            .map(RuleDiagnostic::from_load_error)
            .collect::<Vec<_>>();

        let Some(language) = language else {
            return diagnostics;
        };

        for spec in self.specs.iter().filter(|spec| spec.language == language) {
            diagnostics.extend(spec.diagnostics(source));
        }

        diagnostics
    }

    fn load(root: Option<PathBuf>, settings: RuleSettings) -> Self {
        if !settings.enabled {
            return Self {
                root,
                enabled: false,
                specs: Arc::new(Vec::new()),
                load_errors: Arc::new(Vec::new()),
                _package_snapshot: None,
            };
        }

        let mut specs = Vec::new();
        let mut errors = Vec::new();
        let Some(root_path) = root.as_deref() else {
            return Self {
                root,
                enabled: true,
                specs: Arc::new(specs),
                load_errors: Arc::new(errors),
                _package_snapshot: None,
            };
        };

        let package_snapshot =
            match harn_modules::package_snapshot::PackageSnapshot::acquire(root_path) {
                Ok(snapshot) => snapshot.map(Arc::new),
                Err(error) => {
                    if !settings.rule_packs.is_empty() {
                        errors.push(RuleLoadError {
                            path: root_path.to_path_buf(),
                            message: format!("load package generation: {error}"),
                        });
                    }
                    None
                }
            };

        let mut seen_dirs = HashSet::new();
        for dir in project_rule_dirs(root_path).into_iter().chain(
            settings
                .rule_dirs
                .iter()
                .map(|dir| resolve_path(root_path, dir)),
        ) {
            if seen_dirs.insert(dir.clone()) {
                load_rule_dir(&dir, &mut specs, &mut errors);
            }
        }

        for pack in &settings.rule_packs {
            match resolve_rule_pack(root_path, package_snapshot.as_deref(), pack) {
                Some(paths) => {
                    for dir in paths {
                        if seen_dirs.insert(dir.clone()) {
                            load_rule_dir(&dir, &mut specs, &mut errors);
                        }
                    }
                }
                None => errors.push(RuleLoadError {
                    path: root_path.to_path_buf(),
                    message: format!("rule pack `{pack}` is not installed or is not a directory"),
                }),
            }
        }

        Self {
            root,
            enabled: true,
            specs: Arc::new(specs),
            load_errors: Arc::new(errors),
            _package_snapshot: package_snapshot,
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuleDiagnostic {
    pub(crate) diagnostic: Diagnostic,
    pub(crate) edit: Option<TextEdit>,
    pub(crate) repair_id: Option<String>,
    pub(crate) title: String,
}

impl RuleDiagnostic {
    fn from_load_error(error: &RuleLoadError) -> Self {
        Self {
            diagnostic: Diagnostic {
                range: Range::default(),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some(RULE_SOURCE.to_string()),
                code: Some(NumberOrString::String(RULE_CONFIG_CODE.to_string())),
                message: format!(
                    "Rule pack load failed for `{}`: {}",
                    error.path.display(),
                    error.message
                ),
                ..Default::default()
            },
            edit: None,
            repair_id: None,
            title: "Rule pack load failed".to_string(),
        }
    }

    fn from_rule(rule: &RuleSpec, diag: harn_rules::Diagnostic, source: &SourceText) -> Self {
        let range = rule_span_to_range(&diag.span, source);
        let repair_id = diag.fix.as_ref().map(|_| {
            format!(
                "rules/{}/{}-{}",
                diag.rule_id, diag.span.start_byte, diag.span.end_byte
            )
        });
        let safety = rule.safety.as_str();
        let title = if diag.fix.is_some() {
            format!("Apply rule fix `{}`", diag.rule_id)
        } else {
            format!("Inspect rule `{}`", diag.rule_id)
        };
        let data = repair_id.as_ref().map(|id| {
            serde_json::json!({
                "code": RULE_DIAGNOSTIC_CODE,
                "rule_id": diag.rule_id,
                "repair_id": id,
                "safety": safety,
                "repair": {
                    "id": id,
                    "summary": title,
                    "safety": safety,
                },
            })
        });
        let message = if diag.message.is_empty() {
            format!("[{}] rule matched", diag.rule_id)
        } else {
            format!("[{}] {}", diag.rule_id, diag.message)
        };
        Self {
            diagnostic: Diagnostic {
                range,
                severity: Some(severity_to_lsp(diag.severity)),
                source: Some(RULE_SOURCE.to_string()),
                code: Some(NumberOrString::String(RULE_DIAGNOSTIC_CODE.to_string())),
                message,
                data,
                ..Default::default()
            },
            edit: diag.fix.map(|replacement| TextEdit {
                range,
                new_text: replacement,
            }),
            repair_id,
            title,
        }
    }
}

#[derive(Clone)]
struct RuleSpec {
    path: PathBuf,
    language: Language,
    rule: Rule,
    safety: Safety,
}

impl RuleSpec {
    fn diagnostics(&self, source: &SourceText) -> Vec<RuleDiagnostic> {
        let compiled = match CompiledRule::compile(&self.rule) {
            Ok(compiled) => compiled,
            Err(error) => {
                return vec![RuleDiagnostic::from_load_error(&RuleLoadError {
                    path: self.path.clone(),
                    message: error.to_string(),
                })];
            }
        };
        match compiled.diagnostics(source) {
            Ok(items) => items
                .into_iter()
                .map(|diagnostic| RuleDiagnostic::from_rule(self, diagnostic, source))
                .collect(),
            Err(error) => vec![RuleDiagnostic::from_load_error(&RuleLoadError {
                path: self.path.clone(),
                message: error.to_string(),
            })],
        }
    }
}

#[derive(Clone)]
struct RuleLoadError {
    path: PathBuf,
    message: String,
}

struct RuleSettings {
    enabled: bool,
    rule_dirs: Vec<String>,
    rule_packs: Vec<String>,
}

impl Default for RuleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            rule_dirs: Vec::new(),
            rule_packs: Vec::new(),
        }
    }
}

impl RuleSettings {
    fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self {
                enabled: true,
                ..Default::default()
            };
        };
        let harn = value.get("harn").unwrap_or(value);
        let rules = harn
            .get("rules")
            .or_else(|| harn.get("ruleEngine"))
            .unwrap_or(harn);
        Self {
            enabled: rules
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            rule_dirs: string_array(rules, &["ruleDirs", "rule_dirs", "rule-dirs"]),
            rule_packs: string_array(rules, &["rulePacks", "rule_packs", "rule-packs"]),
        }
    }
}

fn string_array(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn workspace_root(params: &tower_lsp::lsp_types::InitializeParams) -> Option<PathBuf> {
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.uri.to_file_path().ok())
        .or_else(|| {
            params
                .root_uri
                .as_ref()
                .and_then(|uri| uri.to_file_path().ok())
        })
}

fn document_language(uri: &Url, language_id: &str) -> Option<Language> {
    let normalized = match language_id {
        "typescriptreact" => "tsx",
        "javascriptreact" => "jsx",
        other => other,
    };
    let path = uri.to_file_path().ok();
    path.as_deref()
        .and_then(|path| {
            Language::detect(path, Some(normalized)).or_else(|| Language::detect(path, None))
        })
        .or_else(|| Language::from_name(normalized))
}

fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[derive(Default, Deserialize)]
struct MinimalManifest {
    #[serde(default)]
    rules: MinimalRules,
}

#[derive(Default, Deserialize)]
struct MinimalRules {
    #[serde(default, alias = "ruleDirs", alias = "rule-dirs")]
    rule_dirs: Vec<String>,
}

fn project_rule_dirs(root: &Path) -> Vec<PathBuf> {
    let Some((manifest, dir)) = find_nearest_manifest(root) else {
        return Vec::new();
    };
    manifest
        .rules
        .rule_dirs
        .iter()
        .map(|rel| dir.join(rel))
        .collect()
}

/// Locate the nearest `harn.toml` via the shared project-root walk and parse
/// just the `[rules]` view the LSP needs. Using the shared walk keeps the
/// server's notion of the project root identical to the CLI's.
fn find_nearest_manifest(start: &Path) -> Option<(MinimalManifest, PathBuf)> {
    let found = harn_modules::manifest_walk::find_nearest_manifest(start)?;
    let source = std::fs::read_to_string(&found.path).ok()?;
    let manifest = toml::from_str::<MinimalManifest>(&source).ok()?;
    Some((manifest, found.dir))
}

fn load_rule_dir(dir: &Path, specs: &mut Vec<RuleSpec>, errors: &mut Vec<RuleLoadError>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(RuleLoadError {
                path: dir.to_path_buf(),
                message: format!("read `{}`: {error}", dir.display()),
            });
            return;
        }
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    files.sort();
    for file in files {
        load_rule_file(&file, specs, errors);
    }
}

fn load_rule_file(path: &Path, specs: &mut Vec<RuleSpec>, errors: &mut Vec<RuleLoadError>) {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            errors.push(RuleLoadError {
                path: path.to_path_buf(),
                message: format!("read `{}`: {error}", path.display()),
            });
            return;
        }
    };
    let rule = match Rule::from_toml_str(&source) {
        Ok(rule) => rule,
        Err(error) => {
            errors.push(RuleLoadError {
                path: path.to_path_buf(),
                message: format!("parse `{}`: {error}", path.display()),
            });
            return;
        }
    };
    let Some(language) = Language::from_name(&rule.language) else {
        errors.push(RuleLoadError {
            path: path.to_path_buf(),
            message: format!(
                "rule `{}` uses unknown language `{}`",
                rule.id, rule.language
            ),
        });
        return;
    };
    specs.push(RuleSpec {
        path: path.to_path_buf(),
        language,
        safety: rule.safety,
        rule,
    });
}

fn resolve_rule_pack(
    root: &Path,
    package_snapshot: Option<&harn_modules::package_snapshot::PackageSnapshot>,
    pack: &str,
) -> Option<Vec<PathBuf>> {
    let local = resolve_path(root, pack);
    if local.is_dir() {
        return Some(pack_rule_dirs(&local));
    }

    let snapshot = package_snapshot?;
    let package_dir = snapshot.packages_root().join(pack);
    if package_dir.is_dir() {
        return Some(pack_rule_dirs(&package_dir));
    }

    let locked = lockfile_package_dir(snapshot, pack)?;
    locked.is_dir().then(|| pack_rule_dirs(&locked))
}

fn pack_rule_dirs(dir: &Path) -> Vec<PathBuf> {
    let manifest_path = dir.join("harn.toml");
    if manifest_path.is_file() {
        if let Ok(source) = std::fs::read_to_string(&manifest_path) {
            if let Ok(manifest) = toml::from_str::<MinimalManifest>(&source) {
                if !manifest.rules.rule_dirs.is_empty() {
                    return manifest
                        .rules
                        .rule_dirs
                        .iter()
                        .map(|rel| dir.join(rel))
                        .collect();
                }
            }
        }
    }
    vec![dir.to_path_buf()]
}

#[derive(Deserialize)]
struct MinimalLockFile {
    #[serde(default, rename = "package")]
    packages: Vec<MinimalLockEntry>,
}

#[derive(Deserialize)]
struct MinimalLockEntry {
    name: String,
    #[serde(default)]
    registry: Option<MinimalRegistry>,
}

#[derive(Deserialize)]
struct MinimalRegistry {
    name: String,
}

fn lockfile_package_dir(
    snapshot: &harn_modules::package_snapshot::PackageSnapshot,
    pack: &str,
) -> Option<PathBuf> {
    let source = std::fs::read_to_string(snapshot.lock_path()).ok()?;
    let lock = toml::from_str::<MinimalLockFile>(&source).ok()?;
    let entry = lock.packages.iter().find(|entry| {
        entry.name == pack
            || entry
                .registry
                .as_ref()
                .is_some_and(|registry| registry.name == pack)
    })?;
    Some(snapshot.packages_root().join(&entry.name))
}

fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Error => DiagnosticSeverity::ERROR,
    }
}

fn rule_span_to_range(span: &harn_rules::Span, source: &SourceText) -> Range {
    Range {
        start: source.position(span.start_byte),
        end: source.position(span.end_byte.max(span.start_byte + 1).min(source.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish_package_generation(root: &Path, alias: &str, registry_name: &str) -> PathBuf {
        use harn_modules::package_snapshot::{
            generation_root, package_current_path, package_lock_digest,
            package_publication_lock_path, PackageGenerationManifest, PackageGenerationPointer,
            GENERATION_LEASE_FILE, GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE,
            GENERATION_PACKAGES_DIR,
        };

        let generation = "generation-lsp-rules";
        let generation_root = generation_root(root, generation);
        let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
        std::fs::create_dir_all(&packages_root).unwrap();
        let lock = format!(
            "version = 4\n\n[[package]]\nname = {alias:?}\n\n[package.registry]\nname = {registry_name:?}\n"
        );
        write(&generation_root.join(GENERATION_LOCK_FILE), &lock);
        write(&generation_root.join(GENERATION_LEASE_FILE), "");
        let manifest =
            PackageGenerationManifest::new(generation, package_lock_digest(lock.as_bytes()))
                .unwrap();
        write(
            &generation_root.join(GENERATION_MANIFEST_FILE),
            &toml::to_string_pretty(&manifest).unwrap(),
        );
        let pointer = PackageGenerationPointer::new(generation).unwrap();
        write(
            &package_current_path(root),
            &toml::to_string_pretty(&pointer).unwrap(),
        );
        write(&package_publication_lock_path(root), "");
        packages_root.join(alias)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn project_rule_dir_publishes_diagnostic_with_repair_data() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("harn.toml"),
            "[rules]\nruleDirs = [\"rules\"]\n",
        );
        write(
            &temp.path().join("rules/no-debugger.toml"),
            r#"
id = "no-debugger"
language = "typescript"
message = "remove debugger statements"
severity = "warning"
safety = "behavior-preserving"
fix = ""

[rule]
regex = "debugger;"
"#,
        );

        let workspace = RuleWorkspace::from_root(temp.path());
        let uri = Url::from_file_path(temp.path().join("src/main.ts")).unwrap();
        let diagnostics = workspace.diagnostics_for_document(
            &uri,
            "typescript",
            &SourceText::new("function f() { debugger; }\n"),
        );

        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0].diagnostic;
        assert_eq!(diagnostic.source.as_deref(), Some(RULE_SOURCE));
        assert_eq!(
            diagnostic.message,
            "[no-debugger] remove debugger statements"
        );
        let data = diagnostic.data.as_ref().expect("repair data");
        assert_eq!(
            data.get("repair_id").and_then(Value::as_str),
            Some("rules/no-debugger/15-24")
        );
        assert_eq!(
            data.get("safety").and_then(Value::as_str),
            Some("behavior-preserving")
        );
        assert_eq!(diagnostics[0].edit.as_ref().unwrap().new_text, "");
    }

    #[test]
    fn document_language_falls_back_to_file_extension() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("harn.toml"),
            "[rules]\nruleDirs = [\"rules\"]\n",
        );
        write(
            &temp.path().join("rules/no-debugger.toml"),
            r#"
id = "no-debugger"
language = "typescript"
message = "remove debugger statements"
severity = "warning"
safety = "behavior-preserving"
fix = ""

[rule]
regex = "debugger;"
"#,
        );

        let workspace = RuleWorkspace::from_root(temp.path());
        let uri = Url::from_file_path(temp.path().join("src/main.ts")).unwrap();
        let diagnostics = workspace.diagnostics_for_document(
            &uri,
            "harn-rule-engine",
            &SourceText::new("function f() { debugger; }\n"),
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].diagnostic.message,
            "[no-debugger] remove debugger statements"
        );
    }

    #[test]
    fn registry_rule_pack_resolves_from_published_generation() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("harn.toml"),
            "[package]\nname = \"app\"\n",
        );
        let package = publish_package_generation(temp.path(), "rules-alias", "acme/rules");
        write(
            &package.join("harn.toml"),
            "[rules]\nruleDirs = [\"rules\"]\n",
        );
        write(
            &package.join("rules/no-debugger.toml"),
            r#"
id = "no-debugger"
language = "typescript"
message = "remove debugger statements"
severity = "warning"
safety = "behavior-preserving"
fix = ""

[rule]
regex = "debugger;"
"#,
        );

        let workspace = RuleWorkspace::load(
            Some(temp.path().to_path_buf()),
            RuleSettings {
                rule_packs: vec!["acme/rules".to_string()],
                ..RuleSettings::default()
            },
        );
        let uri = Url::from_file_path(temp.path().join("src/main.ts")).unwrap();
        let diagnostics =
            workspace.diagnostics_for_document(&uri, "typescript", &SourceText::new("debugger;\n"));

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].diagnostic.message,
            "[no-debugger] remove debugger statements"
        );
    }
}
