//! Deterministic code-grounding helpers for package/API verification.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::llm::helpers::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::process::resolve_source_relative_path;

#[derive(Debug, Clone)]
struct ImportRef {
    path: String,
    language: String,
    ecosystem: String,
    raw: String,
    package: String,
    symbol: Option<String>,
    local: bool,
    standard: bool,
}

#[derive(Debug, Clone, Default)]
struct RegistryEntry {
    ecosystem: String,
    name: String,
    aliases: BTreeSet<String>,
    exists: bool,
    symbols: Option<BTreeSet<String>>,
    source_url: Option<String>,
    docs_url: Option<String>,
    version: Option<String>,
    trust_score: Option<f64>,
    first_seen: Option<String>,
    published_at: Option<String>,
}

#[derive(Debug, Default)]
struct ManifestPackages {
    packages: BTreeMap<String, BTreeSet<String>>,
    paths: BTreeSet<String>,
}

#[derive(Debug)]
struct VerifyOptions {
    project_root: PathBuf,
    registry: Vec<RegistryEntry>,
    installed: BTreeMap<String, BTreeSet<String>>,
    low_trust_threshold: f64,
    min_package_age_days: i64,
    now: Option<NaiveDate>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            project_root: resolve_source_relative_path("."),
            registry: Vec::new(),
            installed: BTreeMap::new(),
            low_trust_threshold: 0.35,
            min_package_age_days: 30,
            now: None,
        }
    }
}

pub(crate) fn register_grounding_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&VERIFY_IMPORTS_IMPL_DEF];

#[harn_builtin(
    sig = "__verify_imports(paths: list|string, options?: dict) -> dict",
    category = "grounding"
)]
fn verify_imports_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let paths = parse_paths(args.first())?;
    let options = parse_verify_options(args.get(1))?;
    let resolved_paths = paths
        .iter()
        .map(|path| resolve_source_relative_path(path))
        .collect::<Vec<_>>();
    let manifests = collect_manifest_packages(&options.project_root, &resolved_paths);

    let mut import_records = Vec::new();
    let mut unresolved = Vec::new();
    let mut warnings = Vec::new();
    let mut scanned_paths = Vec::new();

    for path in &resolved_paths {
        let display_path = path.to_string_lossy().into_owned();
        let language = language_for_path(path);
        scanned_paths.push(display_path.clone());
        let Ok(source) = std::fs::read_to_string(path) else {
            unresolved.push(json!({
                "kind": "source_unreadable",
                "path": display_path,
                "message": "source file could not be read",
            }));
            continue;
        };
        let Some(language) = language else {
            continue;
        };
        for import in extract_imports(&display_path, language, &source) {
            let resolved = resolve_import(&import, &options, &manifests);
            if let Some(finding) = resolved.unresolved.clone() {
                unresolved.push(finding);
            }
            warnings.extend(resolved.warnings.iter().cloned());

            import_records.push(json!({
                "path": import.path,
                "language": import.language,
                "ecosystem": import.ecosystem,
                "raw": import.raw,
                "package": import.package,
                "symbol": import.symbol,
                "status": resolved.status,
                "evidence": resolved.evidence,
            }));
        }
    }

    let status = if !unresolved.is_empty() {
        "fail"
    } else if !warnings.is_empty() {
        "warn"
    } else {
        "pass"
    };
    let result = json!({
        "ok": unresolved.is_empty(),
        "status": status,
        "checked": import_records.len(),
        "paths": scanned_paths,
        "imports": import_records,
        "unresolved": unresolved,
        "warnings": warnings,
        "evidence": {
            "project_root": options.project_root.to_string_lossy(),
            "manifest_paths": manifests.paths.into_iter().collect::<Vec<_>>(),
            "registry_entries": options.registry.len(),
            "installed_packages": options.installed.values().map(BTreeSet::len).sum::<usize>(),
        },
    });
    Ok(json_to_vm_value(&result))
}

#[derive(Debug, Clone)]
struct ResolvedImport {
    status: String,
    evidence: Vec<Value>,
    unresolved: Option<Value>,
    warnings: Vec<Value>,
}

