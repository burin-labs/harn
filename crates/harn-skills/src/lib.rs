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
    /// The full original SKILL.md source — frontmatter delimiter,
    /// frontmatter block, blank line, and body — exactly as embedded.
    /// Use this when round-tripping a skill back to disk so the dumped
    /// copy is byte-identical to the binary's canonical record.
    pub source: &'static str,
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
        source,
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
        let names: Vec<&str> = skills.iter().map(|skill| skill.name).collect();
        assert_eq!(
            names,
            [
                "harn-agent",
                "harn-diagnostics",
                "harn-language",
                "harn-orchestration",
                "harn-providers",
                "harn-testing",
                "harn-tracing",
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
                skill.source.starts_with("---\n"),
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
            ("harn-providers", ["llm_call", "provider", "schema"]),
            (
                "harn-testing",
                ["conformance", "deterministic", "mock_time"],
            ),
            ("harn-tracing", ["replay", "receipts", "transcript"]),
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
