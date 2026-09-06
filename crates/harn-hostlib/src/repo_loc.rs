//! Repository-owned measurement policy over the shared physical counter and language parsers.

use std::{collections::BTreeMap, fs, path::Path};

use crate::tools::args::to_agent_path;
use globset::{Glob, GlobSet, GlobSetBuilder};
use harn_lexer::{Lexer, TokenKind};
use serde::{Deserialize, Serialize};
use tokei::{Config, LanguageType};
use walkdir::WalkDir;

/// Repository-owned scope, ownership, and exclusions for one measurement.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocRegistry {
    /// Registry format version; currently one.
    pub version: u32,
    /// Empty means all files; directories are still traversed to account for excluded paths.
    #[serde(default)]
    pub include: Vec<String>,
    /// Existing Harn ignore policy; absence selects `none`.
    #[serde(default)]
    pub ignore_policy: Option<String>,
    /// Ordered ownership rules; the first match wins.
    pub areas: Vec<AreaRule>,
    /// Ordered exclusion rules; the first match wins.
    pub exclusions: Vec<ExclusionRule>,
}

/// Named ownership area selected by root-relative globs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AreaRule {
    /// Unique, nonempty repository area name.
    pub name: String,
    /// Root-relative path globs.
    pub patterns: Vec<String>,
}

/// Explicit reason for omitting matching files or directory subtrees.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionRule {
    /// Classification recorded in the receipt.
    pub reason: ExclusionReason,
    /// Root-relative path globs.
    pub patterns: Vec<String>,
}

/// Reasons a path is outside the measured source inventory.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Mechanically produced source or data.
    Generated,
    /// Third-party source maintained elsewhere.
    Vendored,
    /// Dependency resolution records.
    Lockfiles,
    /// Reproducible build products.
    BuildOutput,
    /// Another explicit repository policy exclusion.
    Other,
    /// Symbolic link; never traversed.
    Symlink,
    /// File outside the registry's include patterns.
    OutsideScope,
    /// Binary content, detected by NUL bytes or invalid UTF-8.
    NonText,
    /// Excluded by the selected Harn ignore policy.
    Ignored,
}

/// Additive measurements; code, comment and blank partition physical lines.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Counts {
    /// Lines under the shared hostlib physical-line definition.
    pub physical: u64,
    /// Harn comment-only delimiter lines, available for existing file-length ceilings.
    pub doc_delimiter_lines: u64,
    /// Lines containing code, including mixed code/comment lines.
    pub code: u64,
    /// Comment-only lines.
    pub comment: u64,
    /// Blank lines outside multiline tokens.
    pub blank: u64,
    /// Number of physical files represented.
    pub files: u64,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        self.physical += other.physical;
        self.doc_delimiter_lines += other.doc_delimiter_lines;
        self.code += other.code;
        self.comment += other.comment;
        self.blank += other.blank;
        self.files += other.files;
    }
}

/// One classified source file and its owning area.
#[derive(Debug, Serialize)]
pub struct FileCount {
    /// Root-relative agent path.
    pub path: String,
    /// Parser's canonical language name.
    pub language: String,
    /// First matching repository area, absent when unmapped.
    pub area: Option<String>,
    /// Reusable measurements from the file's source bytes.
    pub counts: Counts,
}

/// An inventoried path intentionally not read.
#[derive(Debug, Serialize)]
pub struct ExcludedPath {
    /// Root-relative agent path.
    pub path: String,
    /// Policy or filesystem reason for exclusion.
    pub reason: ExclusionReason,
    /// Whether this record excludes an entire directory subtree.
    pub directory: bool,
    /// Excluded paths are inventoried, never read or assigned fabricated zero counts.
    pub measured: bool,
}