fn parse_paths(value: Option<&VmValue>) -> Result<Vec<String>, VmError> {
    match value {
        Some(VmValue::List(items)) => Ok(items.iter().map(VmValue::display).collect()),
        Some(VmValue::String(path)) if !path.is_empty() => Ok(vec![path.to_string()]),
        Some(other) => Err(vm_error(format!(
            "__verify_imports: paths must be a string or list, got {}",
            other.type_name()
        ))),
        None => Err(vm_error("__verify_imports: paths are required")),
    }
}

fn parse_verify_options(value: Option<&VmValue>) -> Result<VerifyOptions, VmError> {
    let mut opts = VerifyOptions::default();
    let Some(value) = value else {
        return Ok(opts);
    };
    let Value::Object(map) = vm_value_to_json(value) else {
        return Ok(opts);
    };
    if let Some(project_root) = map.get("project_root").and_then(Value::as_str) {
        opts.project_root = resolve_source_relative_path(project_root);
    } else if let Some(project_root) = map.get("root").and_then(Value::as_str) {
        opts.project_root = resolve_source_relative_path(project_root);
    }
    if let Some(threshold) = map.get("low_trust_threshold").and_then(Value::as_f64) {
        opts.low_trust_threshold = threshold;
    }
    if let Some(days) = map.get("min_package_age_days").and_then(Value::as_i64) {
        opts.min_package_age_days = days;
    }
    if let Some(now) = map.get("now").and_then(Value::as_str).and_then(parse_date) {
        opts.now = Some(now);
    }
    opts.registry = parse_registry_entries(map.get("registry"));
    opts.installed = parse_installed_packages(map.get("installed_packages"));
    Ok(opts)
}

fn parse_registry_entries(value: Option<&Value>) -> Vec<RegistryEntry> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items.iter().filter_map(parse_registry_entry).collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(name, entry)| {
                let mut parsed = parse_registry_entry(entry)?;
                if parsed.name.is_empty() {
                    parsed.name = name.clone();
                }
                Some(parsed)
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_registry_entry(value: &Value) -> Option<RegistryEntry> {
    let dict = value.as_object()?;
    let name = string_field(dict, &["name", "package", "module"])?;
    let ecosystem = string_field(dict, &["ecosystem", "registry", "language"])
        .map(|value| normalize_ecosystem(&value))
        .unwrap_or_else(|| "unknown".to_string());
    let mut aliases = BTreeSet::new();
    aliases.insert(normalize_package_name(&ecosystem, &name));
    for key in ["alias", "aliases", "import_name", "import_names"] {
        match dict.get(key) {
            Some(Value::String(alias)) => {
                aliases.insert(normalize_package_name(&ecosystem, alias));
            }
            Some(Value::Array(items)) => {
                for item in items.iter().filter_map(Value::as_str) {
                    aliases.insert(normalize_package_name(&ecosystem, item));
                }
            }
            _ => {}
        }
    }
    let symbols = dict.get("symbols").and_then(|value| match value {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>(),
        ),
        _ => None,
    });
    Some(RegistryEntry {
        ecosystem,
        name,
        aliases,
        exists: dict.get("exists").and_then(Value::as_bool).unwrap_or(true),
        symbols,
        source_url: string_field(dict, &["source_url", "registry_url", "url"]),
        docs_url: string_field(dict, &["docs_url", "documentation_url"]),
        version: string_field(dict, &["version", "latest_version"]),
        trust_score: dict.get("trust_score").and_then(Value::as_f64),
        first_seen: string_field(dict, &["first_seen", "created_at"]),
        published_at: string_field(dict, &["published_at", "release_date"]),
    })
}

fn parse_installed_packages(value: Option<&Value>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    let Some(value) = value else {
        return out;
    };
    match value {
        Value::Array(items) => {
            for item in items {
                add_installed_value(&mut out, "*", item);
            }
        }
        Value::Object(map) => {
            for (ecosystem, entries) in map {
                add_installed_value(&mut out, &normalize_ecosystem(ecosystem), entries);
            }
        }
        _ => {}
    }
    out
}

