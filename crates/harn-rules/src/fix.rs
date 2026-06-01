//! `fix` template interpolation and application.
//!
//! A `fix` is a replacement template that interpolates the match's
//! metavars — both the captured `$VAR`s and any `transform`-synthesized
//! ones — into replacement text. The engine computes one replacement per
//! match and splices them into the source (format-preserving byte-splice,
//! the same guarantee as `ast.batch_apply`).

use std::collections::BTreeMap;

use crate::engine::Span;

/// Interpolate `$VAR` / `${VAR}` references in `template` from `vars`.
/// An unknown reference is left verbatim (so a literal `$` survives), and
/// `$$` is an escaped literal dollar sign.
pub fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            let ch = template[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // `$$` -> literal `$`.
        if template[i..].starts_with("$$") {
            out.push('$');
            i += 2;
            continue;
        }
        // `${NAME}` braced form.
        let (name, consumed) = if template[i..].starts_with("${") {
            match template[i + 2..].find('}') {
                Some(rel) => (&template[i + 2..i + 2 + rel], 2 + rel + 1),
                None => {
                    out.push('$');
                    i += 1;
                    continue;
                }
            }
        } else {
            // `$NAME` bare form.
            let name_start = i + 1;
            let mut j = name_start;
            if j < bytes.len() && is_ident_start(bytes[j]) {
                j += 1;
                while j < bytes.len() && is_ident_continue(bytes[j]) {
                    j += 1;
                }
            }
            (&template[name_start..j], j - i)
        };

        if name.is_empty() {
            out.push('$');
            i += 1;
            continue;
        }
        match vars.get(name) {
            Some(value) => out.push_str(value),
            // Unknown metavar: keep the reference literal.
            None => out.push_str(&template[i..i + consumed]),
        }
        i += consumed;
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// One concrete edit: replace `span`'s bytes (`before`) with `replacement`.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    /// The replaced span.
    pub span: Span,
    /// The original text at the span.
    pub before: String,
    /// The interpolated replacement text.
    pub replacement: String,
}

/// Apply `edits` to `source` by byte-splice. Edits are spliced in reverse
/// start order so earlier offsets stay valid; whitespace outside each span
/// is preserved verbatim.
pub fn splice(source: &str, edits: &[AppliedEdit]) -> String {
    let mut ordered: Vec<&AppliedEdit> = edits.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.span.start_byte));
    let mut out = source.to_string();
    for edit in ordered {
        out.replace_range(edit.span.start_byte..edit.span.end_byte, &edit.replacement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn interpolates_bare_and_braced() {
        let v = vars(&[("KEY", "userId"), ("NAME", "id")]);
        assert_eq!(interpolate("{ $KEY: $NAME }", &v), "{ userId: id }");
        assert_eq!(interpolate("${KEY}_${NAME}", &v), "userId_id");
    }

    #[test]
    fn unknown_metavar_left_literal() {
        let v = vars(&[("KEY", "x")]);
        assert_eq!(interpolate("$KEY $UNKNOWN", &v), "x $UNKNOWN");
    }

    #[test]
    fn escaped_dollar() {
        let v = vars(&[]);
        assert_eq!(interpolate("price is $$5", &v), "price is $5");
    }

    #[test]
    fn splices_in_reverse_order() {
        let source = "aaa bbb ccc";
        let edits = vec![
            AppliedEdit {
                span: Span {
                    start_byte: 0,
                    end_byte: 3,
                    start_row: 0,
                    start_col: 0,
                    end_row: 0,
                    end_col: 3,
                },
                before: "aaa".into(),
                replacement: "X".into(),
            },
            AppliedEdit {
                span: Span {
                    start_byte: 8,
                    end_byte: 11,
                    start_row: 0,
                    start_col: 8,
                    end_row: 0,
                    end_col: 11,
                },
                before: "ccc".into(),
                replacement: "ZZZZ".into(),
            },
        ];
        assert_eq!(splice(source, &edits), "X bbb ZZZZ");
    }
}
