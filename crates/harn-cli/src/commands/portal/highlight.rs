use std::collections::HashSet;

use harn_lexer::KEYWORDS;
use harn_vm::stdlib::stdlib_builtin_names;

use super::dto::PortalHighlightKeywords;

pub(super) fn build_highlight_keywords() -> PortalHighlightKeywords {
    let literals = ["true", "false", "nil"];
    let literal_set = literals.into_iter().collect::<HashSet<_>>();
    let keyword = KEYWORDS
        .iter()
        .filter(|item| !literal_set.contains(**item))
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    let keyword_set = KEYWORDS.iter().copied().collect::<HashSet<_>>();
    let mut built_in = stdlib_builtin_names()
        .into_iter()
        .filter(|name| !name.starts_with("__"))
        .filter(|name| !keyword_set.contains(name.as_str()))
        .collect::<Vec<_>>();
    built_in.sort();
    built_in.dedup();
    PortalHighlightKeywords {
        keyword,
        literal: literals.into_iter().map(str::to_string).collect(),
        built_in,
    }
}
