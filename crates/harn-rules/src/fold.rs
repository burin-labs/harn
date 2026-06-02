//! Fold consecutive destructure-with-defaults runs (#2824).
//!
//! Collapses a run of consecutive `let <name> = <src>?.<key> ?? <default>`
//! statements sharing the same `<src>` into a single destructuring bind:
//!
//! ```text
//!   let timeout = cfg?.timeout ?? 30
//!   let retries = cfg?.retries ?? 3
//! ```
//! becomes
//! ```text
//!   let { timeout = 30, retries = 3 } = cfg ?? {}
//! ```
//!
//! **Behavior-preserving:** `cfg ?? {}` guards a nil source — bare
//! `let { x = d } = nil` throws, whereas `cfg?.x ?? d` yields `d`. Coalescing
//! the source to `{}` first reproduces the `?.`/`??` semantics exactly.
//!
//! Only the **non-alias** case (binding name == property name) is folded, via
//! the engine's metavar unification (`let $K = $X?.$K ?? $D`); aliased sites
//! (`let t = cfg?.timeout ?? d`) are left untouched. Only consecutive lines
//! sharing one source are merged; a blank line, comment, or other statement
//! between two `let`s breaks the run.

use crate::engine::{CompiledRule, RuleMatch};
use crate::error::RulesError;
use crate::model::Rule;

/// The matcher for a single migratable site. `$K` is unified (binding name ==
/// property name), so aliased sites do not match.
fn site_rule(language: &str) -> CompiledRule {
    let toml = format!(
        "id = \"destructure-fold\"\nlanguage = \"{language}\"\n[rule]\npattern = \"let $K = $X?.$K ?? $D\"\n"
    );
    let rule = Rule::from_toml_str(&toml).expect("internal fold rule parses");
    CompiledRule::compile(&rule).expect("internal fold rule compiles")
}

/// One captured site: the binding/property key, default, and source expression.
struct Site {
    key: String,
    default: String,
    source: String,
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
}

impl Site {
    fn from_match(m: &RuleMatch) -> Option<Self> {
        Some(Self {
            key: m.bindings.get("K")?.text.clone(),
            default: m.bindings.get("D")?.text.clone(),
            source: m.bindings.get("X")?.text.clone(),
            start_byte: m.span.start_byte,
            end_byte: m.span.end_byte,
            start_row: m.span.start_row,
        })
    }
}

/// Fold a source string's consecutive same-source `let x = src?.x ?? d` runs of
/// length ≥ 2 into merged destructures. Returns the rewritten source (identical
/// when nothing folds). `language` must name a tree-sitter grammar (e.g.
/// `"harn"`, `"typescript"`).
pub fn fold_destructure_defaults(source: &str, language: &str) -> Result<String, RulesError> {
    let rule = site_rule(language);
    let matches = rule.run(source)?;

    let mut sites: Vec<Site> = matches.iter().filter_map(Site::from_match).collect();
    sites.sort_by_key(|s| s.start_byte);

    // Group consecutive sites: same source, and on the immediately next line.
    let mut groups: Vec<Vec<Site>> = Vec::new();
    for site in sites {
        match groups.last_mut() {
            Some(group)
                if group.last().is_some_and(|prev| {
                    prev.source == site.source && site.start_row == prev.start_row + 1
                }) =>
            {
                group.push(site);
            }
            _ => groups.push(vec![site]),
        }
    }

    // Build replacement edits for runs of length ≥ 2, applied back-to-front so
    // earlier byte offsets stay valid.
    let mut edits: Vec<(usize, usize, String)> = groups
        .into_iter()
        .filter(|group| group.len() >= 2)
        .map(|group| {
            let fields = group
                .iter()
                .map(|s| format!("{} = {}", s.key, s.default))
                .collect::<Vec<_>>()
                .join(", ");
            let replacement = format!("let {{ {fields} }} = {} ?? {{}}", group[0].source);
            (
                group[0].start_byte,
                group[group.len() - 1].end_byte,
                replacement,
            )
        })
        .collect();
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));

    let mut out = source.to_string();
    for (start, end, replacement) in edits {
        out.replace_range(start..end, &replacement);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(src: &str) -> String {
        fold_destructure_defaults(src, "harn").unwrap()
    }

    #[test]
    fn folds_a_consecutive_run() {
        let src =
            "fn f() {\n  let timeout = cfg?.timeout ?? 30\n  let retries = cfg?.retries ?? 3\n}\n";
        let out = fold(src);
        assert_eq!(
            out,
            "fn f() {\n  let { timeout = 30, retries = 3 } = cfg ?? {}\n}\n"
        );
    }

    #[test]
    fn leaves_a_single_site_untouched() {
        // A lone site is not a "run"; folding it would be a lateral change.
        let src = "fn f() {\n  let timeout = cfg?.timeout ?? 30\n}\n";
        assert_eq!(fold(src), src);
    }

    #[test]
    fn does_not_merge_across_different_sources() {
        let src = "fn f() {\n  let a = x?.a ?? 1\n  let b = y?.b ?? 2\n}\n";
        // Two different sources, each a lone site → no fold.
        assert_eq!(fold(src), src);
    }

    #[test]
    fn does_not_merge_across_a_blank_line() {
        let src = "fn f() {\n  let a = x?.a ?? 1\n\n  let b = x?.b ?? 2\n}\n";
        assert_eq!(fold(src), src);
    }

    #[test]
    fn folds_three_and_preserves_surrounding_code() {
        let src = "fn f() {\n  before()\n  let a = s?.a ?? 1\n  let b = s?.b ?? 2\n  let c = s?.c ?? 3\n  after()\n}\n";
        let out = fold(src);
        assert_eq!(
            out,
            "fn f() {\n  before()\n  let { a = 1, b = 2, c = 3 } = s ?? {}\n  after()\n}\n"
        );
    }

    #[test]
    fn aliased_site_is_left_untouched() {
        // Binding `t` != property `timeout`, so `$K` cannot unify → no match.
        let src = "fn f() {\n  let t = cfg?.timeout ?? 30\n  let r = cfg?.retries ?? 3\n}\n";
        assert_eq!(fold(src), src);
    }
}
