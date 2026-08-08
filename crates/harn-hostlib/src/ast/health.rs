//! One parser provenance, fitness, and coverage decision for every AST caller.
//!
//! The checked-in receipt is generated from the resolved Cargo graph and the
//! exact versioned corpus bytes. Runtime callers add only the selected grammar
//! ABI and the observation for their source; policy is derived here once.

use std::sync::{Arc, OnceLock};

use harn_vm::VmValue;
use serde::Deserialize;

use super::language::Language;

const RECEIPT_JSON: &str = include_str!("../../data/grammar-fitness/receipt.v1.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The structural operation whose parser evidence is being reported.
pub enum ParserOperation {
    /// Parse diagnostics or a syntax tree.
    Parse,
    /// Read-only structural search or analysis.
    Search,
    /// A structural mutation that must fail closed.
    SafeEdit,
}

impl ParserOperation {
    /// Stable wire name used by schemas and receipts.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Search => "search",
            Self::SafeEdit => "safe_edit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What the selected grammar observed in the caller's source.
pub enum SourceObservation {
    /// The produced syntax tree contained no error node.
    Clean,
    /// The produced syntax tree contained at least one error node.
    Errors,
    /// No source parse was attempted for this response.
    NotObserved,
}

impl SourceObservation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Errors => "errors",
            Self::NotObserved => "not_observed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Truthfulness level callers may rely on.
pub enum Coverage {
    /// Artifact fitness passed and any required source observation is authoritative.
    Verified,
    /// Read-only results may be useful but the source contains parser errors.
    Partial,
    /// No supported, fitted parser artifact was available.
    Unavailable,
    /// The parser saw errors and no independent authority resolved their cause.
    Inconclusive,
}

impl Coverage {
    /// Stable wire name used by schemas.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Clone)]
/// One authoritative parser provenance and coverage decision.
pub struct ParserHealth {
    operation: ParserOperation,
    language: Option<Language>,
    observation: SourceObservation,
    source_authority: Option<String>,
    coverage: Coverage,
    support: &'static str,
    receipt: Option<&'static LanguageReceipt>,
    abi_version: Option<usize>,
}

impl ParserHealth {
    /// Build health from an actual source parse.
    pub fn observed(language: Language, operation: ParserOperation, had_errors: bool) -> Self {
        let observation = if had_errors {
            SourceObservation::Errors
        } else {
            SourceObservation::Clean
        };
        Self::new(Some(language), operation, observation)
    }

    /// Build health for a supported operation that did not inspect source.
    pub fn not_observed(language: Language, operation: ParserOperation) -> Self {
        Self::new(Some(language), operation, SourceObservation::NotObserved)
    }

    /// Report artifact/corpus fitness for capability discovery without making
    /// a claim about caller source.
    pub fn fitted(language: Language, operation: ParserOperation) -> Self {
        let mut health = Self::new(Some(language), operation, SourceObservation::NotObserved);
        if health.support == "supported" {
            health.coverage = Coverage::Verified;
        }
        health
    }

    /// Build an explicit unavailable result without implying a clean parse.
    pub fn unavailable(language: Option<Language>, operation: ParserOperation) -> Self {
        let receipt = language.and_then(receipt_for);
        let ts_language = language.and_then(Language::ts_language);
        let fitted = receipt.is_some_and(|row| {
            row.operations
                .iter()
                .any(|candidate| candidate == operation.as_str())
        });
        Self {
            operation,
            language,
            observation: SourceObservation::NotObserved,
            source_authority: None,
            coverage: Coverage::Unavailable,
            support: if ts_language.is_some() && fitted {
                "supported"
            } else {
                "unsupported"
            },
            receipt,
            abi_version: ts_language.as_ref().map(tree_sitter::Language::abi_version),
        }
    }

    fn new(
        language: Option<Language>,
        operation: ParserOperation,
        observation: SourceObservation,
    ) -> Self {
        let receipt = language.and_then(receipt_for);
        let ts_language = language.and_then(Language::ts_language);
        let fitted = receipt.is_some_and(|row| {
            row.operations
                .iter()
                .any(|candidate| candidate == operation.as_str())
        });
        let supported = ts_language.is_some() && fitted;
        let coverage = if !supported {
            Coverage::Unavailable
        } else {
            match (operation, observation) {
                (_, SourceObservation::Clean) => Coverage::Verified,
                (_, SourceObservation::NotObserved) => Coverage::Inconclusive,
                (ParserOperation::Search, SourceObservation::Errors) => Coverage::Partial,
                (ParserOperation::Parse | ParserOperation::SafeEdit, SourceObservation::Errors) => {
                    Coverage::Inconclusive
                }
            }
        };
        Self {
            operation,
            language,
            observation,
            source_authority: (!matches!(observation, SourceObservation::NotObserved))
                .then(|| "tree_sitter".to_string()),
            coverage,
            support: if supported {
                "supported"
            } else {
                "unsupported"
            },
            receipt,
            abi_version: ts_language.as_ref().map(tree_sitter::Language::abi_version),
        }
    }

    /// Return the derived coverage state.
    pub const fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Whether a structural write may rely on this health result.
    pub const fn is_verified(&self) -> bool {
        matches!(self.coverage, Coverage::Verified)
    }

    /// Record an independent configured checker that verified source rejected
    /// by Tree-sitter. Callers must name the authority; span shape is never an
    /// authority.
    pub fn externally_verified(
        language: Language,
        operation: ParserOperation,
        authority: impl Into<String>,
    ) -> Self {
        let mut health = Self::new(Some(language), operation, SourceObservation::Errors);
        if health.support == "supported" {
            health.coverage = Coverage::Verified;
            health.source_authority = Some(authority.into());
        }
        health
    }

    pub(crate) fn attach_to(&self, response: VmValue) -> VmValue {
        match response {
            VmValue::Dict(map) => {
                let mut fields = (*map).clone();
                fields.insert("health".into(), self.to_vm_value());
                VmValue::dict(fields)
            }
            other => other,
        }
    }

    /// Serialize the versioned public health contract.
    pub fn to_vm_value(&self) -> VmValue {
        let receipt = fitness_receipt();
        let grammar = self.receipt.map_or_else(
            || {
                dict([
                    (
                        "language",
                        self.language
                            .map_or(VmValue::Nil, |language| VmValue::string(language.name())),
                    ),
                    ("package", VmValue::Nil),
                    (
                        "runtime_version",
                        VmValue::string(&receipt.tree_sitter_runtime),
                    ),
                    ("abi_version", VmValue::Nil),
                    ("enabled_features", VmValue::List(Arc::new(Vec::new()))),
                ])
            },
            |language_receipt| {
                let package = &language_receipt.package;
                dict([
                    ("language", VmValue::string(&language_receipt.language)),
                    (
                        "package",
                        dict([
                            ("name", VmValue::string(&package.name)),
                            ("version", VmValue::string(&package.version)),
                            ("source", VmValue::string(&package.source)),
                            (
                                "checksum",
                                package
                                    .checksum
                                    .as_deref()
                                    .map_or(VmValue::Nil, VmValue::string),
                            ),
                            ("artifact_digest", VmValue::string(&package.artifact_digest)),
                        ]),
                    ),
                    (
                        "runtime_version",
                        VmValue::string(&receipt.tree_sitter_runtime),
                    ),
                    (
                        "abi_version",
                        self.abi_version
                            .map_or(VmValue::Nil, |abi| VmValue::Int(abi as i64)),
                    ),
                    (
                        "enabled_features",
                        VmValue::List(Arc::new(vec![VmValue::string(grammar_feature(
                            self.language.expect("receipt has language"),
                        ))])),
                    ),
                ])
            },
        );
        let corpus_fitness = if self.receipt.is_some_and(|row| {
            row.operations
                .iter()
                .any(|candidate| candidate == self.operation.as_str())
        }) {
            "verified"
        } else {
            "unavailable"
        };
        dict([
            ("schema_version", VmValue::Int(receipt.schema_version)),
            ("operation", VmValue::string(self.operation.as_str())),
            ("grammar", grammar),
            (
                "corpus",
                dict([
                    (
                        "schema_version",
                        VmValue::Int(receipt.corpus.schema_version),
                    ),
                    ("digest", VmValue::string(&receipt.corpus.digest)),
                    ("fitness", VmValue::string(corpus_fitness)),
                    (
                        "authority",
                        self.receipt
                            .map_or(VmValue::Nil, |row| VmValue::string(&row.authority)),
                    ),
                ]),
            ),
            (
                "source",
                dict([
                    ("observation", VmValue::string(self.observation.as_str())),
                    (
                        "authority",
                        self.source_authority
                            .as_deref()
                            .map_or(VmValue::Nil, VmValue::string),
                    ),
                ]),
            ),
            ("support", VmValue::string(self.support)),
            ("coverage", VmValue::string(self.coverage.as_str())),
        ])
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitnessReceipt {
    schema_version: i64,
    corpus: CorpusReceipt,
    tree_sitter_runtime: String,
    languages: Vec<LanguageReceipt>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusReceipt {
    schema_version: i64,
    digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LanguageReceipt {
    language: String,
    package: PackageReceipt,
    operations: Vec<String>,
    authority: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageReceipt {
    name: String,
    version: String,
    source: String,
    checksum: Option<String>,
    artifact_digest: String,
}

fn fitness_receipt() -> &'static FitnessReceipt {
    static RECEIPT: OnceLock<FitnessReceipt> = OnceLock::new();
    RECEIPT.get_or_init(|| {
        let receipt: FitnessReceipt =
            serde_json::from_str(RECEIPT_JSON).expect("grammar fitness receipt must be valid");
        assert_eq!(receipt.schema_version, 1, "unsupported fitness receipt");
        assert!(
            !receipt.tree_sitter_runtime.is_empty(),
            "fitness receipt must identify the tree-sitter runtime"
        );
        receipt
    })
}

fn receipt_for(language: Language) -> Option<&'static LanguageReceipt> {
    fitness_receipt()
        .languages
        .iter()
        .find(|row| row.language == language.name())
}

const fn grammar_feature(language: Language) -> &'static str {
    match language {
        Language::Harn => "grammar-harn",
        Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx
        | Language::Html
        | Language::Css => "grammar-web",
        Language::Rust | Language::C | Language::Cpp | Language::Go | Language::Zig => {
            "grammar-systems"
        }
        Language::Python
        | Language::Ruby
        | Language::Bash
        | Language::Lua
        | Language::Php
        | Language::R => "grammar-scripting",
        Language::Java | Language::Kotlin | Language::Scala => "grammar-jvm",
        Language::CSharp | Language::Swift | Language::Elixir | Language::Haskell => {
            "grammar-enterprise"
        }
        Language::Json | Language::Yaml | Language::Toml | Language::Sql | Language::Markdown => {
            "grammar-data"
        }
    }
}

fn dict<const N: usize>(entries: [(&str, VmValue); N]) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<harn_vm::value::DictMap>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_covers_every_registered_language_and_operation() {
        for language in Language::all() {
            let receipt = receipt_for(*language)
                .unwrap_or_else(|| panic!("missing receipt for {}", language.name()));
            for operation in [
                ParserOperation::Parse,
                ParserOperation::Search,
                ParserOperation::SafeEdit,
            ] {
                assert!(
                    receipt
                        .operations
                        .iter()
                        .any(|candidate| candidate == operation.as_str()),
                    "{} lacks {} fitness",
                    language.name(),
                    operation.as_str()
                );
            }
        }
    }

    #[test]
    fn coverage_policy_is_operation_specific() {
        let parse = ParserHealth::observed(Language::Swift, ParserOperation::Parse, true);
        let search = ParserHealth::observed(Language::Swift, ParserOperation::Search, true);
        let edit = ParserHealth::observed(Language::Swift, ParserOperation::SafeEdit, false);
        assert_eq!(parse.coverage(), Coverage::Inconclusive);
        assert_eq!(search.coverage(), Coverage::Partial);
        assert_eq!(edit.coverage(), Coverage::Verified);
        let VmValue::Dict(parse_health) = parse.to_vm_value() else {
            panic!("health dict")
        };
        let Some(VmValue::Dict(source)) = parse_health.get("source") else {
            panic!("source dict")
        };
        assert!(matches!(
            source.get("authority"),
            Some(VmValue::String(authority)) if authority.as_str() == "tree_sitter"
        ));
    }

    #[test]
    fn only_a_named_external_authority_can_verify_a_parser_limitation() {
        let tree_sitter_only =
            ParserHealth::observed(Language::Scala, ParserOperation::SafeEdit, true);
        let externally_verified = ParserHealth::externally_verified(
            Language::Scala,
            ParserOperation::SafeEdit,
            "scalac 3.7.1",
        );
        assert_eq!(tree_sitter_only.coverage(), Coverage::Inconclusive);
        assert_eq!(externally_verified.coverage(), Coverage::Verified);
        let value = externally_verified.to_vm_value();
        let VmValue::Dict(health) = value else {
            panic!("health dict")
        };
        let Some(VmValue::Dict(source)) = health.get("source") else {
            panic!("source dict")
        };
        assert!(matches!(
            source.get("authority"),
            Some(VmValue::String(authority)) if authority.as_str() == "scalac 3.7.1"
        ));
    }
}