fn add_installed_value(
    out: &mut BTreeMap<String, BTreeSet<String>>,
    ecosystem: &str,
    value: &Value,
) {
    match value {
        Value::String(name) => {
            let (ecosystem, name) = split_ecosystem_name(ecosystem, name);
            out.entry(ecosystem.clone())
                .or_default()
                .insert(normalize_package_name(&ecosystem, &name));
        }
        Value::Array(items) => {
            for item in items {
                add_installed_value(out, ecosystem, item);
            }
        }
        Value::Object(dict) => {
            if let Some(name) = string_field(dict, &["name", "package", "module"]) {
                let ecosystem = string_field(dict, &["ecosystem", "registry", "language"])
                    .map(|value| normalize_ecosystem(&value))
                    .unwrap_or_else(|| ecosystem.to_string());
                out.entry(ecosystem.clone())
                    .or_default()
                    .insert(normalize_package_name(&ecosystem, &name));
            }
        }
        _ => {}
    }
}

fn split_ecosystem_name(default_ecosystem: &str, value: &str) -> (String, String) {
    if let Some((ecosystem, name)) = value.split_once(':') {
        (normalize_ecosystem(ecosystem), name.to_string())
    } else {
        (default_ecosystem.to_string(), value.to_string())
    }
}

fn string_field(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn collect_manifest_packages(root: &Path, paths: &[PathBuf]) -> ManifestPackages {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut dirs = BTreeSet::new();
    dirs.insert(root.clone());
    for path in paths {
        let mut current = path.parent();
        while let Some(dir) = current {
            dirs.insert(dir.to_path_buf());
            if dir == root {
                break;
            }
            current = dir.parent();
        }
    }

    let mut out = ManifestPackages::default();
    for dir in dirs {
        collect_package_json(&dir, &mut out);
        collect_pyproject(&dir, &mut out);
        collect_requirements(&dir, &mut out);
        collect_cargo_toml(&dir, &mut out);
    }
    out
}

fn collect_package_json(dir: &Path, out: &mut ManifestPackages) {
    let path = dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(map) = parsed.as_object() else {
        return;
    };
    out.paths.insert(path.to_string_lossy().into_owned());
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(deps) = map.get(key).and_then(Value::as_object) else {
            continue;
        };
        for name in deps.keys() {
            add_manifest_package(out, "npm", name);
        }
    }
}

fn collect_pyproject(dir: &Path, out: &mut ManifestPackages) {
    let path = dir.join("pyproject.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return;
    };
    out.paths.insert(path.to_string_lossy().into_owned());
    if let Some(project) = parsed.get("project") {
        collect_python_dependency_array(project.get("dependencies"), out);
        if let Some(optional) = project
            .get("optional-dependencies")
            .and_then(toml::Value::as_table)
        {
            for deps in optional.values() {
                collect_python_dependency_array(Some(deps), out);
            }
        }
    }
    if let Some(poetry) = parsed.get("tool").and_then(|tool| tool.get("poetry")) {
        collect_poetry_dependency_table(poetry.get("dependencies"), out);
        if let Some(groups) = poetry.get("group").and_then(toml::Value::as_table) {
            for group in groups.values() {
                collect_poetry_dependency_table(group.get("dependencies"), out);
            }
        }
    }
}

fn collect_requirements(dir: &Path, out: &mut ManifestPackages) {
    for name in [
        "requirements.txt",
        "requirements-dev.txt",
        "requirements-test.txt",
    ] {
        let path = dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.paths.insert(path.to_string_lossy().into_owned());
        for line in text.lines() {
            if let Some(package) = parse_python_requirement_name(line) {
                add_manifest_package(out, "python", &package);
            }
        }
    }
}

fn collect_cargo_toml(dir: &Path, out: &mut ManifestPackages) {
    let path = dir.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return;
    };
    out.paths.insert(path.to_string_lossy().into_owned());
    for section in [
        &["dependencies"][..],
        &["dev-dependencies"][..],
        &["build-dependencies"][..],
        &["workspace", "dependencies"][..],
    ] {
        let Some(table) = lookup_toml_path(&parsed, section).and_then(toml::Value::as_table) else {
            continue;
        };
        for name in table.keys() {
            add_manifest_package(out, "cargo", name);
        }
    }
}

