//! Error type for the rule engine.

use thiserror::Error;

/// Anything that can go wrong loading, compiling, or running a rule.
#[derive(Debug, Error)]
pub enum RulesError {
    /// A rule file could not be read.
    #[error("read rule `{path}`: {source}")]
    Read {
        /// The path that failed to read.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A rule file could not be parsed as TOML.
    #[error("parse rule `{path}`: {source}")]
    Parse {
        /// The path that failed to parse.
        path: String,
        /// The underlying TOML error.
        source: Box<toml::de::Error>,
    },

    /// A rule referenced a language the engine does not know.
    #[error("rule `{rule}`: unknown language `{language}`")]
    UnknownLanguage {
        /// The offending rule id.
        rule: String,
        /// The language string that did not resolve.
        language: String,
    },

    /// A rule's language is known but its grammar was not compiled into
    /// this build.
    #[error("rule `{rule}`: grammar for `{language}` is not compiled into this build")]
    GrammarUnavailable {
        /// The offending rule id.
        rule: String,
        /// The language whose grammar is missing.
        language: String,
    },

    /// A `pattern` snippet failed to compile into a tree-sitter query.
    #[error("rule `{rule}`: {message}")]
    PatternCompile {
        /// The offending rule id.
        rule: String,
        /// What went wrong compiling the snippet.
        message: String,
    },

    /// The compiled query was rejected by tree-sitter. This is an engine
    /// bug if it happens for a snippet that parsed cleanly, so it carries
    /// the generated query for debugging.
    #[error("rule `{rule}`: generated query rejected by tree-sitter: {message}\nquery:\n{query}")]
    QueryRejected {
        /// The offending rule id.
        rule: String,
        /// The tree-sitter query error.
        message: String,
        /// The generated S-expression query.
        query: String,
    },

    /// Source text could not be parsed in the rule's grammar.
    #[error("rule `{rule}`: parse source: {message}")]
    SourceParse {
        /// The offending rule id.
        rule: String,
        /// The parse failure detail.
        message: String,
    },

    /// `auto_apply` was called on a rule whose `safety` is above the
    /// machine-applicable threshold; it must be applied with explicit opt-in.
    #[error(
        "rule `{rule}`: refusing to auto-apply a `{safety}` fix (machine-applicable required)"
    )]
    NotAutoApplicable {
        /// The offending rule id.
        rule: String,
        /// The rule's declared safety tier.
        safety: String,
    },

    /// A fix did not reach a fixed point: re-running it after applying still
    /// produced changes.
    #[error("rule `{rule}`: fix is not idempotent (re-running it produced further changes)")]
    NotIdempotent {
        /// The offending rule id.
        rule: String,
    },
}