/// Readable text whose source-language partition is unavailable.
#[derive(Debug, Serialize)]
pub struct UnsupportedFile {
    /// Root-relative agent path.
    pub path: String,
    /// Why the file could not be classified.
    pub reason: String,
    /// Physical lines, still usable by length ratchets.
    pub physical: u64,
    /// Physically whitespace-only lines, without inferred lexical meaning.
    pub blank: u64,
}

/// Complete inventory receipt, including evidence that prevents partial success.
#[derive(Debug, Serialize)]
pub struct LocReport {
    /// Receipt format version.
    pub schema_version: u32,
    /// Applied registry format version.
    pub registry_version: u32,
    /// Canonical directory in agent path format.
    pub root: String,
    /// True only when every included file has language counts and an area.
    pub complete: bool,
    /// Classified-source totals; unsupported text is separate.
    pub total: Counts,
    /// Counts grouped by each file's outer language.
    pub per_language: BTreeMap<String, Counts>,
    /// Counts grouped by declared ownership area.
    pub per_area: BTreeMap<String, Counts>,
    /// Classified files in stable traversal order.
    pub files: Vec<FileCount>,
    /// Explicitly excluded files and subtrees.
    pub excluded: Vec<ExcludedPath>,
    /// Unclassified files with retained physical measurements.
    pub unsupported: Vec<UnsupportedFile>,
    /// Classified paths with no matching ownership rule.
    pub unmapped: Vec<String>,
}

/// Invalid policy, unavailable inventory, or unreadable source.
#[derive(Debug, thiserror::Error)]
#[error("repository measurement: {0}")]
pub struct LocError(
    /// Diagnostic describing the failed boundary.
    pub String,
);

fn patterns(patterns: &[String]) -> Result<GlobSet, LocError> {
    if patterns.is_empty() {
        return Err(LocError("a policy rule has no patterns".into()));
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| LocError(error.to_string()))?);
    }
    builder.build().map_err(|error| LocError(error.to_string()))
}