fn collect_python_dependency_array(value: Option<&toml::Value>, out: &mut ManifestPackages) {
    let Some(items) = value.and_then(toml::Value::as_array) else {
        return;
    };
    for item in items.iter().filter_map(toml::Value::as_str) {
        if let Some(name) = parse_python_requirement_name(item) {
            add_manifest_package(out, "python", &name);
        }
    }
}

fn collect_poetry_dependency_table(value: Option<&toml::Value>, out: &mut ManifestPackages) {
    let Some(table) = value.and_then(toml::Value::as_table) else {
        return;
    };
    for name in table.keys().filter(|name| name.as_str() != "python") {
        add_manifest_package(out, "python", name);
    }
}

fn lookup_toml_path<'a>(value: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn add_manifest_package(out: &mut ManifestPackages, ecosystem: &str, name: &str) {
    out.packages
        .entry(ecosystem.to_string())
        .or_default()
        .insert(normalize_package_name(ecosystem, name));
}

fn parse_python_requirement_name(raw: &str) -> Option<String> {
    let clean = raw.split('#').next()?.trim();
    if clean.is_empty() || clean.starts_with('-') {
        return None;
    }
    let name = clean
        .split(['<', '>', '=', '!', '~', ';', '[', ' '])
        .next()
        .unwrap_or_default()
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn language_for_path(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("py") => Some("python"),
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") | Some("mjs") | Some("cjs") => {
            Some("typescript")
        }
        Some("rs") => Some("rust"),
        Some("harn") => Some("harn"),
        _ => None,
    }
}

fn extract_imports(path: &str, language: &str, source: &str) -> Vec<ImportRef> {
    match language {
        "python" => extract_python_imports(path, source),
        "typescript" => extract_node_imports(path, source),
        "rust" => extract_rust_imports(path, source),
        "harn" => extract_harn_imports(path, source),
        _ => Vec::new(),
    }
}

fn extract_python_imports(path: &str, source: &str) -> Vec<ImportRef> {
    let import_re = Regex::new(
        r"(?m)^\s*import\s+([A-Za-z_][A-Za-z0-9_\.]*(?:\s+as\s+[A-Za-z_][A-Za-z0-9_]*)?(?:\s*,\s*[A-Za-z_][A-Za-z0-9_\.]*(?:\s+as\s+[A-Za-z_][A-Za-z0-9_]*)?)*)",
    )
    .expect("static regex parses");
    let from_re = Regex::new(r"(?m)^\s*from\s+([A-Za-z_][A-Za-z0-9_\.]*)\s+import\s+([^\n#]+)")
        .expect("static regex parses");
    let mut out = Vec::new();
    for capture in import_re.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        for item in capture[1].split(',') {
            let module = item.split_whitespace().next().unwrap_or_default().trim();
            push_python_import(path, &raw, module, None, &mut out);
        }
    }
    for capture in from_re.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        let module = capture[1].trim();
        for symbol in capture[2].split(',') {
            let symbol = symbol.split_whitespace().next().unwrap_or_default().trim();
            let symbol = (!symbol.is_empty() && symbol != "*").then(|| symbol.to_string());
            push_python_import(path, &raw, module, symbol, &mut out);
        }
    }
    dedupe_imports(out)
}

fn push_python_import(
    path: &str,
    raw: &str,
    module: &str,
    symbol: Option<String>,
    out: &mut Vec<ImportRef>,
) {
    if module.starts_with('.') || module.is_empty() {
        return;
    }
    let package = module.split('.').next().unwrap_or(module).to_string();
    out.push(ImportRef {
        path: path.to_string(),
        language: "python".to_string(),
        ecosystem: "python".to_string(),
        raw: raw.to_string(),
        standard: PYTHON_STDLIB.contains(&package.as_str()),
        local: false,
        package,
        symbol,
    });
}

