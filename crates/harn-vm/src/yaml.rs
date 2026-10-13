//! Harn's YAML codec.
//!
//! Every YAML read and write in the workspace goes through this module, so the
//! parser and the scalar policy below have one owner. It wraps `serde-saphyr`,
//! a safe-Rust parser with built-in limits on nesting, aliases, and document
//! size. Untyped YAML decodes into [`serde_json::Value`]; mapping keys become
//! strings, so `123: x` reads as `{"123": "x"}`.
//!
//! The policy:
//!
//! - Only `true` and `false` are booleans. YAML 1.1 spellings such as `yes`,
//!   `on`, `y`, and `n` stay strings, as they did before the parser swap.
//! - `.inf`, `-.inf`, and `.nan` in untyped data decode as the strings
//!   `".inf"`, `"-.inf"`, and `".nan"`. JSON-shaped values cannot hold them.
//! - Nesting is capped at [`MAX_YAML_DEPTH`].
//! - Merge keys (`<<: *anchor`) are expanded, and duplicate keys are an error.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Deepest mapping or sequence nesting a YAML document may use.
pub const MAX_YAML_DEPTH: usize = 128;

/// A YAML document that failed to decode or a value that failed to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    message: String,
    location: Option<(usize, usize)>,
}

impl YamlError {
    /// One-based `(line, column)` of a decode failure, when the parser knows it.
    pub fn location(&self) -> Option<(usize, usize)> {
        self.location
    }

    fn decode(error: serde_saphyr::Error) -> Self {
        let location = error
            .location()
            .filter(|location| location.line() > 0)
            .map(|location| (location.line() as usize, location.column() as usize));
        // The user formatter leaves out the parser's Rust-API advice, and the
        // rustc-style source snippet would bury the one-line cause in thrown
        // Harn values and CLI output.
        let mut render = serde_saphyr::RenderOptions::new(&serde_saphyr::UserMessageFormatter);
        render.snippets = serde_saphyr::SnippetMode::Off;
        Self {
            message: error.render_with_options(render),
            location,
        }
    }

    fn encode(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            location: None,
        }
    }
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for YamlError {}

/// Parser options for one document of `input_len` bytes.
///
/// The parser's default node, event, scalar, and comment budgets are fixed
/// counts that ordinary large documents exceed, such as a long comment block
/// in a workflow or an 80 MB relocation map. Each is raised to what the input
/// itself can hold: a document without aliases cannot produce more of them
/// than it has bytes. Alias expansion beyond the input's size stays bounded by
/// these limits, and the depth, alias, anchor, and merge-key budgets are
/// unchanged.
fn options(input_len: usize) -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_depth = MAX_YAML_DEPTH;
    budget.max_buffered_comment_events = budget.max_buffered_comment_events.max(input_len);
    budget.max_nodes = budget.max_nodes.max(input_len);
    budget.max_events = budget.max_events.max(input_len.saturating_mul(2));
    budget.max_total_scalar_bytes = budget.max_total_scalar_bytes.max(input_len);
    budget.max_total_comment_bytes = budget.max_total_comment_bytes.max(input_len);
    serde_saphyr::options! {
        budget: Some(budget),
        strict_booleans: true,
        reject_non_finite_typeless_float: false,
    }
}

/// Decode one YAML document into `T`.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, YamlError> {
    serde_saphyr::from_str_with_options(text, options(text.len())).map_err(YamlError::decode)
}

/// Decode one UTF-8 YAML document into `T`.
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, YamlError> {
    serde_saphyr::from_slice_with_options(bytes, options(bytes.len())).map_err(YamlError::decode)
}

