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
//! Aliased sites (`let t = cfg?.timeout ?? d`) are folded with the Harn dict
//! pattern alias form (`{ timeout: t = d }`). Only consecutive statements
//! sharing one source are merged; a blank line, comment, or other statement
//! between two `let`s breaks the run.

use crate::engine::{CompiledRule, RuleMatch};
use crate::error::RulesError;
use crate::model::Rule;

/// The matcher for a single migratable site.
fn site_rule(language: &str) -> CompiledRule {
    let toml = format!(
        "id = \"destructure-fold\"\nlanguage = \"{language}\"\n[rule]\npattern = \"let $N:identifier = $X?.$K:identifier ?? $D\"\n"
    );
    let rule = Rule::from_toml_str(&toml).expect("internal fold rule parses");
    CompiledRule::compile(&rule).expect("internal fold rule compiles")
}

/// One captured site: the binding name, property key, default, and source.
struct Site {
    binding: String,
    key: String,
    default: String,
    source: String,
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
}

impl Site {
    fn from_match(m: &RuleMatch) -> Option<Self> {
        Some(Self {
            binding: m.bindings.get("N")?.text.clone(),
            key: m.bindings.get("K")?.text.clone(),
            default: m.bindings.get("D")?.text.clone(),
            source: m.bindings.get("X")?.text.clone(),
            start_byte: m.span.start_byte,
            end_byte: m.span.end_byte,
            start_row: m.span.start_row,
            end_row: m.span.end_row,
        })
    }

    fn field(&self) -> String {
        if self.binding == self.key {
            format!("{} = {}", self.key, self.default)
        } else {
            format!("{}: {} = {}", self.key, self.binding, self.default)
        }
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

    // Group consecutive sites: same source, and starting on the line after the
    // previous statement ends. This supports wrapped defaults/source
    // expressions without merging across blank lines or comments.
    let mut groups: Vec<Vec<Site>> = Vec::new();
    for site in sites {
        match groups.last_mut() {
            Some(group)
                if group.last().is_some_and(|prev| {
                    prev.source == site.source && site.start_row == prev.end_row + 1
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
        .filter(|group| has_unique_keys(group))
        .map(|group| {
            let fields = group.iter().map(Site::field).collect::<Vec<_>>().join(", ");
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

fn has_unique_keys(group: &[Site]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    group.iter().all(|site| seen.insert(&site.key))
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
    fn folds_aliased_sites() {
        let src = "fn f() {\n  let t = cfg?.timeout ?? 30\n  let retries = cfg?.retries ?? 3\n  let label = cfg?.name ?? \"anon\"\n}\n";
        let out = fold(src);
        assert_eq!(
            out,
            "fn f() {\n  let { timeout: t = 30, retries = 3, name: label = \"anon\" } = cfg ?? {}\n}\n"
        );
    }

    #[test]
    fn folds_after_a_wrapped_previous_site() {
        let src = "fn f() {\n  let path = parse({name: \"x\"}, argv).ok?.path\n    ?? \"\"\n  let verbose = parse({name: \"x\"}, argv).ok?.verbose ?? false\n}\n";
        let out = fold(src);
        assert_eq!(
            out,
            "fn f() {\n  let { path = \"\", verbose = false } = parse({name: \"x\"}, argv).ok ?? {}\n}\n"
        );
    }

    #[test]
    fn leaves_duplicate_property_keys_untouched() {
        let src = "fn f() {\n  let first = cfg?.value ?? 1\n  let second = cfg?.value ?? 2\n}\n";
        assert_eq!(fold(src), src);
    }
}
