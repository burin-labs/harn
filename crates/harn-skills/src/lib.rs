//! Embedded Harn skill corpus.
//!
//! This crate exposes the bundled corpus as metadata plus `SKILL.md`
//! bodies. CLI commands that enumerate, dump, or install these skills
//! are layered above this foundation.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment override for canonical Harn skill discovery.
pub const HARN_SKILLS_DIR_ENV: &str = "HARN_SKILLS_DIR";

/// Frontmatter fields embedded with each bundled skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: &'static str,
    pub short: &'static str,
    pub description: &'static str,
    pub when_to_use: Option<&'static str>,
}

/// A single skill embedded into the Harn build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedSkill {
    pub name: &'static str,
    pub frontmatter: SkillFrontmatter,
    pub body: &'static str,
    /// The full original SKILL.md source — frontmatter delimiter,
    /// frontmatter block, blank line, and body — exactly as embedded.
    /// Use this when round-tripping a skill back to disk so the dumped
    /// copy is byte-identical to the binary's canonical record.
    pub source: &'static str,
}

/// Owned frontmatter fields loaded from a `SKILL.md` on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSkillFrontmatter {
    pub name: String,
    pub short: String,
    pub description: String,
    pub when_to_use: Option<String>,
}

/// A single skill discovered recursively from `HARN_SKILLS_DIR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSkill {
    pub name: String,
    pub frontmatter: DiskSkillFrontmatter,
    pub body: String,
    pub source: String,
    pub path: PathBuf,
}

/// The active canonical corpus used by `harn skills list/get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillCorpus {
    Embedded(&'static [EmbeddedSkill]),
    Disk(Vec<DiskSkill>),
}

impl SkillCorpus {
    pub fn is_disk(&self) -> bool {
        matches!(self, Self::Disk(_))
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Embedded(skills) => skills.len(),
            Self::Disk(skills) => skills.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Error returned when disk skill discovery finds malformed files.
#[derive(Debug)]
pub enum SkillDiscoveryError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    MissingFrontmatter {
        path: PathBuf,
    },
    MissingField {
        path: PathBuf,
        field: &'static str,
    },
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

impl fmt::Display for SkillDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::MissingFrontmatter { path } => {
                write!(f, "{}: missing SKILL.md frontmatter", path.display())
            }
            Self::MissingField { path, field } => {
                write!(f, "{}: missing `{field}` frontmatter field", path.display())
            }
            Self::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate skill `{name}` in {} and {}",
                first.display(),
                second.display()
            ),
        }
    }
}

impl std::error::Error for SkillDiscoveryError {}

const SOURCES: &[&str] = &[
    include_str!("corpus/harn-agent/SKILL.md"),
    include_str!("corpus/harn-diagnostics/SKILL.md"),
    include_str!("corpus/harn-language/SKILL.md"),
    include_str!("corpus/harn-orchestration/SKILL.md"),
    include_str!("corpus/harn-probe/SKILL.md"),
    include_str!("corpus/harn-providers/SKILL.md"),
    include_str!("corpus/harn-testing/SKILL.md"),
    include_str!("corpus/harn-tracing/SKILL.md"),
    include_str!("corpus/release-harn/SKILL.md"),
];

static EMBEDDED_SKILLS: OnceLock<Box<[EmbeddedSkill]>> = OnceLock::new();

/// Return every skill bundled into this build.
pub fn list_embedded_skills() -> &'static [EmbeddedSkill] {
    EMBEDDED_SKILLS
        .get_or_init(|| SOURCES.iter().map(|source| parse_skill(source)).collect())
        .as_ref()
}

/// Return one bundled skill by canonical skill name.
pub fn get_embedded_skill(name: &str) -> Option<&'static EmbeddedSkill> {
    list_embedded_skills()
        .iter()
        .find(|skill| skill.name == name)
}

/// Return the active canonical corpus. `HARN_SKILLS_DIR` wins only
/// when it contains at least one recursively discovered `SKILL.md`;
/// otherwise callers fall back to the embedded corpus.
pub fn resolve_skill_corpus_from_env() -> Result<SkillCorpus, SkillDiscoveryError> {
    let Ok(dir) = env::var(HARN_SKILLS_DIR_ENV) else {
        return Ok(SkillCorpus::Embedded(list_embedded_skills()));
    };
    if dir.trim().is_empty() {
        return Ok(SkillCorpus::Embedded(list_embedded_skills()));
    }

    let skills = list_disk_skills(dir)?;
    if skills.is_empty() {
        Ok(SkillCorpus::Embedded(list_embedded_skills()))
    } else {
        Ok(SkillCorpus::Disk(skills))
    }
}

