//! Agent Plugins 1.0 package loading and projection.
//!
//! This module owns the portable package boundary. Hosts consume the typed
//! report and never need to reinterpret `plugin.json`, `mcp.json`, discovery,
//! placeholder expansion, or component failure isolation.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use url::Url;

pub const SPEC_VERSION: &str = "1.0.0";
pub const MANIFEST_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
pub const MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
pub const HARN_EXTENSION: &str = "org.harnlang";

const MANIFEST_FIELDS: &[&str] = &[
    "$schema",
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "extensions",
];
const RESERVED_ENV: &[&str] = &["PLUGIN_ROOT", "PLUGIN_DATA"];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginAuthor {
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<PluginAuthor>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticScope {
    Plugin,
    Skills,
    Mcp,
    McpServer,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginDiagnostic {
    pub code: &'static str,
    pub severity: DiagnosticSeverity,
    pub scope: DiagnosticScope,
    pub component: Option<String>,
    pub fatal: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PluginMcpServer {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl PluginMcpServer {
    /// Project an accepted portable server into Harn's runtime contract.
    pub fn to_harn_spec(&self, name: impl Into<String>) -> crate::mcp::McpServerSpec {
        let name = name.into();
        match self {
            Self::Stdio {
                command,
                args,
                env,
                cwd,
            } => crate::mcp::McpServerSpec::stdio(
                name,
                command.clone(),
                args.clone(),
                env.clone(),
                Some(cwd.to_string_lossy().into_owned()),
            ),
            Self::StreamableHttp { url, headers } => {
                crate::mcp::McpServerSpec::http(name, url.clone(), headers.clone())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentPlugin {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub manifest: PluginManifest,
    pub skills: Vec<PluginSkill>,
    pub mcp_servers: BTreeMap<String, PluginMcpServer>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentPluginLoadReport {
    /// False only when the root manifest is unusable and the whole package is rejected.
    pub accepted: bool,
    /// False when any normative violation was observed, including an isolated component error.
    pub conformant: bool,
    pub plugin: Option<AgentPlugin>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl AgentPluginLoadReport {
    /// Project accepted servers without mutating the filesystem.
    ///
    /// Use [`AgentPlugin::prepare_runtime_specs`] before launching stdio
    /// servers so the Agent Plugins `PLUGIN_DATA` lifecycle invariant holds.
    pub fn runtime_specs(&self) -> BTreeMap<String, crate::mcp::McpServerSpec> {
        self.plugin
            .as_ref()
            .map(|plugin| {
                plugin
                    .mcp_servers
                    .iter()
                    .map(|(name, server)| (name.clone(), server.to_harn_spec(name)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl AgentPlugin {
    /// Create the persistent plugin data root, then return Harn MCP specs.
    pub fn prepare_runtime_specs(
        &self,
    ) -> Result<BTreeMap<String, crate::mcp::McpServerSpec>, PluginPrepareError> {
        fs::create_dir_all(&self.data_dir).map_err(|source| PluginPrepareError {
            path: self.data_dir.clone(),
            source,
        })?;
        Ok(self
            .mcp_servers
            .iter()
            .map(|(name, server)| (name.clone(), server.to_harn_spec(name)))
            .collect())
    }
}

#[derive(Debug)]
pub struct PluginPrepareError {
    pub path: PathBuf,
    pub source: std::io::Error,
}

impl std::fmt::Display for PluginPrepareError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cannot create plugin data directory {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for PluginPrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Load one Agent Plugins 1.0 package without launching code or creating data.
///
/// `data_dir` is the host-owned, persistent writable directory reserved for
/// this installed plugin. Launchers must create it before starting a server.
pub fn load_agent_plugin(
    root: impl AsRef<Path>,
    data_dir: impl AsRef<Path>,
) -> AgentPluginLoadReport {
    let mut diagnostics = Vec::new();
    let root = match fs::canonicalize(root.as_ref()) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            return fatal_report(
                "AP_PLUGIN_ROOT",
                format!("plugin root is not a directory: {}", path.display()),
            )
        }
        Err(error) => {
            return fatal_report(
                "AP_PLUGIN_ROOT",
                format!("cannot open plugin root: {error}"),
            )
        }
    };
    let data_dir =
        resolve_allow_missing(data_dir.as_ref()).unwrap_or_else(|| data_dir.as_ref().to_path_buf());
    let manifest_path = root.join("plugin.json");
    if !is_contained_file(&root, &manifest_path) {
        return fatal_report(
            "AP_MANIFEST_MISSING",
            format!(
                "required manifest is missing or resolves outside the plugin root: {}",
                manifest_path.display()
            ),
        );
    }
    let value = match read_json(&manifest_path) {
        Ok(value) => value,
        Err(message) => return fatal_report("AP_MANIFEST_JSON", message),
    };
    let Some(object) = value.as_object() else {
        return fatal_report(
            "AP_MANIFEST_SHAPE",
            "plugin.json must contain a JSON object".into(),
        );
    };
    let Some(manifest) = validate_manifest(object, &mut diagnostics) else {
        return AgentPluginLoadReport {
            accepted: false,
            conformant: false,
            plugin: None,
            diagnostics,
        };
    };
    let skills = load_skills(&root, &mut diagnostics);
    let mcp_servers = load_mcp(&root, &data_dir, &mut diagnostics);
    let conformant = diagnostics.is_empty();
    AgentPluginLoadReport {
        accepted: true,
        conformant,
        plugin: Some(AgentPlugin {
            root,
            data_dir,
            manifest,
            skills,
            mcp_servers,
        }),
        diagnostics,
    }
}

fn validate_manifest(
    object: &Map<String, Value>,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<PluginManifest> {
    for key in object
        .keys()
        .filter(|key| !MANIFEST_FIELDS.contains(&key.as_str()))
    {
        diagnostic(
            diagnostics,
            "AP_MANIFEST_UNKNOWN_FIELD",
            DiagnosticSeverity::Warning,
            DiagnosticScope::Plugin,
            None,
            false,
            format!("unknown plugin.json field `{key}` is ignored"),
        );
    }
    if object.get("$schema").and_then(Value::as_str) != Some(MANIFEST_SCHEMA) {
        diagnostic(
            diagnostics,
            "AP_MANIFEST_SCHEMA",
            DiagnosticSeverity::Error,
            DiagnosticScope::Plugin,
            None,
            true,
            format!("`$schema` must be `{MANIFEST_SCHEMA}`"),
        );
    }
    let name = object.get("name").and_then(Value::as_str);
    if !name.is_some_and(valid_plugin_name) {
        diagnostic(diagnostics, "AP_MANIFEST_NAME", DiagnosticSeverity::Error, DiagnosticScope::Plugin, None, true, "`name` must be 1-64 lowercase letters, digits, dots, or hyphens, with alphanumeric endpoints and no repeated dot or hyphen".into());
    }
    validate_optional_string_fields(
        object,
        &[
            "version",
            "description",
            "homepage",
            "repository",
            "license",
        ],
        diagnostics,
    );
    validate_author(object.get("author"), diagnostics);
    if let Some(value) = object.get("keywords") {
        if !value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
        {
            diagnostic(
                diagnostics,
                "AP_MANIFEST_KEYWORDS",
                DiagnosticSeverity::Error,
                DiagnosticScope::Plugin,
                None,
                true,
                "`keywords` must be an array of strings".into(),
            );
        }
    }
    validate_extensions(object.get("extensions"), diagnostics);
    if diagnostics.iter().any(|item| item.fatal) {
        return None;
    }
    let mut normalized = object.clone();
    if !normalized.get("extensions").is_some_and(Value::is_object) {
        normalized.remove("extensions");
    }
    serde_json::from_value(Value::Object(normalized)).ok()
}

fn validate_optional_string_fields(
    object: &Map<String, Value>,
    fields: &[&str],
    diagnostics: &mut Vec<PluginDiagnostic>,
) {
    for field in fields {
        if object.get(*field).is_some_and(|value| !value.is_string()) {
            diagnostic(
                diagnostics,
                "AP_MANIFEST_FIELD_TYPE",
                DiagnosticSeverity::Error,
                DiagnosticScope::Plugin,
                Some((*field).into()),
                true,
                format!("`{field}` must be a string"),
            );
        }
    }
}

fn validate_author(value: Option<&Value>, diagnostics: &mut Vec<PluginDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(author) = value.as_object() else {
        diagnostic(
            diagnostics,
            "AP_MANIFEST_AUTHOR",
            DiagnosticSeverity::Error,
            DiagnosticScope::Plugin,
            None,
            true,
            "`author` must be an object".into(),
        );
        return;
    };
    for (key, value) in author {
        if !["name", "email", "url"].contains(&key.as_str()) || !value.is_string() {
            diagnostic(
                diagnostics,
                "AP_MANIFEST_AUTHOR",
                DiagnosticSeverity::Error,
                DiagnosticScope::Plugin,
                Some(key.clone()),
                true,
                "`author` accepts only string fields `name`, `email`, and `url`".into(),
            );
        }
    }
}

fn validate_extensions(value: Option<&Value>, diagnostics: &mut Vec<PluginDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(_extensions) = value.as_object() else {
        diagnostic(
            diagnostics,
            "AP_EXTENSIONS_SHAPE",
            DiagnosticSeverity::Warning,
            DiagnosticScope::Plugin,
            None,
            false,
            "non-object `extensions` is ignored".into(),
        );
        return;
    };
    // Namespace payloads are client-owned. Core must not validate the
    // contents of namespaces it does not implement (§8.1).
}

fn load_skills(root: &Path, diagnostics: &mut Vec<PluginDiagnostic>) -> Vec<PluginSkill> {
    let skills_root = root.join("skills");
    if !path_present(&skills_root) {
        return Vec::new();
    }
    if !is_contained_dir(root, &skills_root) {
        diagnostic(
            diagnostics,
            "AP_SKILLS_NOT_DIRECTORY",
            DiagnosticSeverity::Error,
            DiagnosticScope::Skills,
            None,
            false,
            "`skills` exists but is not a directory; the skills component is disabled".into(),
        );
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&skills_root) else {
        diagnostic(
            diagnostics,
            "AP_SKILLS_READ",
            DiagnosticSeverity::Error,
            DiagnosticScope::Skills,
            None,
            false,
            "cannot enumerate `skills`".into(),
        );
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            if !is_contained_file(root, &path.join("SKILL.md")) {
                diagnostic(
                    diagnostics,
                    "AP_SKILL_CONTAINMENT",
                    DiagnosticSeverity::Error,
                    DiagnosticScope::Skills,
                    Some(name),
                    false,
                    "SKILL.md is missing or resolves outside the plugin root".into(),
                );
                return None;
            }
            match validate_agent_skill(&path) {
                Ok(skill) => Some(skill),
                Err(error) => {
                    diagnostic(
                        diagnostics,
                        "AP_SKILL_INVALID",
                        DiagnosticSeverity::Error,
                        DiagnosticScope::Skills,
                        Some(name),
                        false,
                        error,
                    );
                    None
                }
            }
        })
        .collect()
}

fn validate_agent_skill(path: &Path) -> Result<PluginSkill, String> {
    let skill_file = path.join("SKILL.md");
    let source = fs::read_to_string(&skill_file)
        .map_err(|error| format!("cannot read {}: {error}", skill_file.display()))?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let first_line = source
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r');
    if first_line != "---" {
        return Err("SKILL.md must begin with YAML frontmatter".into());
    }
    let (frontmatter, _) = crate::skills::split_frontmatter(source);
    if frontmatter.is_empty() {
        return Err("SKILL.md frontmatter is not closed".into());
    }
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(frontmatter)
        .map_err(|error| format!("invalid SKILL.md frontmatter: {error}"))?;
    let mapping = value
        .as_mapping()
        .ok_or("SKILL.md frontmatter must be a mapping")?;
    const FIELDS: &[&str] = &[
        "name",
        "description",
        "license",
        "compatibility",
        "metadata",
        "allowed-tools",
    ];
    for key in mapping.keys() {
        let key = key
            .as_str()
            .ok_or("SKILL.md frontmatter field names must be strings")?;
        if !FIELDS.contains(&key) {
            return Err(format!("unexpected SKILL.md frontmatter field `{key}`"));
        }
    }
    let name = yaml_string(mapping, "name")?;
    if !(1..=64).contains(&name.len())
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("skill `name` must be 1-64 lowercase letters, digits, or hyphens, with no leading, trailing, or repeated hyphen".into());
    }
    let directory_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if name != directory_name {
        return Err(format!(
            "skill name `{name}` must match directory `{directory_name}`"
        ));
    }
    let description = yaml_string(mapping, "description")?;
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("skill `description` must contain 1-1024 characters".into());
    }
    for field in ["license", "allowed-tools"] {
        if mapping
            .get(serde_yaml_ng::Value::String(field.into()))
            .is_some_and(|value| !value.is_string())
        {
            return Err(format!("skill `{field}` must be a string"));
        }
    }
    if let Some(compatibility) = mapping.get(serde_yaml_ng::Value::String("compatibility".into())) {
        let compatibility = compatibility
            .as_str()
            .ok_or("skill `compatibility` must be a string")?;
        if compatibility.is_empty() || compatibility.chars().count() > 500 {
            return Err("skill `compatibility` must contain 1-500 characters".into());
        }
    }
    if let Some(metadata) = mapping.get(serde_yaml_ng::Value::String("metadata".into())) {
        let metadata = metadata
            .as_mapping()
            .ok_or("skill `metadata` must be a string-to-string mapping")?;
        if !metadata
            .iter()
            .all(|(key, value)| key.is_string() && value.is_string())
        {
            return Err("skill `metadata` must be a string-to-string mapping".into());
        }
    }
    Ok(PluginSkill {
        name,
        description,
        path: path.to_path_buf(),
    })
}

fn yaml_string(mapping: &serde_yaml_ng::Mapping, field: &str) -> Result<String, String> {
    mapping
        .get(serde_yaml_ng::Value::String(field.into()))
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("skill `{field}` is required and must be a string"))
}

fn load_mcp(
    root: &Path,
    data_dir: &Path,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> BTreeMap<String, PluginMcpServer> {
    let path = root.join("mcp.json");
    if !path_present(&path) {
        return BTreeMap::new();
    }
    if !is_contained_file(root, &path) {
        diagnostic(
            diagnostics,
            "AP_MCP_NOT_FILE",
            DiagnosticSeverity::Error,
            DiagnosticScope::Mcp,
            None,
            false,
            "`mcp.json` is not a regular file; the MCP component is disabled".into(),
        );
        return BTreeMap::new();
    }
    let value = match read_json(&path) {
        Ok(value) => value,
        Err(message) => {
            diagnostic(
                diagnostics,
                "AP_MCP_JSON",
                DiagnosticSeverity::Error,
                DiagnosticScope::Mcp,
                None,
                false,
                message,
            );
            return BTreeMap::new();
        }
    };
    let Some(object) = value.as_object() else {
        diagnostic(
            diagnostics,
            "AP_MCP_SHAPE",
            DiagnosticSeverity::Error,
            DiagnosticScope::Mcp,
            None,
            false,
            "mcp.json must contain a JSON object".into(),
        );
        return BTreeMap::new();
    };
    if object.len() != 2
        || object.get("$schema").and_then(Value::as_str) != Some(MCP_SCHEMA)
        || !object.get("mcpServers").is_some_and(Value::is_object)
    {
        diagnostic(diagnostics, "AP_MCP_SCHEMA", DiagnosticSeverity::Error, DiagnosticScope::Mcp, None, false, format!("mcp.json must contain only `$schema: {MCP_SCHEMA}` and object `mcpServers`; the MCP component is disabled"));
        return BTreeMap::new();
    }
    object["mcpServers"]
        .as_object()
        .unwrap()
        .iter()
        .filter_map(|(name, value)| {
            parse_mcp_server(name, value, root, data_dir, diagnostics)
                .map(|server| (name.clone(), server))
        })
        .collect()
}

fn parse_mcp_server(
    name: &str,
    value: &Value,
    root: &Path,
    data_dir: &Path,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<PluginMcpServer> {
    if value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        == Some("sse")
    {
        diagnostic(
            diagnostics,
            "AP_MCP_TRANSPORT_UNSUPPORTED",
            DiagnosticSeverity::Error,
            DiagnosticScope::McpServer,
            Some(name.into()),
            false,
            "SSE transport is valid but unsupported by this Harn build".into(),
        );
        return None;
    }
    let result = (|| -> Result<PluginMcpServer, String> {
        let object = value.as_object().ok_or("server entry must be an object")?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or("server entry requires string `type`")?;
        match kind {
            "stdio" => parse_stdio(object, root, data_dir),
            "streamable-http" => parse_http(object),
            "sse" => unreachable!("valid SSE entries are handled as unsupported above"),
            _ => Err("`type` must be `stdio`, `streamable-http`, or `sse`".into()),
        }
    })();
    match result {
        Ok(server) => Some(server),
        Err(message) => {
            diagnostic(
                diagnostics,
                "AP_MCP_SERVER_INVALID",
                DiagnosticSeverity::Error,
                DiagnosticScope::McpServer,
                Some(name.into()),
                false,
                message,
            );
            None
        }
    }
}

fn parse_stdio(
    object: &Map<String, Value>,
    root: &Path,
    data_dir: &Path,
) -> Result<PluginMcpServer, String> {
    require_exact_fields(object, &["type", "command"], &["args", "env", "cwd"])?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| valid_command(value))
        .ok_or("`command` must be one executable token or a contained `./` plugin-relative path")?;
    let command = if let Some(relative) = command.strip_prefix("./") {
        contained_join(root, relative)?
            .to_string_lossy()
            .into_owned()
    } else {
        command.into()
    };
    let args = string_array(object.get("args"), "args")?
        .into_iter()
        .map(|value| expand_once(&value, root, data_dir))
        .collect();
    let mut env = string_map(object.get("env"), "env")?;
    if env.keys().any(|key| reserved_env_name(key)) {
        return Err("`env` must not define reserved PLUGIN_ROOT or PLUGIN_DATA".into());
    }
    for value in env.values_mut() {
        *value = expand_once(value, root, data_dir);
    }
    env.insert("PLUGIN_ROOT".into(), root.to_string_lossy().into_owned());
    env.insert(
        "PLUGIN_DATA".into(),
        data_dir.to_string_lossy().into_owned(),
    );
    let cwd = match object.get("cwd") {
        None => root.to_path_buf(),
        Some(Value::String(value)) => resolve_cwd(value, root, data_dir)?,
        Some(_) => return Err("`cwd` must be a string".into()),
    };
    Ok(PluginMcpServer::Stdio {
        command,
        args,
        env,
        cwd,
    })
}

fn parse_http(object: &Map<String, Value>) -> Result<PluginMcpServer, String> {
    require_exact_fields(object, &["type", "url"], &["headers"])?;
    let raw = object
        .get("url")
        .and_then(Value::as_str)
        .ok_or("`url` must be a string")?;
    let url = Url::parse(raw).map_err(|_| "`url` must be an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("`url` must be absolute HTTP(S) without userinfo or fragment".into());
    }
    if !is_loopback_host(url.host_str().unwrap()) && url.scheme() != "https" {
        return Err("non-loopback MCP URLs must use HTTPS".into());
    }
    let headers = string_map(object.get("headers"), "headers")?;
    let mut names = BTreeSet::new();
    for (name, value) in &headers {
        let folded = name.to_ascii_lowercase();
        if !names.insert(folded) {
            return Err("header names must be unique ignoring ASCII case".into());
        }
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid HTTP header name `{name}`"))?;
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| format!("invalid HTTP header value for `{name}`"))?;
    }
    Ok(PluginMcpServer::StreamableHttp {
        url: url.into(),
        headers,
    })
}

fn require_exact_fields(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    for field in required {
        if !object.contains_key(*field) {
            return Err(format!("missing required `{field}`"));
        }
    }
    for key in object.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(format!("unknown field `{key}`"));
        }
    }
    Ok(())
}

fn string_array(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .filter(|items| items.iter().all(Value::is_string))
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap().into())
                .collect()
        })
        .ok_or_else(|| format!("`{field}` must be an array of strings"))
}

fn string_map(value: Option<&Value>, field: &str) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .filter(|map| map.values().all(Value::is_string))
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap().into()))
                .collect()
        })
        .ok_or_else(|| format!("`{field}` must be an object of string values"))
}

fn valid_plugin_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.contains("--")
        && !name.contains("..")
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_command(command: &str) -> bool {
    !command.is_empty()
        && !command.chars().any(char::is_whitespace)
        && !command.contains("${")
        && (!command.contains('/') || command.starts_with("./"))
}

fn reserved_env_name(name: &str) -> bool {
    if cfg!(windows) {
        RESERVED_ENV
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    } else {
        RESERVED_ENV.contains(&name)
    }
}

fn expand_once(value: &str, root: &Path, data_dir: &Path) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${PLUGIN_") {
        expanded.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(tail) = rest.strip_prefix("${PLUGIN_ROOT}") {
            expanded.push_str(&root.to_string_lossy());
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("${PLUGIN_DATA}") {
            expanded.push_str(&data_dir.to_string_lossy());
            rest = tail;
        } else {
            expanded.push_str("${PLUGIN_");
            rest = &rest[9..];
        }
    }
    expanded.push_str(rest);
    expanded
}

fn resolve_cwd(value: &str, root: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    if let Some(relative) = value.strip_prefix("./") {
        return contained_join(root, relative);
    }
    if value == "${PLUGIN_ROOT}" {
        return Ok(root.to_path_buf());
    }
    if let Some(relative) = value.strip_prefix("${PLUGIN_ROOT}/") {
        return contained_join(root, relative);
    }
    if value == "${PLUGIN_DATA}" {
        return Ok(data_dir.to_path_buf());
    }
    if let Some(relative) = value.strip_prefix("${PLUGIN_DATA}/") {
        return contained_join(data_dir, relative);
    }
    Err("`cwd` must start with `./`, `${PLUGIN_ROOT}`, or `${PLUGIN_DATA}`".into())
}

fn contained_join(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("path escapes its declared plugin root".into());
    }
    let joined = base.join(relative);
    if !base.exists() {
        // `base` has already been resolved through its nearest existing
        // ancestor. With no filesystem entries below it yet, the lexical
        // parent-component check above is the complete containment proof.
        return Ok(joined);
    }
    if let Ok(canonical) = fs::canonicalize(&joined) {
        let canonical_base = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        if !canonical.starts_with(&canonical_base) {
            return Err("path resolves through a symlink outside its declared plugin root".into());
        }
        return Ok(canonical);
    }
    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| "path has no existing contained ancestor".to_string())?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor)
        .map_err(|error| format!("cannot resolve path ancestor: {error}"))?;
    let canonical_base = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    if !canonical_ancestor.starts_with(&canonical_base) {
        return Err("path resolves through a symlink outside its declared plugin root".into());
    }
    Ok(joined)
}

