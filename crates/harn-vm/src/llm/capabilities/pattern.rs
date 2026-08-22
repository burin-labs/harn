use std::collections::HashSet;

use serde::{de, Deserialize, Deserializer};

use super::rule::ProviderRule;
use crate::llm::providers::anthropic::claude_generation;
use crate::llm::providers::openai_compat::gpt_generation;
use harn_glob::match_name as glob_match;

/// Model IDs covered by one capability rule.
#[derive(Debug, Clone)]
pub enum ModelPatterns {
    One(String),
    Many(Vec<String>),
}

impl ModelPatterns {
    pub(super) fn iter(&self) -> impl Iterator<Item = &str> {
        let (one, many) = match self {
            Self::One(pattern) => (Some(pattern.as_str()), &[][..]),
            Self::Many(patterns) => (None, patterns.as_slice()),
        };
        one.into_iter().chain(many.iter().map(String::as_str))
    }
}

impl<'de> Deserialize<'de> for ModelPatterns {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            One(String),
            Many(Vec<String>),
        }

        let patterns = match Repr::deserialize(deserializer)? {
            Repr::One(pattern) => Self::One(pattern),
            Repr::Many(patterns) => Self::Many(patterns),
        };
        if patterns.iter().next().is_none() || patterns.iter().any(str::is_empty) {
            return Err(de::Error::custom(
                "model_match must be a non-empty pattern or list of patterns",
            ));
        }
        let mut seen = HashSet::new();
        if patterns
            .iter()
            .map(str::to_ascii_lowercase)
            .any(|pattern| !seen.insert(pattern))
        {
            return Err(de::Error::custom(
                "model_match patterns must be unique ignoring case",
            ));
        }
        Ok(patterns)
    }
}

pub(super) fn rule_matches(rule: &ProviderRule, model: &str) -> bool {
    let lower = model.to_lowercase();
    if !rule
        .match_patterns()
        .any(|pattern| glob_match(&pattern.to_lowercase(), &lower))
    {
        return false;
    }
    if let Some(version_min) = &rule.version_min {
        if version_min.len() != 2 {
            return false;
        }
        let want = (version_min[0], version_min[1]);
        let Some(have) = extract_version(model) else {
            return false;
        };
        if have < want {
            return false;
        }
    }
    true
}

fn extract_version(model: &str) -> Option<(u32, u32)> {
    claude_generation(model).or_else(|| gpt_generation(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct PatternFixture {
        model_match: ModelPatterns,
    }

    #[test]
    fn model_patterns_accept_one_or_many_and_reject_ambiguous_lists() {
        let one: PatternFixture = toml::from_str("model_match = 'gpt-*'").expect("one pattern");
        assert_eq!(one.model_match.iter().collect::<Vec<_>>(), ["gpt-*"]);

        let many: PatternFixture =
            toml::from_str("model_match = ['gpt-a', 'gpt-b']").expect("many patterns");
        assert_eq!(
            many.model_match.iter().collect::<Vec<_>>(),
            ["gpt-a", "gpt-b"]
        );

        for invalid in [
            "model_match = []",
            "model_match = ['']",
            "model_match = ['GPT-A', 'gpt-a']",
        ] {
            assert!(
                toml::from_str::<PatternFixture>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}