/// Rules match slash-separated root-relative paths in declaration order.
/// No ignore-file or scanner defaults silently narrow the inventory.
pub fn measure(root: &Path, registry: &LocRegistry) -> Result<LocReport, LocError> {
    if registry.version != 1 {
        return Err(LocError(format!(
            "unsupported registry version {}",
            registry.version
        )));
    }
    let root = root
        .canonicalize()
        .map_err(|error| LocError(error.to_string()))?;
    if !root.is_dir() {
        return Err(LocError("root is not a directory".into()));
    }
    root.to_str()
        .ok_or_else(|| LocError("root is not UTF-8".into()))?;
    let policy = harn_vm::ignore_policy::IgnorePolicy::parse_for(
        "repo.loc",
        registry.ignore_policy.as_deref().unwrap_or("none"),
    )
    .map_err(LocError)?;
    let mut builder = ignore::WalkBuilder::new(&root);
    harn_vm::ignore_policy::configure(&mut builder, &root, policy, true).map_err(LocError)?;
    let mut matcher = builder
        .build_matchers()
        .pop()
        .expect("one measurement root");
    let mut areas = BTreeMap::new();
    let mut area_rules = Vec::new();
    for rule in &registry.areas {
        if rule.name.trim().is_empty()
            || areas.insert(rule.name.clone(), Counts::default()).is_some()
        {
            return Err(LocError(format!(
                "empty or duplicate area name {:?}",
                rule.name
            )));
        }
        area_rules.push(patterns(&rule.patterns)?);
    }
    let exclusions = registry
        .exclusions
        .iter()
        .map(|rule| patterns(&rule.patterns))
        .collect::<Result<Vec<_>, _>>()?;
    let included = if registry.include.is_empty() {
        None
    } else {
        Some(patterns(&registry.include)?)
    };
    let mut report = LocReport {
        schema_version: 1,
        registry_version: registry.version,
        root: to_agent_path(&root),
        complete: true,
        total: Counts::default(),
        per_language: BTreeMap::new(),
        per_area: areas,
        files: Vec::new(),
        excluded: Vec::new(),
        unsupported: Vec::new(),
        unmapped: Vec::new(),
    };
    let mut entries = WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|error| LocError(error.to_string()))?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .expect("walk stays below root");
        relative
            .to_str()
            .ok_or_else(|| LocError("path is not UTF-8".into()))?;
        let path = to_agent_path(relative);
        let directory = entry.file_type().is_dir();
        let (ignored, error) = matcher.matched_with_errors(relative, directory);
        if let Some(error) = error {
            return Err(LocError(format!("{path}: {error}")));
        }
        let reason = if entry.file_type().is_symlink() {
            Some(ExclusionReason::Symlink)
        } else {
            exclusions
                .iter()
                .position(|rule| rule.is_match(&path))
                .map(|index| registry.exclusions[index].reason)
        }
        .or_else(|| ignored.is_ignore().then_some(ExclusionReason::Ignored))
        .or_else(|| {
            (!directory && included.as_ref().is_some_and(|rule| !rule.is_match(&path)))
                .then_some(ExclusionReason::OutsideScope)
        });
        if let Some(reason) = reason {
            report.excluded.push(ExcludedPath {
                path,
                reason,
                directory,
                measured: false,
            });
            if directory {
                entries.skip_current_dir();
            }
            continue;
        }
        if directory {
            continue;
        }
        if !entry.file_type().is_file() {
            return Err(LocError(format!("unsupported filesystem entry {path}")));
        }
        let bytes = fs::read(entry.path()).map_err(|error| LocError(format!("{path}: {error}")))?;
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) if !bytes.contains(&0) => source,
            _ => {
                report.excluded.push(ExcludedPath {
                    path,
                    reason: ExclusionReason::NonText,
                    directory: false,
                    measured: false,
                });
                continue;
            }
        };
        let physical = crate::text::count_lines(&bytes);
        let mut unsupported_reason = "unrecognized language".to_owned();
        let counted = if path.ends_with(".harn") || path.ends_with(".harn.txt") {
            match harn_counts(source) {
                Ok(counts) => Some(("Harn".to_owned(), counts)),
                Err(error) => {
                    unsupported_reason = error.to_string();
                    None
                }
            }
        } else {
            LanguageType::from_path(entry.path(), &Config::default()).and_then(|language| {
                // Child blobs are disjoint from the parent counters. Fold them exactly once;
                // the physical file belongs to its outer language in this receipt.
                let stats = language
                    .parse_from_slice(&bytes, &Config::default())
                    .summarise();
                if stats.lines() as u64 != physical {
                    unsupported_reason = format!(
                        "{} parser classified {} of {physical} physical lines",
                        language.name(),
                        stats.lines()
                    );
                    return None;
                }
                Some((
                    language.name().to_owned(),
                    Counts {
                        physical,
                        doc_delimiter_lines: 0,
                        code: stats.code as u64,
                        comment: stats.comments as u64,
                        blank: stats.blanks as u64,
                        files: 1,
                    },
                ))
            })
        };
        let Some((language, counts)) = counted else {
            report.unsupported.push(UnsupportedFile {
                path,
                reason: unsupported_reason,
                physical,
                blank: source.lines().filter(|line| line.trim().is_empty()).count() as u64,
            });
            continue;
        };
        let area = area_rules
            .iter()
            .position(|rule| rule.is_match(&path))
            .map(|index| registry.areas[index].name.clone());
        if let Some(area) = &area {
            report
                .per_area
                .get_mut(area)
                .expect("validated area")
                .add(&counts);
        } else {
            report.unmapped.push(path.clone());
        }
        report.total.add(&counts);
        report
            .per_language
            .entry(language.clone())
            .or_default()
            .add(&counts);
        report.files.push(FileCount {
            path,
            language,
            area,
            counts,
        });
    }
    if report.files.is_empty() && report.unsupported.is_empty() {
        return Err(LocError(format!(
            "empty eligible inventory ({} unsupported, {} excluded)",
            report.unsupported.len(),
            report.excluded.len()
        )));
    }
    report.complete = report.unsupported.is_empty() && report.unmapped.is_empty();
    Ok(report)
}