/// Recursively discover `SKILL.md` files under `root`.
///
/// A missing root is treated as an empty disk corpus so
/// `HARN_SKILLS_DIR` can fall back cleanly to embedded skills.
pub fn list_disk_skills(root: impl AsRef<Path>) -> Result<Vec<DiskSkill>, SkillDiscoveryError> {
    let root = root.as_ref();
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    collect_skill_paths(root, &mut paths)?;
    paths.sort();

    let mut by_name: BTreeMap<String, DiskSkill> = BTreeMap::new();
    for path in paths {
        let skill = parse_disk_skill(&path)?;
        if let Some(first) = by_name.get(&skill.name) {
            return Err(SkillDiscoveryError::DuplicateName {
                name: skill.name,
                first: first.path.clone(),
                second: path,
            });
        }
        by_name.insert(skill.name.clone(), skill);
    }

    Ok(by_name.into_values().collect())
}

fn parse_skill(source: &'static str) -> EmbeddedSkill {
    let (frontmatter, body) = split_frontmatter(source);
    let frontmatter = parse_frontmatter(frontmatter);
    EmbeddedSkill {
        name: frontmatter.name,
        frontmatter,
        body,
        source,
    }
}

fn collect_skill_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), SkillDiscoveryError> {
    let entries = fs::read_dir(dir).map_err(|source| SkillDiscoveryError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SkillDiscoveryError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| SkillDiscoveryError::Io {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            collect_skill_paths(&path, out)?;
        } else if file_type.is_file() && entry.file_name() == "SKILL.md" {
            out.push(path);
        }
    }
    Ok(())
}

fn parse_disk_skill(path: &Path) -> Result<DiskSkill, SkillDiscoveryError> {
    let source = fs::read_to_string(path).map_err(|source| SkillDiscoveryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let (frontmatter, body) =
        split_disk_frontmatter(&source).ok_or_else(|| SkillDiscoveryError::MissingFrontmatter {
            path: path.to_path_buf(),
        })?;
    let frontmatter = parse_disk_frontmatter(path, frontmatter)?;
    Ok(DiskSkill {
        name: frontmatter.name.clone(),
        frontmatter,
        body: body.to_string(),
        source,
        path: path.to_path_buf(),
    })
}

fn split_disk_frontmatter(source: &str) -> Option<(&str, &str)> {
    split_frontmatter_parts(source)
}

fn parse_disk_frontmatter(
    path: &Path,
    frontmatter: &str,
) -> Result<DiskSkillFrontmatter, SkillDiscoveryError> {
    let mut name = None;
    let mut short = None;
    let mut description = None;
    let mut when_to_use = None;

    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key {
            "name" => name = Some(value),
            "short" => short = Some(value),
            "description" => description = Some(value),
            "when_to_use" => when_to_use = Some(value),
            _ => {}
        }
    }

    Ok(DiskSkillFrontmatter {
        name: require_disk_field(path, name, "name")?,
        short: short.unwrap_or_default(),
        description: require_disk_field(path, description, "description")?,
        when_to_use,
    })
}

fn require_disk_field(
    path: &Path,
    value: Option<String>,
    field: &'static str,
) -> Result<String, SkillDiscoveryError> {
    value.ok_or_else(|| SkillDiscoveryError::MissingField {
        path: path.to_path_buf(),
        field,
    })
}

fn split_frontmatter(source: &'static str) -> (&'static str, &'static str) {
    let Some((after_open, line_ending)) = split_opening_frontmatter(source) else {
        panic!("embedded skill source is missing opening frontmatter delimiter");
    };
    let Some((frontmatter, body)) = split_closing_frontmatter(after_open, line_ending) else {
        panic!("embedded skill source is missing closing frontmatter delimiter");
    };
    (frontmatter, body)
}

fn split_frontmatter_parts(source: &str) -> Option<(&str, &str)> {
    let (after_open, line_ending) = split_opening_frontmatter(source)?;
    split_closing_frontmatter(after_open, line_ending)
}