fn extract_node_imports(path: &str, source: &str) -> Vec<ImportRef> {
    let import_from = Regex::new(r#"(?m)\bimport\s+([^;\n]*?)\s+from\s*["']([^"']+)["']"#)
        .expect("static regex parses");
    let side_effect =
        Regex::new(r#"(?m)\bimport\s*["']([^"']+)["']"#).expect("static regex parses");
    let require =
        Regex::new(r#"\brequire\s*\(\s*["']([^"']+)["']\s*\)"#).expect("static regex parses");
    let dynamic =
        Regex::new(r#"\bimport\s*\(\s*["']([^"']+)["']\s*\)"#).expect("static regex parses");
    let mut out = Vec::new();
    for capture in import_from.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        let module = capture[2].trim();
        for symbol in node_import_symbols(capture[1].trim()) {
            push_node_import(path, &raw, module, symbol, &mut out);
        }
    }
    for capture in side_effect.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        push_node_import(path, &raw, capture[1].trim(), None, &mut out);
    }
    for capture in require.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        push_node_import(path, &raw, capture[1].trim(), None, &mut out);
    }
    for capture in dynamic.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        push_node_import(path, &raw, capture[1].trim(), None, &mut out);
    }
    dedupe_imports(out)
}

fn node_import_symbols(prefix: &str) -> Vec<Option<String>> {
    if let Some(start) = prefix.find('{') {
        if let Some(end) = prefix[start + 1..].find('}') {
            let body = &prefix[start + 1..start + 1 + end];
            let symbols = body
                .split(',')
                .filter_map(|part| {
                    let name = part.split_whitespace().next().unwrap_or_default().trim();
                    (!name.is_empty()).then(|| Some(name.to_string()))
                })
                .collect::<Vec<_>>();
            if !symbols.is_empty() {
                return symbols;
            }
        }
    }
    if prefix.trim().is_empty() {
        vec![None]
    } else {
        vec![Some("default".to_string())]
    }
}

fn push_node_import(
    path: &str,
    raw: &str,
    module: &str,
    symbol: Option<String>,
    out: &mut Vec<ImportRef>,
) {
    let local = module.starts_with('.') || module.starts_with('/');
    let package = node_package_name(module);
    let standard = NODE_BUILTINS.contains(&module);
    out.push(ImportRef {
        path: path.to_string(),
        language: "typescript".to_string(),
        ecosystem: "npm".to_string(),
        raw: raw.to_string(),
        package,
        symbol,
        local,
        standard,
    });
}

fn node_package_name(module: &str) -> String {
    if module.starts_with('@') {
        let mut parts = module.split('/');
        let scope = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if name.is_empty() {
            module.to_string()
        } else {
            format!("{scope}/{name}")
        }
    } else {
        module.split('/').next().unwrap_or(module).to_string()
    }
}

fn extract_rust_imports(path: &str, source: &str) -> Vec<ImportRef> {
    let use_re = Regex::new(r"(?m)^\s*(?:pub\s+)?use\s+([^;]+);").expect("static regex parses");
    let extern_re = Regex::new(r"(?m)^\s*extern\s+crate\s+([A-Za-z_][A-Za-z0-9_]*)")
        .expect("static regex parses");
    let mut out = Vec::new();
    for capture in use_re.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        let import_path = capture[1].trim().trim_start_matches("::");
        let package = import_path.split("::").next().unwrap_or_default();
        if package.is_empty() {
            continue;
        }
        for symbol in rust_symbols(import_path) {
            push_rust_import(path, &raw, package, symbol, &mut out);
        }
    }
    for capture in extern_re.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        push_rust_import(path, &raw, capture[1].trim(), None, &mut out);
    }
    dedupe_imports(out)
}

fn rust_symbols(import_path: &str) -> Vec<Option<String>> {
    if let Some(start) = import_path.find('{') {
        if let Some(end) = import_path[start + 1..].find('}') {
            let body = &import_path[start + 1..start + 1 + end];
            let symbols = body
                .split(',')
                .filter_map(|part| {
                    let name = part.trim();
                    (!name.is_empty() && name != "*").then(|| Some(name.to_string()))
                })
                .collect::<Vec<_>>();
            if !symbols.is_empty() {
                return symbols;
            }
        }
    }
    import_path
        .rsplit("::")
        .next()
        .filter(|symbol| *symbol != "*" && *symbol != import_path.split("::").next().unwrap_or(""))
        .map(|symbol| vec![Some(symbol.to_string())])
        .unwrap_or_else(|| vec![None])
}

