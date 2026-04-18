//! Body substitution for SKILL.md text.
//!
//! Claude-Code skills support a handful of dollar-escapes in the body
//! text: `$ARGUMENTS` (all positional args joined on spaces), `$N`
//! (the N-th positional arg, 1-based), `${HARN_SKILL_DIR}` (absolute
//! path to the skill directory), and `${HARN_SESSION_ID}` (opaque id
//! threaded through the run).
//!
//! We keep this as a focused string-pass (not a tree rewrite) so SKILL
//! authors can rely on exactly these four forms without worrying about
//! collisions with Markdown / Handlebars / Jinja / Harn's own
//! `{{ expr }}` surface. Anything outside the recognized set passes
//! through unchanged.

use std::collections::HashMap;

/// Context passed to [`substitute_skill_body`]. Unset optional fields
/// leave their placeholders intact so downstream template engines or
/// host rendering passes can still resolve them.
#[derive(Debug, Clone, Default)]
pub struct SubstitutionContext {
    pub arguments: Vec<String>,
    pub skill_dir: Option<String>,
    pub session_id: Option<String>,
    /// Additional `${NAME}` replacements provided by the host
    /// (e.g. `${HARN_USER_NAME}`). Keys should be uppercase by
    /// convention.
    pub extra_env: HashMap<String, String>,
}

/// Substitute `$ARGUMENTS`, `$N`, and `${HARN_*}` in a SKILL.md body.
///
/// Rules:
/// - `$ARGUMENTS` expands to `arguments.join(" ")`.
/// - `$1`..`$9` expands to the N-th argument (1-based). `$0` passes
///   through unchanged — reserved.
/// - `${HARN_SKILL_DIR}` / `${HARN_SESSION_ID}` / any other
///   `${NAME}` looks up `extra_env` and falls back to `std::env::var`
///   so hosts can scope through the process environment.
/// - A literal `$$` escapes to `$`. Everything else is untouched.
pub fn substitute_skill_body(body: &str, ctx: &SubstitutionContext) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            // Copy the full UTF-8 codepoint.
            let ch_len = utf8_char_len(b);
            out.push_str(&body[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        // Peek at the next character to decide on the escape form.
        let next = bytes.get(i + 1).copied();
        match next {
            Some(b'$') => {
                out.push('$');
                i += 2;
            }
            Some(b'{') => {
                if let Some(close) = find_ascii(bytes, i + 2, b'}') {
                    let name = &body[i + 2..close];
                    out.push_str(&resolve_named(name, ctx, &body[i..close + 1]));
                    i = close + 1;
                } else {
                    // No closing brace — pass `$` through and keep scanning.
                    out.push('$');
                    i += 1;
                }
            }
            Some(b'A') if body[i..].starts_with("$ARGUMENTS") => {
                out.push_str(&ctx.arguments.join(" "));
                i += "$ARGUMENTS".len();
            }
            Some(b) if b.is_ascii_digit() => {
                // `$N` — greedy consume digits. Index is 1-based; $0 passes through.
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                let digits = &body[start..end];
                let idx: usize = digits.parse().unwrap_or(0);
                if idx == 0 {
                    out.push_str(&body[i..end]);
                } else if let Some(arg) = ctx.arguments.get(idx - 1) {
                    out.push_str(arg);
                } else {
                    // Missing argument — leave the placeholder in so the
                    // author sees what wasn't supplied rather than a
                    // silent empty substitution.
                    out.push_str(&body[i..end]);
                }
                i = end;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn resolve_named(name: &str, ctx: &SubstitutionContext, original: &str) -> String {
    match name {
        "HARN_SKILL_DIR" => ctx
            .skill_dir
            .clone()
            .unwrap_or_else(|| original.to_string()),
        "HARN_SESSION_ID" => ctx
            .session_id
            .clone()
            .unwrap_or_else(|| original.to_string()),
        other => ctx
            .extra_env
            .get(other)
            .cloned()
            .or_else(|| std::env::var(other).ok())
            .unwrap_or_else(|| original.to_string()),
    }
}

fn find_ascii(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == target)
        .map(|p| from + p)
}

fn utf8_char_len(first_byte: u8) -> usize {
    // ASCII / continuation bytes (< 0xC0) always advance by one byte; the
    // latter only happens when we're mid-codepoint, which the outer loop
    // never enters, so treating them as 1-byte is safe.
    if first_byte < 0xC0 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_joined_with_spaces() {
        let ctx = SubstitutionContext {
            arguments: vec!["alpha".into(), "beta".into(), "gamma".into()],
            ..Default::default()
        };
        assert_eq!(
            substitute_skill_body("run $ARGUMENTS now", &ctx),
            "run alpha beta gamma now"
        );
    }

    #[test]
    fn positional_args_are_one_based() {
        let ctx = SubstitutionContext {
            arguments: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        };
        assert_eq!(substitute_skill_body("$1 / $2", &ctx), "alpha / beta");
    }

    #[test]
    fn missing_positional_arg_preserves_placeholder() {
        let ctx = SubstitutionContext {
            arguments: vec!["only".into()],
            ..Default::default()
        };
        assert_eq!(substitute_skill_body("$1 / $2", &ctx), "only / $2");
    }

    #[test]
    fn skill_dir_and_session_id() {
        let ctx = SubstitutionContext {
            skill_dir: Some("/tmp/skills/deploy".into()),
            session_id: Some("sess-abc".into()),
            ..Default::default()
        };
        assert_eq!(
            substitute_skill_body("cd ${HARN_SKILL_DIR} && echo ${HARN_SESSION_ID}", &ctx),
            "cd /tmp/skills/deploy && echo sess-abc"
        );
    }

    #[test]
    fn unknown_named_placeholder_looks_up_extra_env() {
        let mut extra = HashMap::new();
        extra.insert("HARN_USER_NAME".to_string(), "kenneth".to_string());
        let ctx = SubstitutionContext {
            extra_env: extra,
            ..Default::default()
        };
        assert_eq!(
            substitute_skill_body("hi ${HARN_USER_NAME}!", &ctx),
            "hi kenneth!"
        );
    }

    #[test]
    fn unknown_named_placeholder_passes_through_when_unset() {
        let ctx = SubstitutionContext::default();
        // Only true when the env var is also unset. This test relies on
        // a name we're confident is never set by CI.
        std::env::remove_var("HARN_CI_UNLIKELY_VAR_NAME_XYZ");
        let body = "value=${HARN_CI_UNLIKELY_VAR_NAME_XYZ}";
        assert_eq!(substitute_skill_body(body, &ctx), body);
    }

    #[test]
    fn dollar_dollar_escapes_to_literal_dollar() {
        let ctx = SubstitutionContext::default();
        assert_eq!(substitute_skill_body("price: $$5", &ctx), "price: $5");
    }

    #[test]
    fn lone_dollar_passes_through() {
        let ctx = SubstitutionContext::default();
        assert_eq!(
            substitute_skill_body("cost is $ then done", &ctx),
            "cost is $ then done"
        );
    }

    #[test]
    fn dollar_zero_is_reserved_and_passes_through() {
        let ctx = SubstitutionContext {
            arguments: vec!["first".into()],
            ..Default::default()
        };
        assert_eq!(substitute_skill_body("$0 -> $1", &ctx), "$0 -> first");
    }

    #[test]
    fn utf8_bodies_are_preserved() {
        let ctx = SubstitutionContext {
            arguments: vec!["🚀".into()],
            ..Default::default()
        };
        assert_eq!(
            substitute_skill_body("emoji → $1 ← end", &ctx),
            "emoji → 🚀 ← end"
        );
    }
}