fn harn_counts(source: &str) -> Result<Counts, LocError> {
    let physical = crate::text::count_lines(source.as_bytes());
    let mut lines = vec![0u8; physical as usize];
    for token in Lexer::new(source)
        .tokenize_with_comments()
        .map_err(|error| LocError(error.to_string()))?
    {
        let class = match token.kind {
            TokenKind::Eof | TokenKind::Newline => continue,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => 1,
            _ => 2,
        };
        let end = token.span.end.min(source.len());
        let last = token.span.line - 1
            + source.as_bytes()[token.span.start..end]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
        for line in lines.iter_mut().take(last + 1).skip(token.span.line - 1) {
            *line = (*line).max(class);
        }
    }
    Ok(Counts {
        physical,
        doc_delimiter_lines: source
            .lines()
            .zip(&lines)
            .filter(|(text, class)| **class == 1 && matches!(text.trim(), "/**" | "*/"))
            .count() as u64,
        code: lines.iter().filter(|&&line| line == 2).count() as u64,
        comment: lines.iter().filter(|&&line| line == 1).count() as u64,
        blank: lines.iter().filter(|&&line| line == 0).count() as u64,
        files: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_repository_accounts_for_every_path_without_reclassifying_strings() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("main.harn"),
            "// note\nconst text = \"\"\"\n/* string */\n\"\"\"\n\n",
        )
        .unwrap();
        fs::write(
            root.path().join("main.rs"),
            "/* outer /* nested */ end */\nfn main() {} // mixed\n",
        )
        .unwrap();
        fs::write(root.path().join("config.toml"), "# config\nkey = true\n").unwrap();
        fs::write(root.path().join("UNKNOWN"), "unclassified\n\n").unwrap();
        fs::create_dir(root.path().join("generated")).unwrap();
        fs::write(root.path().join("generated/invalid.harn"), "\"unterminated").unwrap();
        let registry = LocRegistry {
            version: 1,
            include: vec![],
            ignore_policy: None,
            areas: vec![AreaRule {
                name: "source".into(),
                patterns: vec!["*".into()],
            }],
            exclusions: vec![ExclusionRule {
                reason: ExclusionReason::Generated,
                patterns: vec!["generated".into()],
            }],
        };
        let report = measure(root.path(), &registry).unwrap();
        assert!(!report.complete);
        assert_eq!(
            (
                report.total.physical,
                report.total.code,
                report.total.comment,
                report.total.blank,
                report.total.files
            ),
            (9, 5, 3, 1, 3)
        );
        assert_eq!(report.per_area["source"].physical, 9);
        assert_eq!(
            report
                .per_language
                .values()
                .map(|count| count.physical)
                .sum::<u64>(),
            9
        );
        assert_eq!(report.unsupported[0].path, "UNKNOWN");
        assert_eq!(
            (report.unsupported[0].physical, report.unsupported[0].blank),
            (2, 1)
        );
        assert_eq!(report.excluded[0].path, "generated");
        assert!(!report.excluded[0].measured);
        fs::write(root.path().join("asset.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();
        fs::write(root.path().join("nul.txt"), b"binary\0data").unwrap();
        let assets = measure(root.path(), &registry).unwrap();
        assert_eq!(assets.total.physical, report.total.physical);
        assert_eq!(
            assets
                .excluded
                .iter()
                .filter(|file| matches!(file.reason, ExclusionReason::NonText) && !file.measured)
                .count(),
            2
        );
        fs::write(root.path().join("broken.harn"), "\"unterminated").unwrap();
        let partial = measure(root.path(), &registry).unwrap();
        assert!(!partial.complete);
        assert!(partial
            .unsupported
            .iter()
            .any(|file| file.path == "broken.harn" && !file.reason.is_empty()));
        let scoped = LocRegistry {
            include: vec!["*.rs".into()],
            ..registry.clone()
        };
        let scoped = measure(root.path(), &scoped).unwrap();
        assert!(scoped.complete);
        assert_eq!(scoped.total.files, 1);
        assert!(scoped.excluded.iter().any(|file| file.path == "broken.harn"
            && matches!(file.reason, ExclusionReason::OutsideScope)));
        assert!(measure(&root.path().join("absent"), &registry).is_err());
        assert!(measure(&root.path().join("main.rs"), &registry).is_err());
        assert!(measure(tempfile::tempdir().unwrap().path(), &registry)
            .unwrap_err()
            .to_string()
            .contains("empty eligible"));
    }

    #[test]
    fn harn_delimiter_projection_uses_comment_spans_not_quote_toggles() {
        let counts = harn_counts("/**\ntext\n*/\nconst text = \"\"\"\n/**\n*/\n\"\"\"\n").unwrap();
        assert_eq!(counts.doc_delimiter_lines, 2);
        assert_eq!((counts.code, counts.comment, counts.physical), (4, 3, 7));
        // Quotes in a line comment cannot open a multiline string or hide later comments.
        let counts = harn_counts("// \"\"\"\n/**\ntext\n*/\nconst end = 1 /* mixed */\n").unwrap();
        assert_eq!(counts.doc_delimiter_lines, 2);
        assert_eq!((counts.code, counts.comment), (1, 4));
    }

    #[test]
    fn notebook_cell_counts_cannot_masquerade_as_physical_source_counts() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            root.path().join("cells.ipynb"),
            "{\n\"cells\": [],\n\"metadata\": {},\n\"nbformat\": 4\n}\n",
        )
        .unwrap();
        let registry = LocRegistry {
            version: 1,
            include: vec![],
            ignore_policy: None,
            exclusions: vec![],
            areas: vec![AreaRule {
                name: "source".into(),
                patterns: vec!["*".into()],
            }],
        };
        let report = measure(root.path(), &registry).unwrap();
        assert!(!report.complete);
        assert_eq!(report.total.physical, 1);
        assert_eq!(report.unsupported.len(), 1);
        assert_eq!(report.unsupported[0].physical, 5);
        assert!(report.unsupported[0].reason.contains("classified 0 of 5"));
        fs::remove_file(root.path().join("main.rs")).unwrap();
        let unsupported_only = measure(root.path(), &registry).unwrap();
        assert!(!unsupported_only.complete);
        assert_eq!(unsupported_only.total.files, 0);
        assert_eq!(unsupported_only.unsupported[0].physical, 5);
    }

    #[test]
    fn project_ignore_preserves_nested_rules_negation_and_loading_errors() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join(".gitignore"), "*.rs\n!src/keep.rs\n").unwrap();
        fs::write(root.path().join("src/.agentignore"), "!nested.rs\n").unwrap();
        for path in ["skip.rs", "src/keep.rs", "src/nested.rs", "src/skip.rs"] {
            fs::write(root.path().join(path), "fn main() {}\n").unwrap();
        }
        let registry = LocRegistry {
            version: 1,
            include: vec!["**/*.rs".into()],
            ignore_policy: Some("project".into()),
            exclusions: vec![],
            areas: vec![AreaRule {
                name: "source".into(),
                patterns: vec!["**".into()],
            }],
        };
        let report = measure(root.path(), &registry).unwrap();
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["src/keep.rs", "src/nested.rs"]
        );
        assert!(report
            .excluded
            .iter()
            .any(|file| file.path == "skip.rs" && matches!(file.reason, ExclusionReason::Ignored)));
        fs::write(root.path().join("src/.agentignore"), "[z-a]\n").unwrap();
        assert!(measure(root.path(), &registry).is_err());
    }
}