fn push_rust_import(
    path: &str,
    raw: &str,
    package: &str,
    symbol: Option<String>,
    out: &mut Vec<ImportRef>,
) {
    out.push(ImportRef {
        path: path.to_string(),
        language: "rust".to_string(),
        ecosystem: "cargo".to_string(),
        raw: raw.to_string(),
        package: package.to_string(),
        symbol,
        local: false,
        standard: RUST_STDLIB.contains(&package),
    });
}

fn extract_harn_imports(path: &str, source: &str) -> Vec<ImportRef> {
    let import_re =
        Regex::new(r#"(?m)^\s*(?:pub\s+)?import\s+(?:\{[^}]*\}\s+from\s+)?["']([^"']+)["']"#)
            .expect("static regex parses");
    let mut out = Vec::new();
    for capture in import_re.captures_iter(source) {
        let raw = capture.get(0).unwrap().as_str().trim().to_string();
        let module = capture[1].trim();
        let local = module.starts_with('.') || module.starts_with('/');
        let package = module.split('/').next().unwrap_or(module).to_string();
        out.push(ImportRef {
            path: path.to_string(),
            language: "harn".to_string(),
            ecosystem: "harn".to_string(),
            raw,
            package: package.clone(),
            symbol: None,
            local,
            standard: package == "std",
        });
    }
    dedupe_imports(out)
}

fn dedupe_imports(imports: Vec<ImportRef>) -> Vec<ImportRef> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for import in imports {
        let key = format!(
            "{}\0{}\0{}\0{}",
            import.path,
            import.package,
            import.symbol.clone().unwrap_or_default(),
            import.raw
        );
        if seen.insert(key) {
            out.push(import);
        }
    }
    out
}

fn resolve_import(
    import: &ImportRef,
    options: &VerifyOptions,
    manifests: &ManifestPackages,
) -> ResolvedImport {
    let mut evidence = Vec::new();
    let mut warnings = Vec::new();
    if import.local {
        evidence.push(json!({"kind": "local_path"}));
        return ResolvedImport {
            status: "resolved".to_string(),
            evidence,
            unresolved: None,
            warnings,
        };
    }
    if import.standard {
        evidence.push(json!({"kind": "standard_library"}));
        return ResolvedImport {
            status: "resolved".to_string(),
            evidence,
            unresolved: None,
            warnings,
        };
    }

    let normalized = normalize_package_name(&import.ecosystem, &import.package);
    let registry_entry = find_registry_entry(options, &import.ecosystem, &normalized);
    let mut package_resolved = false;

    if package_set_contains(&options.installed, &import.ecosystem, &normalized) {
        evidence.push(json!({"kind": "installed_environment"}));
        package_resolved = true;
    }
    if package_set_contains(&manifests.packages, &import.ecosystem, &normalized) {
        evidence.push(json!({"kind": "manifest"}));
        package_resolved = true;
    }
    if let Some(entry) = registry_entry {
        if entry.exists {
            evidence.push(registry_evidence(entry));
            package_resolved = true;
            warnings.extend(low_trust_warnings(import, entry, options));
        } else {
            evidence.push(json!({"kind": "registry", "exists": false, "name": entry.name}));
        }
    }

    if !package_resolved {
        let finding = json!({
            "kind": "package_not_found",
            "path": import.path,
            "language": import.language,
            "ecosystem": import.ecosystem,
            "package": import.package,
            "symbol": import.symbol,
            "raw": import.raw,
            "evidence": evidence,
        });
        return ResolvedImport {
            status: "not_found".to_string(),
            evidence,
            unresolved: Some(finding),
            warnings,
        };
    }

    if let Some(symbol) = import.symbol.as_deref() {
        if let Some(entry) = registry_entry {
            if let Some(symbols) = &entry.symbols {
                if symbol != "default" && !symbols.contains(symbol) {
                    let finding = json!({
                        "kind": "symbol_not_found",
                        "path": import.path,
                        "language": import.language,
                        "ecosystem": import.ecosystem,
                        "package": import.package,
                        "symbol": symbol,
                        "raw": import.raw,
                        "evidence": evidence,
                    });
                    return ResolvedImport {
                        status: "unresolved_symbol".to_string(),
                        evidence,
                        unresolved: Some(finding),
                        warnings,
                    };
                }
                evidence.push(json!({"kind": "symbol_registry", "symbol": symbol}));
            } else {
                warnings.push(json!({
                    "kind": "symbol_unverified",
                    "path": import.path,
                    "ecosystem": import.ecosystem,
                    "package": import.package,
                    "symbol": symbol,
                    "raw": import.raw,
                    "message": "package resolved but registry evidence did not include symbol metadata",
                }));
            }
        }
    }

    ResolvedImport {
        status: if warnings.is_empty() {
            "resolved".to_string()
        } else {
            "warning".to_string()
        },
        evidence,
        unresolved: None,
        warnings,
    }
}

