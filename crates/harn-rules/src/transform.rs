//! The `transform` pipeline: synthesize new metavars from captured ones
//! before `fix` interpolation (ast-grep `transform:`).
//!
//! Each transform reads one `source` metavar and applies exactly one
//! operation — regex `replace`, a `substring` slice, or a case `convert` —
//! binding the result under a new metavar name. In v1 transforms read only
//! the originally-captured metavars (not each other's output).

use regex::Regex;

use crate::error::RulesError;
use crate::model::{ConvertOp, Transform};

/// A compiled transform: a source metavar plus one operation.
pub struct CompiledTransform {
    /// The source metavar name (without `$`).
    pub source: String,
    op: Op,
}

enum Op {
    Replace {
        regex: Regex,
        by: String,
    },
    Substring {
        start: Option<i64>,
        end: Option<i64>,
    },
    Convert(ConvertOp),
}

impl CompiledTransform {
    /// Compile a transform, enforcing the exactly-one-operation rule.
    pub fn compile(rule_id: &str, name: &str, t: &Transform) -> Result<Self, RulesError> {
        let set = [
            t.replace.is_some(),
            t.substring.is_some(),
            t.convert.is_some(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count();
        if set != 1 {
            return Err(RulesError::PatternCompile {
                rule: rule_id.to_string(),
                message: format!(
                    "transform `{name}` must set exactly one of `replace` / `substring` / `convert`"
                ),
            });
        }

        let op = if let Some(r) = &t.replace {
            Op::Replace {
                regex: Regex::new(&r.regex).map_err(|err| RulesError::PatternCompile {
                    rule: rule_id.to_string(),
                    message: format!("transform `{name}`: invalid regex `{}`: {err}", r.regex),
                })?,
                by: r.by.clone(),
            }
        } else if let Some(s) = &t.substring {
            Op::Substring {
                start: s.start,
                end: s.end,
            }
        } else {
            Op::Convert(t.convert.expect("convert is set"))
        };

        Ok(CompiledTransform {
            source: t.source.clone(),
            op,
        })
    }

    /// Apply the transform to the source metavar's text.
    pub fn apply(&self, input: &str) -> String {
        match &self.op {
            Op::Replace { regex, by } => regex.replace_all(input, by.as_str()).into_owned(),
            Op::Substring { start, end } => slice_chars(input, *start, *end),
            Op::Convert(convert) => convert_case(input, *convert),
        }
    }
}

/// Slice `input` by 0-based char indices. A negative index counts from the
/// end; out-of-range bounds clamp.
fn slice_chars(input: &str, start: Option<i64>, end: Option<i64>) -> String {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len() as i64;
    let resolve = |idx: i64| -> usize {
        let resolved = if idx < 0 { len + idx } else { idx };
        resolved.clamp(0, len) as usize
    };
    let s = resolve(start.unwrap_or(0));
    let e = resolve(end.unwrap_or(len));
    if s >= e {
        return String::new();
    }
    chars[s..e].iter().collect()
}

/// Split an identifier into lowercase words, honoring `_` / `-` / space
/// separators and camelCase / digit boundaries.
fn split_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut prev: Option<char> = None;
    for ch in input.chars() {
        if ch == '_' || ch == '-' || ch == ' ' || ch == '/' || ch == '.' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev = None;
            continue;
        }
        // A lower→upper transition (`fooBar`) or letter→digit starts a word.
        if let Some(p) = prev {
            let boundary = (p.is_lowercase() && ch.is_uppercase())
                || (p.is_alphabetic() && ch.is_ascii_digit())
                || (p.is_ascii_digit() && ch.is_alphabetic());
            if boundary && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(ch.to_ascii_lowercase());
        prev = Some(ch);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn convert_case(input: &str, convert: ConvertOp) -> String {
    match convert {
        ConvertOp::Lower => input.to_lowercase(),
        ConvertOp::Upper => input.to_uppercase(),
        ConvertOp::Snake => split_words(input).join("_"),
        ConvertOp::ScreamingSnake => split_words(input)
            .iter()
            .map(|w| w.to_uppercase())
            .collect::<Vec<_>>()
            .join("_"),
        ConvertOp::Kebab => split_words(input).join("-"),
        ConvertOp::UpperCamel => split_words(input).iter().map(|w| capitalize(w)).collect(),
        ConvertOp::LowerCamel => {
            let words = split_words(input);
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i == 0 {
                    out.push_str(w);
                } else {
                    out.push_str(&capitalize(w));
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReplaceOp, SubstringOp};

    fn convert(input: &str, op: ConvertOp) -> String {
        let t = Transform {
            source: "X".into(),
            replace: None,
            substring: None,
            convert: Some(op),
        };
        CompiledTransform::compile("r", "out", &t)
            .unwrap()
            .apply(input)
    }

    #[test]
    fn case_conversions() {
        assert_eq!(convert("user_id", ConvertOp::LowerCamel), "userId");
        assert_eq!(convert("userId", ConvertOp::Snake), "user_id");
        assert_eq!(convert("user-id", ConvertOp::UpperCamel), "UserId");
        assert_eq!(convert("userId", ConvertOp::ScreamingSnake), "USER_ID");
        assert_eq!(convert("userId", ConvertOp::Kebab), "user-id");
        assert_eq!(convert("FooBar", ConvertOp::Lower), "foobar");
    }

    #[test]
    fn regex_replace() {
        let t = Transform {
            source: "X".into(),
            replace: Some(ReplaceOp {
                regex: "Controller$".into(),
                by: String::new(),
            }),
            substring: None,
            convert: None,
        };
        let c = CompiledTransform::compile("r", "out", &t).unwrap();
        assert_eq!(c.apply("UserController"), "User");
    }

    #[test]
    fn substring_slice() {
        let t = Transform {
            source: "X".into(),
            replace: None,
            substring: Some(SubstringOp {
                start: Some(0),
                end: Some(3),
            }),
            convert: None,
        };
        let c = CompiledTransform::compile("r", "out", &t).unwrap();
        assert_eq!(c.apply("abcdef"), "abc");
    }

    #[test]
    fn substring_negative_end() {
        let t = Transform {
            source: "X".into(),
            replace: None,
            substring: Some(SubstringOp {
                start: None,
                end: Some(-1),
            }),
            convert: None,
        };
        let c = CompiledTransform::compile("r", "out", &t).unwrap();
        assert_eq!(c.apply("hello"), "hell");
    }

    #[test]
    fn rejects_zero_or_multiple_ops() {
        let none = Transform {
            source: "X".into(),
            replace: None,
            substring: None,
            convert: None,
        };
        assert!(CompiledTransform::compile("r", "out", &none).is_err());
        let two = Transform {
            source: "X".into(),
            replace: Some(ReplaceOp {
                regex: "a".into(),
                by: "b".into(),
            }),
            substring: None,
            convert: Some(ConvertOp::Lower),
        };
        assert!(CompiledTransform::compile("r", "out", &two).is_err());
    }
}