fn absolute_normalized(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn resolve_allow_missing(path: &Path) -> Option<PathBuf> {
    let absolute = absolute_normalized(path)?;
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return Some(canonical);
    }
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent()?;
    }
    let suffix = absolute.strip_prefix(ancestor).ok()?;
    fs::canonicalize(ancestor)
        .ok()
        .map(|base| base.join(suffix))
}

fn is_contained_file(root: &Path, path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with(root) && resolved.is_file())
}

fn is_contained_dir(root: &Path, path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with(root) && resolved.is_dir())
}

fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

fn fatal_report(code: &'static str, message: String) -> AgentPluginLoadReport {
    AgentPluginLoadReport {
        accepted: false,
        conformant: false,
        plugin: None,
        diagnostics: vec![PluginDiagnostic {
            code,
            severity: DiagnosticSeverity::Error,
            scope: DiagnosticScope::Plugin,
            component: None,
            fatal: true,
            message,
        }],
    }
}

fn diagnostic(
    diagnostics: &mut Vec<PluginDiagnostic>,
    code: &'static str,
    severity: DiagnosticSeverity,
    scope: DiagnosticScope,
    component: Option<String>,
    fatal: bool,
    message: String,
) {
    diagnostics.push(PluginDiagnostic {
        code,
        severity,
        scope,
        component,
        fatal,
        message,
    });
}

#[cfg(test)]
mod tests;
