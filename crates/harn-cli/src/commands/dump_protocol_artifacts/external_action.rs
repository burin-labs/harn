use std::collections::BTreeSet;

use harn_parser::{parse_source, Node, TypeExpr};

use super::support::ProtocolArtifactSource;

pub(super) const EXTERNAL_ACTION_VOCABULARY_SOURCE: &str =
    "crates/harn-stdlib/src/stdlib/external_action/vocabulary.harn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalActionVocabulary {
    pub(super) outcomes: Vec<String>,
    pub(super) receipt_statuses: Vec<String>,
    pub(super) next_actions: Vec<String>,
    pub(super) protected_field_classes: Vec<String>,
}

impl ExternalActionVocabulary {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let text = source.read_text(EXTERNAL_ACTION_VOCABULARY_SOURCE)?;
        Self::parse(&text)
    }

    fn parse(source: &str) -> Result<Self, String> {
        let program = parse_source(source).map_err(|error| {
            format!("failed to parse {EXTERNAL_ACTION_VOCABULARY_SOURCE}: {error}")
        })?;
        Ok(Self {
            outcomes: literal_union(&program, "ExternalActionOutcome")?,
            receipt_statuses: literal_union(&program, "ExternalActionReceiptStatus")?,
            next_actions: literal_union(&program, "ExternalActionNextAction")?,
            protected_field_classes: literal_union(&program, "ExternalActionProtectedFieldClass")?,
        })
    }

    #[cfg(test)]
    pub(super) fn load_for_tests() -> Self {
        let source =
            ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("harn-cli is compiled from the Harn workspace");
        Self::load(&source).expect("external-action vocabulary is valid")
    }
}

fn literal_union(program: &[harn_parser::SNode], type_name: &str) -> Result<Vec<String>, String> {
    let type_expr = program.iter().find_map(|node| match &node.node {
        Node::TypeDecl {
            name, type_expr, ..
        } if name == type_name => Some(type_expr),
        _ => None,
    });
    let Some(type_expr) = type_expr else {
        return Err(format!(
            "{EXTERNAL_ACTION_VOCABULARY_SOURCE} must declare `pub type {type_name}`"
        ));
    };

    let values: Vec<String> = match type_expr {
        TypeExpr::LitString(value) => vec![value.clone()],
        TypeExpr::Union(members) => members
            .iter()
            .map(|member| match member {
                TypeExpr::LitString(value) => Ok(value.clone()),
                other => Err(format!(
                    "{type_name} must contain only string literals, found {other:?}"
                )),
            })
            .collect::<Result<_, _>>()?,
        other => {
            return Err(format!(
                "{type_name} must be a closed string-literal union, found {other:?}"
            ));
        }
    };
    if values.is_empty() {
        return Err(format!("{type_name} must contain at least one value"));
    }
    let mut seen = BTreeSet::new();
    for value in &values {
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        {
            return Err(format!(
                "{type_name} value `{value}` must be non-empty lower snake case"
            ));
        }
        if !seen.insert(value) {
            return Err(format!("{type_name} repeats `{value}`"));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_closed_literal_unions_in_source_order() {
        let vocabulary = ExternalActionVocabulary::parse(
            r#"
pub type ExternalActionOutcome = "first" | "second_value"
pub type ExternalActionReceiptStatus = "ready"
pub type ExternalActionNextAction = "none" | "continue"
pub type ExternalActionProtectedFieldClass = "legal_identity"
"#,
        )
        .unwrap();
        assert_eq!(vocabulary.outcomes, ["first", "second_value"]);
        assert_eq!(vocabulary.receipt_statuses, ["ready"]);
        assert_eq!(vocabulary.next_actions, ["none", "continue"]);
        assert_eq!(vocabulary.protected_field_classes, ["legal_identity"]);
    }

    #[test]
    fn rejects_open_vocabulary() {
        let open = ExternalActionVocabulary::parse(
            r#"
pub type ExternalActionOutcome = string
pub type ExternalActionReceiptStatus = "ready"
pub type ExternalActionNextAction = "none"
pub type ExternalActionProtectedFieldClass = "legal_identity"
"#,
        )
        .unwrap_err();
        assert!(open.contains("closed string-literal union"));
    }
}