fn split_opening_frontmatter(source: &str) -> Option<(&str, &str)> {
    if let Some(after_open) = source.strip_prefix("---\n") {
        Some((after_open, "\n"))
    } else if let Some(after_open) = source.strip_prefix("---\r\n") {
        Some((after_open, "\r\n"))
    } else {
        None
    }
}

fn split_closing_frontmatter<'a>(
    after_open: &'a str,
    line_ending: &str,
) -> Option<(&'a str, &'a str)> {
    let close = format!("{line_ending}---{line_ending}");
    let close_offset = after_open.find(&close)?;
    Some((
        &after_open[..close_offset],
        &after_open[close_offset + close.len()..],
    ))
}

fn parse_frontmatter(frontmatter: &'static str) -> SkillFrontmatter {
    let mut name = None;
    let mut short = None;
    let mut description = None;
    let mut when_to_use = None;

    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "name" => name = Some(value),
            "short" => short = Some(value),
            "description" => description = Some(value),
            "when_to_use" => when_to_use = Some(value),
            _ => {}
        }
    }

    SkillFrontmatter {
        name: name.expect("embedded skill frontmatter is missing `name`"),
        short: short.expect("embedded skill frontmatter is missing `short`"),
        description: description.expect("embedded skill frontmatter is missing `description`"),
        when_to_use,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    #[test]
    fn lists_expected_initial_corpus() {
        let skills = list_embedded_skills();
        let names: Vec<&str> = skills.iter().map(|skill| skill.name).collect();
        assert_eq!(
            names,
            [
                "harn-agent",
                "harn-diagnostics",
                "harn-language",
                "harn-orchestration",
                "harn-probe",
                "harn-providers",
                "harn-testing",
                "harn-tracing",
                "release-harn",
            ]
        );
        assert_eq!(skills.len(), SOURCES.len());
    }

    #[test]
    fn can_fetch_harn_language_skill() {
        let skill = get_embedded_skill("harn-language").expect("harn-language skill is embedded");
        assert_eq!(skill.frontmatter.name, "harn-language");
        assert!(skill.body.contains("Harn language"));
    }

    #[test]
    fn skills_have_unique_names_and_body_only_content() {
        let mut names = BTreeSet::new();
        for skill in list_embedded_skills() {
            assert_eq!(skill.name, skill.frontmatter.name);
            assert!(names.insert(skill.name), "duplicate skill {}", skill.name);
            assert!(
                !skill.body.trim().is_empty(),
                "{} body is empty",
                skill.name
            );
            assert!(
                !skill.body.trim_start().starts_with("---"),
                "{} body includes frontmatter",
                skill.name
            );
        }
    }

    #[test]
    fn skills_are_sorted_by_name() {
        let names: Vec<&str> = list_embedded_skills()
            .iter()
            .map(|skill| skill.name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn source_round_trips_to_frontmatter_and_body() {
        for skill in list_embedded_skills() {
            assert!(
                split_frontmatter_parts(skill.source).is_some(),
                "{} source missing opening fence",
                skill.name
            );
            assert!(
                skill.source.ends_with(skill.body),
                "{} source must end with the body so dump output is byte-stable",
                skill.name
            );
            assert!(
                skill.source.contains(&format!("name: {}\n", skill.name)),
                "{} source missing canonical name field",
                skill.name
            );
        }
    }

    #[test]
    fn frontmatter_split_accepts_crlf_sources() {
        let source = "---\r\nname: crlf\r\n---\r\n# Body\r\n";
        let (frontmatter, body) = split_frontmatter_parts(source).expect("CRLF frontmatter");
        assert_eq!(frontmatter, "name: crlf");
        assert_eq!(body, "# Body\r\n");
    }

    #[test]
    fn frontmatter_split_rejects_missing_closing_fence() {
        assert!(split_frontmatter_parts("---\nname: missing\n# Body\n").is_none());
    }

    #[test]
    fn embedded_corpus_stays_within_binary_budget() {
        let bytes: usize = SOURCES.iter().map(|source| source.len()).sum();
        assert!(
            bytes <= 200 * 1024,
            "embedded corpus is {bytes} bytes, expected <= 200 KiB"
        );
    }

    #[test]
    fn skill_bodies_are_focused_and_not_placeholders() {
        let expectations = [
            ("harn-agent", ["agent_loop", "session id", "approval"]),
            ("harn-diagnostics", ["diagnostic", "repair", "conformance"]),
            ("harn-language", ["quickref", "type", "conformance"]),
            ("harn-orchestration", ["agent_loop", "workflow", "host"]),
            ("harn-probe", ["probe", "fact", "evidence"]),
            ("harn-providers", ["llm_call", "provider", "schema"]),
            (
                "harn-testing",
                ["conformance", "deterministic", "mock_time"],
            ),
            ("harn-tracing", ["replay", "receipts", "transcript"]),
            ("release-harn", ["release_ship", "merge queue", "tag"]),
        ];

        for (name, terms) in expectations {
            let skill = get_embedded_skill(name).expect("expected embedded skill");
            let body = skill.body.to_ascii_lowercase();
            assert!(
                !body.contains("embedded stub") && !body.contains("placeholder"),
                "{name} should contain real guidance, not stub wording"
            );
            for term in terms {
                assert!(
                    body.contains(term),
                    "{name} body should mention focused term `{term}`"
                );
            }
        }
    }

    #[test]
    fn skill_bodies_match_split_skill_contract() {
        for skill in list_embedded_skills() {
            let lines = skill.body.lines().count();
            assert!(
                lines >= 80,
                "{} body is {lines} lines, expected at least 80",
                skill.name
            );
            assert!(
                lines <= 300,
                "{} body is {lines} lines, expected at most 300",
                skill.name
            );
        }
    }

    #[test]
    fn skill_cross_links_resolve_to_embedded_skills() {
        let names: BTreeSet<&str> = list_embedded_skills()
            .iter()
            .map(|skill| skill.name)
            .collect();
        for skill in list_embedded_skills() {
            for reference in bracketed_skill_references(skill.body) {
                assert!(
                    names.contains(reference),
                    "{} links to unknown embedded skill [[{}]]",
                    skill.name,
                    reference
                );
            }
        }
    }

    #[test]
    fn diagnostics_skill_mentions_all_code_categories() {
        let skill = get_embedded_skill("harn-diagnostics").expect("diagnostics skill");
        for category in [
            "TYP", "PAR", "NAM", "CAP", "LLM", "ORC", "STD", "PRM", "MOD", "LNT", "FMT", "IMP",
            "OWN", "RCV", "MAT",
        ] {
            assert!(
                skill.body.contains(&format!("`{category}`")),
                "harn-diagnostics should mention diagnostic category `{category}`"
            );
        }
    }

    #[test]
    fn disk_discovery_finds_recursive_skill_files_sorted_by_name() {
        let temp = TempDir::new().expect("temp dir");
        write_skill(
            &temp.path().join("zeta").join("SKILL.md"),
            "zeta-skill",
            "Zeta",
        );
        write_skill(
            &temp.path().join("nested").join("alpha").join("SKILL.md"),
            "alpha-skill",
            "Alpha",
        );

        let skills = list_disk_skills(temp.path()).expect("discover disk skills");
        let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["alpha-skill", "zeta-skill"]);
        assert_eq!(skills[0].frontmatter.description, "Alpha description");
        assert!(skills[0].body.contains("Alpha body"));
    }

    #[test]
    fn disk_discovery_treats_missing_root_as_empty() {
        let temp = TempDir::new().expect("temp dir");
        let skills = list_disk_skills(temp.path().join("missing")).expect("discover disk skills");
        assert!(skills.is_empty());
    }

    #[test]
    fn disk_discovery_rejects_duplicate_skill_names() {
        let temp = TempDir::new().expect("temp dir");
        write_skill(
            &temp.path().join("one").join("SKILL.md"),
            "same-skill",
            "One",
        );
        write_skill(
            &temp.path().join("two").join("SKILL.md"),
            "same-skill",
            "Two",
        );

        let error = list_disk_skills(temp.path()).expect_err("duplicate name should fail");
        assert!(
            error.to_string().contains("duplicate skill `same-skill`"),
            "unexpected error: {error}"
        );
    }

    fn write_skill(path: &Path, name: &str, label: &str) {
        fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill parent");
        fs::write(
            path,
            format!(
                "---\nname: {name}\nshort: {label} short\ndescription: {label} description\n---\n# {label}\n\n{label} body\n"
            ),
        )
        .expect("write SKILL.md");
    }

    fn bracketed_skill_references(body: &str) -> Vec<&str> {
        let mut references = Vec::new();
        let mut rest = body;
        while let Some(start) = rest.find("[[") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find("]]") else {
                break;
            };
            references.push(&rest[..end]);
            rest = &rest[end + 2..];
        }
        references
    }
}