fn find_registry_entry<'a>(
    options: &'a VerifyOptions,
    ecosystem: &str,
    normalized_name: &str,
) -> Option<&'a RegistryEntry> {
    options.registry.iter().find(|entry| {
        (entry.ecosystem == ecosystem || entry.ecosystem == "unknown")
            && entry.aliases.contains(normalized_name)
    })
}

fn registry_evidence(entry: &RegistryEntry) -> Value {
    let mut value = Map::new();
    value.insert("kind".to_string(), json!("registry"));
    value.insert("exists".to_string(), json!(entry.exists));
    value.insert("name".to_string(), json!(entry.name));
    value.insert("ecosystem".to_string(), json!(entry.ecosystem));
    if let Some(source_url) = &entry.source_url {
        value.insert("source_url".to_string(), json!(source_url));
    }
    if let Some(docs_url) = &entry.docs_url {
        value.insert("docs_url".to_string(), json!(docs_url));
    }
    if let Some(version) = &entry.version {
        value.insert("version".to_string(), json!(version));
    }
    if let Some(trust_score) = entry.trust_score {
        value.insert("trust_score".to_string(), json!(trust_score));
    }
    if let Some(first_seen) = &entry.first_seen {
        value.insert("first_seen".to_string(), json!(first_seen));
    }
    Value::Object(value)
}

fn low_trust_warnings(
    import: &ImportRef,
    entry: &RegistryEntry,
    options: &VerifyOptions,
) -> Vec<Value> {
    let mut warnings = Vec::new();
    if let Some(score) = entry.trust_score {
        if score < options.low_trust_threshold {
            warnings.push(json!({
                "kind": "low_trust_package",
                "path": import.path,
                "ecosystem": import.ecosystem,
                "package": import.package,
                "trust_score": score,
                "threshold": options.low_trust_threshold,
                "source_url": entry.source_url,
            }));
        }
    }
    if options.min_package_age_days > 0 {
        if let (Some(now), Some(first_seen)) = (
            options.now,
            entry
                .first_seen
                .as_deref()
                .or(entry.published_at.as_deref())
                .and_then(parse_date),
        ) {
            let age_days = now.signed_duration_since(first_seen).num_days();
            if age_days >= 0 && age_days < options.min_package_age_days {
                warnings.push(json!({
                    "kind": "fresh_package",
                    "path": import.path,
                    "ecosystem": import.ecosystem,
                    "package": import.package,
                    "age_days": age_days,
                    "min_age_days": options.min_package_age_days,
                    "first_seen": first_seen.to_string(),
                    "source_url": entry.source_url,
                }));
            }
        }
    }
    warnings
}

fn package_set_contains(
    packages: &BTreeMap<String, BTreeSet<String>>,
    ecosystem: &str,
    normalized_name: &str,
) -> bool {
    packages
        .get(ecosystem)
        .is_some_and(|items| items.contains(normalized_name))
        || packages
            .get("*")
            .is_some_and(|items| items.contains(normalized_name))
}