/// Encode `value` as a YAML document.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, YamlError> {
    serde_saphyr::to_string(value).map_err(YamlError::encode)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn parse(text: &str) -> Value {
        from_str(text).expect("valid YAML")
    }

    #[test]
    fn yaml_1_1_boolean_spellings_stay_strings() {
        assert_eq!(
            parse("a: yes\nb: no\nc: on\nd: off\ne: y\nf: n\ng: true\nh: false\nn: n\n"),
            json!({"a": "yes", "b": "no", "c": "on", "d": "off", "e": "y", "f": "n",
                   "g": true, "h": false, "n": "n"})
        );
    }

    #[test]
    fn scalar_typing_follows_the_documented_policy() {
        assert_eq!(
            parse(
                "int: 37\nhex: 0x1F\noct: 0o17\nexp: 1e3\nfloat: 1.0\nver: 3.0.1\n\
                   date: 2001-12-14\nnothing: ~\nstr: !!str 123\n"
            ),
            json!({"int": 37, "hex": 31, "oct": 15, "exp": 1000.0, "float": 1.0,
                   "ver": "3.0.1", "date": "2001-12-14", "nothing": null, "str": "123"})
        );
        // Non-finite floats have no JSON form, so they keep their spelling.
        assert_eq!(
            parse("a: .inf\nb: -.inf\nc: .nan\n"),
            json!({"a": ".inf", "b": "-.inf", "c": ".nan"})
        );
        // A plain scalar with a leading zero is a number under YAML 1.2, so a
        // value that must keep its digits has to be quoted.
        assert_eq!(parse("zip: 02134\n"), json!({"zip": 2134.0}));
        assert_eq!(parse("zip: '02134'\n"), json!({"zip": "02134"}));
    }

    #[test]
    fn mapping_keys_decode_as_strings() {
        assert_eq!(
            parse("123: a\ntrue: b\non: c\n"),
            json!({"123": "a", "true": "b", "on": "c"})
        );
    }

    #[test]
    fn anchors_and_merge_keys_expand() {
        assert_eq!(
            parse("base: &b {x: 1, y: 2}\nalias: *b\nmerged:\n  <<: *b\n  y: 3\n"),
            json!({"base": {"x": 1, "y": 2}, "alias": {"x": 1, "y": 2},
                   "merged": {"x": 1, "y": 3}})
        );
    }

    #[test]
    fn malformed_documents_are_errors() {
        for text in [
            "a: [\n",
            "a: 1\na: 2\n",
            "a: 1\n---\nb: 2\n",
            "a:\n\tb: 1\n",
        ] {
            assert!(from_str::<Value>(text).is_err(), "{text:?} must not decode");
        }
    }

    #[test]
    fn decode_errors_name_the_cause_and_place_without_parser_advice() {
        let error = from_str::<Value>("a: 1\na: 2\n").expect_err("duplicate key");
        assert_eq!(error.location(), Some((2, 1)));
        let message = error.to_string();
        assert!(message.contains("duplicate"), "{message}");
        for advice in ["DuplicateKeyPolicy", "Options", "from_multiple", "Option<"] {
            assert!(!message.contains(advice), "{advice} leaked into: {message}");
        }
        let multi = from_str::<Value>("a: 1\n---\nb: 2\n").expect_err("two documents");
        assert!(!multi.to_string().contains("from_multiple"), "{multi}");
    }

    #[test]
    fn nesting_past_the_depth_cap_is_an_error() {
        let nested = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        assert!(from_str::<Value>(&nested(MAX_YAML_DEPTH - 1)).is_ok());
        assert!(from_str::<Value>(&nested(MAX_YAML_DEPTH + 1)).is_err());
    }

    #[test]
    fn long_comment_blocks_and_large_documents_decode() {
        // A workflow-style block of 40 comment lines before an entry exceeded
        // the parser's default buffer of 32 comment events (#9824).
        let comments = format!("jobs:\n{}  build: 1\n", "  # note\n".repeat(40));
        assert_eq!(
            from_str::<Value>(&comments).expect("decodes"),
            json!({"jobs": {"build": 1}})
        );
        // 300,000 sequence entries exceed the default 250,000-node budget but
        // need no expansion, as in a large relocation map.
        let entries = 300_000;
        let large = "- 1\n".repeat(entries);
        let decoded = from_slice::<Value>(large.as_bytes()).expect("decodes");
        assert_eq!(decoded.as_array().map(Vec::len), Some(entries));
    }

    #[test]
    fn alias_expansion_bombs_are_rejected() {
        let bomb = |levels: usize| {
            let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
            for level in 1..levels {
                let prev = level - 1;
                let refs = vec![format!("*a{prev}"); 10].join(", ");
                text.push_str(&format!("a{level}: &a{level} [{refs}]\n"));
            }
            text
        };
        // The same shape at a harmless size decodes, so the rejection below is
        // the expansion budget and not a malformed document.
        assert!(from_str::<Value>(&bomb(2)).is_ok());
        assert!(from_str::<Value>(&bomb(10)).is_err());
    }

    #[test]
    fn encoded_strings_that_look_like_other_scalars_are_quoted() {
        let value = json!({"a": "yes", "b": "true", "c": "123", "d": "", "e": "x: y", "f": "~"});
        let text = to_string(&value).expect("encodes");
        for line in [
            "a: \"yes\"",
            "b: \"true\"",
            "c: \"123\"",
            "d: \"\"",
            "e: \"x: y\"",
            "f: \"~\"",
        ] {
            assert!(text.contains(line), "{line:?} missing from:\n{text}");
        }
        assert_eq!(from_str::<Value>(&text).expect("round-trips"), value);
    }
}
