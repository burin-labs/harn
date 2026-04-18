use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::process::resolve_source_relative_path;
use super::project_catalog::{project_catalog, ProjectCatalogEntry};

const STANDARD_VENDOR_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];

#[derive(Debug, Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
enum ScanTier {
    #[default]
    Ambient,
    Config,
}

#[derive(Debug, Clone)]
struct ProjectScanOptions {
    tiers: BTreeSet<ScanTier>,
    depth: usize,
    include_hidden: bool,
    include_vendor: bool,
    respect_gitignore: bool,
}

impl Default for ProjectScanOptions {
    fn default() -> Self {
        Self {
            tiers: BTreeSet::from([ScanTier::Ambient]),
            depth: 3,
            include_hidden: false,
            include_vendor: false,
            respect_gitignore: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ProjectEvidence {
    path: PathBuf,
    language_scores: BTreeMap<String, f64>,
    framework_scores: BTreeMap<String, f64>,
    build_systems: BTreeSet<String>,
    vcs: Option<String>,
    lockfiles: BTreeSet<String>,
    anchors: BTreeSet<String>,
    package_name: Option<String>,
    build_commands: Vec<String>,
    declared_scripts: BTreeMap<String, String>,
    readme_code_fences: Vec<String>,
    dockerfile_commands: Vec<String>,
    makefile_targets: Vec<String>,
}

impl ProjectEvidence {
    fn into_vm_value(self) -> VmValue {
        let confidence = confidence_value(&self);
        let mut result = BTreeMap::new();
        result.insert(
            "path".to_string(),
            VmValue::String(Rc::from(self.path.to_string_lossy().into_owned())),
        );
        result.insert(
            "languages".to_string(),
            VmValue::List(Rc::new(
                sorted_confident_labels(&self.language_scores)
                    .into_iter()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "frameworks".to_string(),
            VmValue::List(Rc::new(
                sorted_confident_labels(&self.framework_scores)
                    .into_iter()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "build_systems".to_string(),
            VmValue::List(Rc::new(
                self.build_systems
                    .into_iter()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "vcs".to_string(),
            self.vcs
                .map(|value| VmValue::String(Rc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "lockfiles".to_string(),
            VmValue::List(Rc::new(
                self.lockfiles
                    .into_iter()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "anchors".to_string(),
            VmValue::List(Rc::new(
                self.anchors
                    .into_iter()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert("confidence".to_string(), confidence);
        result.insert(
            "package_name".to_string(),
            self.package_name
                .map(|value| VmValue::String(Rc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "build_commands".to_string(),
            VmValue::List(Rc::new(
                self.build_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(Rc::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "declared_scripts".to_string(),
            VmValue::Dict(Rc::new(
                self.declared_scripts
                    .into_iter()
                    .map(|(k, v)| (k, VmValue::String(Rc::from(v))))
                    .collect(),
            )),
        );
        result.insert(
            "readme_code_fences".to_string(),
            VmValue::List(Rc::new(
                self.readme_code_fences
                    .into_iter()
                    .map(|lang| VmValue::String(Rc::from(lang)))
                    .collect(),
            )),
        );
        result.insert(
            "dockerfile_commands".to_string(),
            VmValue::List(Rc::new(
                self.dockerfile_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(Rc::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "makefile_targets".to_string(),
            VmValue::List(Rc::new(
                self.makefile_targets
                    .into_iter()
                    .map(|target| VmValue::String(Rc::from(target)))
                    .collect(),
            )),
        );
        VmValue::Dict(Rc::new(result))
    }
}

pub(crate) fn register_project_builtins(vm: &mut Vm) {
    vm.register_builtin("project_scan_native", |args, _out| {
        let path = args
            .first()
            .map(|value| value.display())
            .unwrap_or_else(|| ".".to_string());
        let options = parse_project_options(args.get(1));
        let root = resolve_existing_directory(&path)?;
        Ok(scan_exact_directory(&root, &options).into_vm_value())
    });

    vm.register_builtin("project_scan_tree_native", |args, _out| {
        let path = args
            .first()
            .map(|value| value.display())
            .unwrap_or_else(|| ".".to_string());
        let options = parse_project_options(args.get(1));
        let base = resolve_existing_directory(&path)?;
        let tree = scan_project_tree(&base, &options)?;
        Ok(VmValue::Dict(Rc::new(
            tree.into_iter()
                .map(|(rel, evidence)| (rel, evidence.into_vm_value()))
                .collect(),
        )))
    });

    vm.register_builtin("project_catalog_native", |_args, _out| {
        let entries = project_catalog()
            .iter()
            .map(catalog_entry_value)
            .collect::<Vec<_>>();
        Ok(VmValue::List(Rc::new(entries)))
    });
}

fn parse_project_options(value: Option<&VmValue>) -> ProjectScanOptions {
    let mut options = ProjectScanOptions::default();
    let Some(dict) = value.and_then(VmValue::as_dict) else {
        return options;
    };

    if let Some(raw_depth) = dict.get("depth").and_then(VmValue::as_int) {
        options.depth = raw_depth.max(0) as usize;
    }
    if let Some(include_hidden) = dict.get("include_hidden").and_then(value_as_bool) {
        options.include_hidden = include_hidden;
    }
    if let Some(include_vendor) = dict.get("include_vendor").and_then(value_as_bool) {
        options.include_vendor = include_vendor;
    }
    if let Some(respect_gitignore) = dict.get("respect_gitignore").and_then(value_as_bool) {
        options.respect_gitignore = respect_gitignore;
    }
    if let Some(tiers) = dict.get("tiers").and_then(value_as_list) {
        options.tiers.clear();
        for tier in tiers.iter().map(VmValue::display) {
            match tier.as_str() {
                "ambient" => {
                    options.tiers.insert(ScanTier::Ambient);
                }
                "config" => {
                    options.tiers.insert(ScanTier::Config);
                }
                _ => {}
            }
        }
        if options.tiers.is_empty() {
            options.tiers.insert(ScanTier::Ambient);
        }
    }

    options
}

fn value_as_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn value_as_list(value: &VmValue) -> Option<&[VmValue]> {
    match value {
        VmValue::List(items) => Some(items.as_slice()),
        _ => None,
    }
}

fn resolve_existing_directory(path: &str) -> Result<PathBuf, VmError> {
    let resolved = resolve_source_relative_path(path);
    let target = if resolved.is_dir() {
        resolved
    } else {
        resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    if target.exists() {
        target.canonicalize().map_err(path_error)
    } else {
        Err(path_missing_error(&target))
    }
}

fn path_error(error: std::io::Error) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(format!(
        "project.scan: failed to resolve path: {error}"
    ))))
}

fn path_missing_error(path: &Path) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(format!(
        "project.scan: path does not exist: {}",
        path.display()
    ))))
}

fn scan_project_tree(
    base: &Path,
    options: &ProjectScanOptions,
) -> Result<BTreeMap<String, ProjectEvidence>, VmError> {
    let gitignore = build_gitignore(base, options.respect_gitignore);
    let mut builder = WalkBuilder::new(base);
    builder
        .hidden(!options.include_hidden)
        .follow_links(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .max_depth(Some(options.depth))
        .sort_by_file_name(|left, right| left.cmp(right));

    let include_vendor = options.include_vendor;
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let Some(file_type) = entry.file_type() else {
            return true;
        };
        if gitignore
            .matched_path_or_any_parents(entry.path(), file_type.is_dir())
            .is_ignore()
        {
            return false;
        }
        if !file_type.is_dir() {
            return true;
        }
        if include_vendor {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        if STANDARD_VENDOR_DIRS.contains(&name.as_ref()) {
            return false;
        }
        true
    });

    let mut results = BTreeMap::new();
    results.insert(".".to_string(), scan_exact_directory(base, options));

    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 || !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        if !is_project_root_candidate(dir) {
            continue;
        }
        let rel = relative_posix(base, dir);
        results
            .entry(rel)
            .or_insert_with(|| scan_exact_directory(dir, options));
    }

    Ok(results)
}

fn build_gitignore(base: &Path, enabled: bool) -> Gitignore {
    let mut builder = GitignoreBuilder::new(base);
    if enabled {
        let _ = builder.add(base.join(".gitignore"));
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn scan_exact_directory(dir: &Path, options: &ProjectScanOptions) -> ProjectEvidence {
    let mut evidence = ProjectEvidence {
        path: dir.to_path_buf(),
        vcs: detect_vcs(dir),
        ..ProjectEvidence::default()
    };
    let mut build_commands = Vec::new();

    for entry in project_catalog() {
        let found_anchors = collect_present(dir, entry.anchors);
        let found_lockfiles = collect_present(dir, entry.lockfiles);
        if found_anchors.is_empty() && found_lockfiles.is_empty() {
            continue;
        }

        let has_source = entry_has_source(dir, entry, options);
        let score = entry_confidence(
            entry,
            !found_anchors.is_empty(),
            !found_lockfiles.is_empty(),
            has_source,
        );
        if score <= 0.0 {
            continue;
        }

        evidence.anchors.extend(
            found_anchors
                .into_iter()
                .map(|value| maybe_dir_suffix(dir, &value)),
        );
        evidence.lockfiles.extend(found_lockfiles);

        for language in entry.languages {
            record_score(&mut evidence.language_scores, language, score);
        }
        for framework in entry.frameworks {
            record_score(&mut evidence.framework_scores, framework, score);
        }
        if score >= 0.5 {
            evidence
                .build_systems
                .extend(entry.build_systems.iter().map(|value| value.to_string()));
            push_unique_option(&mut build_commands, entry.default_build_cmd);
            push_unique_option(&mut build_commands, entry.default_test_cmd);
        }
    }

    if options.tiers.contains(&ScanTier::Config) {
        evidence.package_name = detect_package_name(dir);
        apply_config_tier(dir, &mut evidence, &mut build_commands);
    }

    evidence.build_commands = build_commands;
    evidence
}

fn apply_config_tier(dir: &Path, evidence: &mut ProjectEvidence, build_commands: &mut Vec<String>) {
    if let Some(package_json) = read_json_object(dir.join("package.json")) {
        if let Some(scripts) = package_json
            .get("scripts")
            .and_then(|value| value.as_object())
        {
            for (name, command) in scripts {
                let Some(command) = command.as_str() else {
                    continue;
                };
                evidence
                    .declared_scripts
                    .insert(name.clone(), command.to_string());
            }
            for key in ["build", "test", "lint", "dev", "start"] {
                if scripts.contains_key(key) {
                    let command = if key == "test" {
                        "npm test".to_string()
                    } else {
                        format!("npm run {key}")
                    };
                    push_unique(build_commands, command);
                }
            }
        }
    }

    if let Some(dockerfile) = read_text_if_exists(dir.join("Dockerfile")) {
        evidence.dockerfile_commands = parse_dockerfile_commands(&dockerfile);
    }

    if let Some(makefile) = read_first_existing_text(dir, &["GNUmakefile", "Makefile", "makefile"])
    {
        evidence.makefile_targets = parse_makefile_targets(&makefile);
        for key in ["build", "test"] {
            if evidence.makefile_targets.iter().any(|target| target == key) {
                push_unique(build_commands, format!("make {key}"));
            }
        }
    }

    if let Some(readme) =
        read_first_existing_text(dir, &["README.md", "README.MD", "README", "Readme.md"])
    {
        evidence.readme_code_fences = parse_readme_code_fences(&readme);
    }
}

fn detect_package_name(dir: &Path) -> Option<String> {
    package_name_from_pyproject(dir)
        .or_else(|| package_name_from_package_json(dir))
        .or_else(|| package_name_from_go_mod(dir))
        .or_else(|| package_name_from_cargo_toml(dir))
}

fn package_name_from_pyproject(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("pyproject.toml"))?;
    let parsed = toml::from_str::<toml::Value>(&text).ok()?;
    parsed
        .get("project")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            parsed
                .get("tool")
                .and_then(|value| value.get("poetry"))
                .and_then(|value| value.get("name"))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        })
}

fn package_name_from_package_json(dir: &Path) -> Option<String> {
    let parsed = read_json_object(dir.join("package.json"))?;
    parsed
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn package_name_from_go_mod(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("go.mod"))?;
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        let module_path = trimmed.strip_prefix("module ")?;
        module_path.rsplit('/').next().map(str::to_string)
    })
}

fn package_name_from_cargo_toml(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("Cargo.toml"))?;
    let parsed = toml::from_str::<toml::Value>(&text).ok()?;
    parsed
        .get("package")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

fn read_first_existing_text(dir: &Path, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| read_text_if_exists(dir.join(name)))
}

fn read_text_if_exists(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_json_object(path: PathBuf) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    parsed.as_object().cloned()
}

fn parse_readme_code_fences(readme: &str) -> Vec<String> {
    let mut fences = Vec::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if let Some(lang) = trimmed.strip_prefix("```") {
            let lang = lang.split_whitespace().next().unwrap_or_default().trim();
            if !lang.is_empty() {
                push_unique(&mut fences, lang.to_string());
            }
        }
    }
    fences
}

fn parse_dockerfile_commands(dockerfile: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut pending = String::new();

    for raw_line in dockerfile.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if pending.is_empty() {
            pending = line.to_string();
        } else {
            pending.push(' ');
            pending.push_str(line);
        }

        if pending.ends_with('\\') {
            pending.pop();
            pending = pending.trim_end().to_string();
            continue;
        }

        let upper = pending.to_ascii_uppercase();
        for keyword in ["RUN ", "CMD ", "ENTRYPOINT "] {
            if upper.starts_with(keyword) {
                push_unique(&mut commands, pending[keyword.len()..].trim().to_string());
                break;
            }
        }
        pending.clear();
    }

    commands
}

fn parse_makefile_targets(makefile: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in makefile.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with('.')
            || trimmed.contains(":=")
            || trimmed.contains("?=")
            || trimmed.contains("+=")
            || trimmed.contains('=')
        {
            continue;
        }
        let Some((target, _rest)) = trimmed.split_once(':') else {
            continue;
        };
        if target.contains('%') || target.contains(' ') || target.is_empty() {
            continue;
        }
        if target
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            push_unique(&mut targets, target.to_string());
        }
    }
    targets
}

fn maybe_dir_suffix(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    if path.is_dir() {
        format!("{name}/")
    } else {
        name.to_string()
    }
}

fn collect_present(dir: &Path, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| dir.join(name).exists())
        .map(|name| (*name).to_string())
        .collect()
}

fn entry_has_source(dir: &Path, entry: &ProjectCatalogEntry, options: &ProjectScanOptions) -> bool {
    if entry.source_globs.is_empty() {
        return false;
    }
    scan_sources(dir, dir, entry, options, 0)
}

fn scan_sources(
    root: &Path,
    dir: &Path,
    entry: &ProjectCatalogEntry,
    options: &ProjectScanOptions,
    depth: usize,
) -> bool {
    if depth > options.depth.saturating_add(2) {
        return false;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in read_dir.flatten() {
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        let name = child.file_name().to_string_lossy().into_owned();
        if !options.include_hidden && name.starts_with('.') {
            continue;
        }
        if file_type.is_dir() {
            if !options.include_vendor && STANDARD_VENDOR_DIRS.contains(&name.as_str()) {
                continue;
            }
            if child.path() != root && is_project_root_candidate(&child.path()) {
                continue;
            }
            if scan_sources(root, &child.path(), entry, options, depth + 1) {
                return true;
            }
            continue;
        }
        let rel = relative_posix(root, &child.path());
        if entry
            .source_globs
            .iter()
            .any(|pattern| simple_glob_match(pattern, &rel))
        {
            return true;
        }
    }
    false
}

fn entry_confidence(
    entry: &ProjectCatalogEntry,
    has_anchor: bool,
    has_lockfile: bool,
    has_source: bool,
) -> f64 {
    let mut score: f64 = 0.0;
    if has_anchor {
        score = score.max(entry.anchor_score);
    }
    if has_source {
        score = score.max(0.5);
    }
    if has_lockfile {
        score = if has_source {
            1.0
        } else {
            (score + 0.45).min(1.0)
        };
    }
    score
}

fn detect_vcs(dir: &Path) -> Option<String> {
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        if path.join(".git").exists() {
            return Some("git".to_string());
        }
        cursor = path.parent();
    }
    None
}

fn is_project_root_candidate(dir: &Path) -> bool {
    if dir.join(".git").exists() || dir.join("harn.toml").is_file() {
        return true;
    }
    project_catalog().iter().any(|entry| {
        entry
            .anchors
            .iter()
            .chain(entry.lockfiles.iter())
            .any(|name| dir.join(name).exists())
    })
}

fn relative_posix(base: &Path, path: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn simple_glob_match(pattern: &str, candidate: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("**/") {
        return candidate == suffix || candidate.ends_with(&format!("/{suffix}"));
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return candidate
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with(&format!(".{suffix}")));
    }
    if pattern.contains("/**/*.") {
        let Some((prefix, ext)) = pattern.split_once("/**/*.") else {
            return false;
        };
        return candidate.starts_with(prefix)
            && candidate
                .rsplit('/')
                .next()
                .is_some_and(|name| name.ends_with(&format!(".{ext}")));
    }
    candidate == pattern
}

fn record_score(scores: &mut BTreeMap<String, f64>, label: &str, score: f64) {
    scores
        .entry(label.to_string())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn confidence_value(evidence: &ProjectEvidence) -> VmValue {
    let mut confidence = BTreeMap::new();
    for (label, score) in evidence
        .language_scores
        .iter()
        .chain(evidence.framework_scores.iter())
    {
        confidence.insert(label.clone(), VmValue::Float(*score));
    }
    VmValue::Dict(Rc::new(confidence))
}

fn sorted_confident_labels(scores: &BTreeMap<String, f64>) -> Vec<String> {
    let mut items = scores
        .iter()
        .filter(|(_label, score)| **score >= 0.5)
        .map(|(label, score)| (label.clone(), *score))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    items.into_iter().map(|(label, _score)| label).collect()
}

fn push_unique_option(values: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        push_unique(values, value.to_string());
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn catalog_entry_value(entry: &ProjectCatalogEntry) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "id".to_string(),
        VmValue::String(Rc::from(entry.id.to_string())),
    );
    value.insert(
        "languages".to_string(),
        VmValue::List(Rc::new(
            entry
                .languages
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "frameworks".to_string(),
        VmValue::List(Rc::new(
            entry
                .frameworks
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "build_systems".to_string(),
        VmValue::List(Rc::new(
            entry
                .build_systems
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "anchors".to_string(),
        VmValue::List(Rc::new(
            entry
                .anchors
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "lockfiles".to_string(),
        VmValue::List(Rc::new(
            entry
                .lockfiles
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "source_globs".to_string(),
        VmValue::List(Rc::new(
            entry
                .source_globs
                .iter()
                .map(|item| VmValue::String(Rc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "default_build_cmd".to_string(),
        entry
            .default_build_cmd
            .map(|value| VmValue::String(Rc::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "default_test_cmd".to_string(),
        entry
            .default_test_cmd
            .map(|value| VmValue::String(Rc::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("harn-project-{label}-"))
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn scan_detects_rust_workspace_root_without_nested_source_walk() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate dir")
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let evidence = scan_exact_directory(&repo_root, &ProjectScanOptions::default());
        assert!(sorted_confident_labels(&evidence.language_scores).contains(&"rust".to_string()));
        assert!(
            evidence
                .language_scores
                .get("rust")
                .copied()
                .unwrap_or_default()
                >= 0.95
        );
    }

    #[test]
    fn package_name_detection_matches_manifest_priority() {
        let dir = temp_dir("package-name");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"python-name\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"node-name\"}").unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module github.com/acme/go-name\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"cargo-name\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        assert_eq!(
            detect_package_name(dir.path()).as_deref(),
            Some("python-name")
        );
    }

    #[test]
    fn scan_tree_respects_gitignore_and_vendor_dirs_by_default() {
        let dir = temp_dir("tree-ignore");
        std::fs::create_dir_all(dir.path().join("frontend/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("ignored/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(
            dir.path().join("frontend/package.json"),
            "{\"name\":\"frontend\"}",
        )
        .unwrap();
        std::fs::write(dir.path().join("frontend/package-lock.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("frontend/src/app.ts"),
            "export const x = 1;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ignored/go.mod"), "module ignored\n").unwrap();
        std::fs::write(
            dir.path().join("node_modules/pkg/package.json"),
            "{\"name\":\"pkg\"}",
        )
        .unwrap();

        let tree = scan_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
        assert!(tree.contains_key("."));
        assert!(tree.contains_key("frontend"));
        assert!(!tree.contains_key("ignored"));
        assert!(!tree.contains_key("node_modules"));
    }
}