fn normalize_ecosystem(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "node" | "javascript" | "typescript" | "js" | "ts" => "npm".to_string(),
        "rust" | "crates.io" => "cargo".to_string(),
        "py" | "pypi" => "python".to_string(),
        other => other.to_string(),
    }
}

fn normalize_package_name(ecosystem: &str, name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match ecosystem {
        "cargo" | "python" => lower.replace('_', "-"),
        _ => lower,
    }
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()
}

fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Arc::from(message.into())))
}

const PYTHON_STDLIB: &[&str] = &[
    "abc",
    "argparse",
    "asyncio",
    "collections",
    "contextlib",
    "csv",
    "dataclasses",
    "datetime",
    "decimal",
    "enum",
    "functools",
    "glob",
    "hashlib",
    "importlib",
    "inspect",
    "io",
    "itertools",
    "json",
    "logging",
    "math",
    "os",
    "pathlib",
    "queue",
    "random",
    "re",
    "shutil",
    "signal",
    "sqlite3",
    "statistics",
    "string",
    "subprocess",
    "sys",
    "tempfile",
    "textwrap",
    "threading",
    "time",
    "typing",
    "unittest",
    "urllib",
    "uuid",
    "xml",
];

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "buffer",
    "child_process",
    "crypto",
    "events",
    "fs",
    "fs/promises",
    "http",
    "https",
    "net",
    "node:assert",
    "node:buffer",
    "node:child_process",
    "node:crypto",
    "node:events",
    "node:fs",
    "node:fs/promises",
    "node:http",
    "node:https",
    "node:net",
    "node:os",
    "node:path",
    "node:process",
    "node:stream",
    "node:url",
    "node:util",
    "os",
    "path",
    "process",
    "stream",
    "url",
    "util",
];

const RUST_STDLIB: &[&str] = &["alloc", "core", "crate", "self", "std", "super"];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn verify_imports_flags_missing_symbol_and_low_trust_packages() {
        let dir = tempdir().unwrap();
        let py = dir.path().join("app.py");
        std::fs::write(
            &py,
            "import requests\nfrom fastapi import FastAPI, NopeSymbol\nimport madeup_package_zz\n",
        )
        .unwrap();
        let ts = dir.path().join("ui.ts");
        std::fs::write(
            &ts,
            "import React from 'react'\nimport { createRoot } from 'react-dom/client'\nimport nope from 'fresh-slop'\n",
        )
        .unwrap();
        let rs = dir.path().join("lib.rs");
        std::fs::write(
            &rs,
            "use serde::Serialize;\nuse hallucinate_crate::Thing;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"19.0.0","react-dom":"19.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();

        let args = vec![
            json_to_vm_value(&json!([
                py.to_string_lossy(),
                ts.to_string_lossy(),
                rs.to_string_lossy()
            ])),
            json_to_vm_value(&json!({
                "project_root": dir.path().to_string_lossy(),
                "now": "2026-05-31",
                "registry": [
                    {"ecosystem":"python","name":"requests","symbols":[],"source_url":"https://pypi.org/project/requests/"},
                    {"ecosystem":"python","name":"fastapi","symbols":["FastAPI"],"source_url":"https://fastapi.tiangolo.com/"},
                    {"ecosystem":"npm","name":"react-dom","symbols":["createRoot"],"source_url":"https://www.npmjs.com/package/react-dom"},
                    {"ecosystem":"cargo","name":"serde","symbols":["Serialize"],"source_url":"https://docs.rs/serde/"},
                    {"ecosystem":"npm","name":"fresh-slop","exists":true,"trust_score":0.1,"first_seen":"2026-05-30","source_url":"https://www.npmjs.com/package/fresh-slop"}
                ]
            })),
        ];
        let result = verify_imports_impl(&args, &mut String::new()).unwrap();
        let json = vm_value_to_json(&result);
        assert_eq!(json["status"], "fail");
        assert!(json["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["package"] == "madeup_package_zz"));
        assert!(json["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["package"] == "hallucinate_crate"));
        assert!(json["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "symbol_not_found"
                && item["package"] == "fastapi"
                && item["symbol"] == "NopeSymbol"));
        assert!(json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "fresh_package"));
        assert!(json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "low_trust_package"));
    }
}
