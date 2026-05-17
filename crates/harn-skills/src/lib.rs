//! Embedded Harn skill corpus.
//!
//! This crate exposes the bundled corpus as metadata plus `SKILL.md`
//! bodies. CLI commands that enumerate, dump, or install these skills
//! are layered above this foundation.

use std::sync::OnceLock;

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
}

const SOURCES: &[&str] = &[
    include_str!("corpus/harn-agent/SKILL.md"),
    include_str!("corpus/harn-diagnostics/SKILL.md"),
    include_str!("corpus/harn-language/SKILL.md"),
    include_str!("corpus/harn-orchestration/SKILL.md"),
    include_str!("corpus/harn-providers/SKILL.md"),
    include_str!("corpus/harn-testing/SKILL.md"),
    include_str!("corpus/harn-tracing/SKILL.md"),
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

fn parse_skill(source: &'static str) -> EmbeddedSkill {
    let (frontmatter, body) = split_frontmatter(source);
    let frontmatter = parse_frontmatter(frontmatter);
    EmbeddedSkill {
        name: frontmatter.name,
        frontmatter,
        body,
    }
}

fn split_frontmatter(source: &'static str) -> (&'static str, &'static str) {
    let Some(after_open) = source.strip_prefix("---\n") else {
        panic!("embedded skill source is missing opening frontmatter delimiter");
    };
    let Some(close_offset) = after_open.find("\n---\n") else {
        panic!("embedded skill source is missing closing frontmatter delimiter");
    };
    (
        &after_open[..close_offset],
        &after_open[close_offset + "\n---\n".len()..],
    )
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

    #[test]
    fn lists_expected_initial_corpus() {
        let skills = list_embedded_skills();
        assert!(skills.len() >= 7);
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
    fn embedded_corpus_stays_within_binary_budget() {
        let bytes: usize = SOURCES.iter().map(|source| source.len()).sum();
        assert!(
            bytes <= 200 * 1024,
            "embedded corpus is {bytes} bytes, expected <= 200 KiB"
        );
    }
}
