//! Owned data model for the scanner output. Mirrors the Swift
//! `ScanResult`/`FileRecord`/`SymbolRecord`/etc. shape from
//! `Sources/BurinCore/Scanner/RepoScannerModels.swift`.
//!
//! Fields are renamed `snake_case` (the JSON shape is also snake_case in
//! schemas/scanner/scan_project.response.json — burin-code's consumers
//! map between the two via Codable's coding keys).

use serde::{Deserialize, Serialize};

/// Coarse symbol kinds. Matches `SymbolKindRS` on the Swift side.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Free function.
    Function,
    /// Method (function attached to a type).
    Method,
    /// Class definition.
    #[serde(rename = "class")]
    ClassDecl,
    /// Struct definition.
    #[serde(rename = "struct")]
    StructDecl,
    /// Enum definition.
    #[serde(rename = "enum")]
    EnumDecl,
    /// Protocol definition (Swift / Obj-C-style).
    #[serde(rename = "protocol")]
    ProtocolDecl,
    /// Interface definition (Java / TypeScript).
    #[serde(rename = "interface")]
    InterfaceDecl,
    /// Type alias.
    #[serde(rename = "typealias")]
    TypeAlias,
    /// Property / field on a type.
    Property,
    /// Module-level variable.
    Variable,
    /// Module-level constant.
    Constant,
    /// Module / package marker.
    Module,
    /// `// MARK:`-style section header.
    Mark,
    /// `// TODO:` annotation.
    Todo,
    /// `// FIXME:` annotation.
    Fixme,
    /// Anything else.
    Other,
}

impl SymbolKind {
    /// True for kinds the importance scorer treats as "type definitions".
    pub fn is_type_definition(self) -> bool {
        matches!(
            self,
            SymbolKind::ClassDecl
                | SymbolKind::StructDecl
                | SymbolKind::EnumDecl
                | SymbolKind::ProtocolDecl
                | SymbolKind::InterfaceDecl
        )
    }

    /// Lowercase keyword used by the repo-map text builder.
    pub fn keyword(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::ClassDecl => "class",
            SymbolKind::StructDecl => "struct",
            SymbolKind::EnumDecl => "enum",
            SymbolKind::ProtocolDecl => "protocol",
            SymbolKind::InterfaceDecl => "interface",
            SymbolKind::TypeAlias => "typealias",
            SymbolKind::Property => "property",
            SymbolKind::Variable => "variable",
            SymbolKind::Constant => "constant",
            SymbolKind::Module => "module",
            SymbolKind::Mark => "mark",
            SymbolKind::Todo => "todo",
            SymbolKind::Fixme => "fixme",
            SymbolKind::Other => "other",
        }
    }
}

/// One file's metadata + import list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    /// Stable id. Equal to `relative_path`.
    pub id: String,
    /// Repo-relative POSIX path.
    pub relative_path: String,
    /// Last path component.
    pub file_name: String,
    /// Lowercase extension (no dot) or `""`.
    pub language: String,
    /// Newline-counted line count.
    pub line_count: usize,
    /// Raw byte size on disk.
    pub size_bytes: u64,
    /// Last modification time, milliseconds since unix epoch (`0` if unknown).
    pub last_modified_unix_ms: i64,
    /// Module/path strings extracted by the import parser.
    pub imports: Vec<String>,
    /// Normalized 0..1 git-churn score (0 if `include_git_history=false`).
    pub churn_score: f64,
    /// Repo-relative path to a paired test file, if [`super::test_mapping`]
    /// found one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub corresponding_test_file: Option<String>,
}

/// One symbol's metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolRecord {
    /// Stable id of the form `{file}:{name}:{line}`.
    pub id: String,
    /// Symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: SymbolKind,
    /// File this symbol lives in (repo-relative POSIX).
    pub file_path: String,
    /// 1-indexed line number.
    pub line: usize,
    /// Optional signature snippet (truncated by the extractor).
    pub signature: String,
    /// Enclosing type name when the symbol is a method/property.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub container: Option<String>,
    /// Cross-file reference count derived from `imports`.
    pub reference_count: usize,
    /// Heuristic importance score (higher = more central).
    pub importance_score: f64,
}

/// One folder's aggregate metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderRecord {
    /// Always equals `relative_path` (used by the Swift consumer for `Identifiable`).
    pub id: String,
    /// Folder path (`"."` for repo root).
    pub relative_path: String,
    /// Number of indexed files in the folder.
    pub file_count: usize,
    /// Sum of `line_count` across files in the folder.
    pub line_count: usize,
    /// Most-frequent language extension.
    pub dominant_language: String,
    /// Top 5 type-definition symbol names sorted by importance score.
    pub key_symbol_names: Vec<String>,
}

/// Per-language summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LanguageStat {
    /// Lowercase extension.
    pub name: String,
    /// Number of files.
    pub file_count: usize,
    /// Total lines across files.
    pub line_count: usize,
    /// Share of total project lines, in percent.
    pub percentage: f64,
}

/// Project-level metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMetadata {
    /// Project name (last path component of root).
    pub name: String,
    /// Absolute root path.
    pub root_path: String,
    /// Per-language line/file/percentage breakdown, sorted desc by lines.
    pub languages: Vec<LanguageStat>,
    /// Map: command (e.g. `pnpm test`) → human-readable label.
    pub test_commands: std::collections::BTreeMap<String, String>,
    /// Best-guess preferred test command, if any.
    pub detected_test_command: Option<String>,
    /// Heuristic project pattern hints (e.g. detected ORM, Zod, etc.).
    pub code_patterns: Vec<String>,
    /// Total file count.
    pub total_files: usize,
    /// Total line count.
    pub total_lines: usize,
    /// ISO-8601 UTC timestamp when scanning finished.
    pub last_scanned_at: String,
}

/// One edge of the file-level dependency graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// File where the import statement appears.
    pub from_file: String,
    /// Module/path the import names.
    pub to_module: String,
}

/// Detected sub-project marker (Cargo.toml, package.json, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubProject {
    /// Absolute path.
    pub path: String,
    /// Human name.
    pub name: String,
    /// Primary language (matches the marker).
    pub language: String,
    /// Marker file name.
    pub project_marker: String,
}

/// Path-delta accompanying a [`scan_incremental`](super::scan_incremental)
/// response.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ScanDelta {
    /// Paths newly present since the snapshot.
    pub added: Vec<String>,
    /// Paths whose content changed since the snapshot.
    pub modified: Vec<String>,
    /// Paths absent since the snapshot.
    pub removed: Vec<String>,
    /// True when the diff exceeded ~30% of the snapshot or the snapshot
    /// was missing/stale, forcing a full rescan.
    pub full_rescan: bool,
}

/// Top-level scanner output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanResult {
    /// Opaque cookie identifying the persisted snapshot for this scan.
    pub snapshot_token: String,
    /// True when `max_files` truncated the file list.
    pub truncated: bool,
    /// Project-level metadata.
    pub project: ProjectMetadata,
    /// Folder aggregates, sorted desc by `line_count`.
    pub folders: Vec<FolderRecord>,
    /// File records, sorted asc by `relative_path`.
    pub files: Vec<FileRecord>,
    /// Symbol records, sorted asc by `id` for deterministic output.
    pub symbols: Vec<SymbolRecord>,
    /// Import-derived dependency edges.
    pub dependencies: Vec<DependencyEdge>,
    /// Detected sub-projects beneath `root` (max 2 levels deep).
    pub sub_projects: Vec<SubProject>,
    /// Token-budgeted text repo map.
    pub repo_map: String,
}
