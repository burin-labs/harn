//! The single glob-matching implementation for the Harn workspace.
//!
//! Before this crate existed, seven near-identical `glob_match` functions
//! lived in `harn-ir`, `harn-vm` (metadata scan, llm config, capabilities,
//! merge-captain audit, runtime hooks, llm mock), and `harn-cli` (skills) —
//! each with subtly different wildcard semantics. A pattern that matched in
//! one subsystem silently behaved differently in another (hook routing
//! honored `?`/`[...]`, model-override matching did not; the metadata scanner
//! matched the `*` in `**/*.rs` literally). This crate is the one place those
//! semantics are defined.
//!
//! Three contracts, chosen per call site:
//!
//! - [`match_path`] — slash-aware file-path globs over [`globset`]: `*`/`?`
//!   never cross `/`, `**` crosses directories, `{a,b}` alternates. Use for
//!   path-shaped inputs (invariant globs, skill manifests, workspace paths,
//!   path-keyed policy constraints).
//! - [`match_name`] — full glob syntax (`*`, `?`, `[...]`, `{a,b}`) over flat
//!   identifiers where `/` has no special meaning (`*` crosses it). Use for
//!   tool names, model ids, hook patterns, event names.
//! - [`match_prose`] — `*`-only ordered-segment matching where every other
//!   character is literal. Use when patterns target free text that routinely
//!   contains `?`, `[`, or `{` as ordinary prose (e.g. llm-mock prompt
//!   matchers).

/// Slash-aware glob matching for file paths, with the semantics of
/// [`globset`] under `literal_separator`:
///
/// - `*` and `?` never match `/`; `**` as a whole segment crosses
///   directories (a leading `**/` also matches zero directories, so
///   `src/**/*.rs` matches `src/main.rs`)
/// - `[...]` matches one byte; only a class that names `/` or is negated
///   (`[!x]`) can match `/`
/// - `{a,b}` matches either alternative; `\` escapes a metacharacter
/// - matching is byte-wise: over a non-ASCII name, `?` consumes one UTF-8
///   byte, not one character
///
/// A path equal to the pattern always matches, so a literal that happens to
/// contain glob syntax (a route directory such as `app/[slug]/page.tsx`)
/// still matches itself. A pattern that does not parse as a glob (an unclosed
/// `[` or `{`) matches only that exact path.
#[must_use]
pub fn match_path(pattern: &str, path: &str) -> bool {
    pattern == path
        || compiled(pattern, Separator::Literal).is_some_and(|matcher| matcher.is_match(path))
}

/// Full glob matching for flat names (tool names, model ids, hook patterns).
///
/// `/` has no special meaning: `*` and `?` match across it. Beyond `*` and
/// `?`, character classes (`[abc]`) and alternates (`{a,b}`) are supported
/// via [`globset`]. Patterns that fail to parse as globs fall back to the
/// historical prefix/suffix/equality behavior shared by the pre-consolidation
/// call sites, so an unclosed `[` never panics or silently rejects.
///
/// A name equal to the pattern always matches, so a literal that happens to
/// contain glob syntax (a route directory such as `app/[slug]/page.tsx`)
/// still matches itself instead of being read only as a character class.
///
/// Compiled matchers are cached per thread; patterns come from configuration
/// and hook registration, so the cache stays small.
#[must_use]
pub fn match_name(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == name {
        return true;
    }
    if !has_name_metacharacters(pattern) {
        return pattern == name;
    }
    // Fast paths for the dominant shapes: `prefix*`, `*suffix`, `*infix*`.
    if let Some(prefix) = pattern.strip_suffix('*') {
        if !has_name_metacharacters(prefix) {
            return name.starts_with(prefix);
        }
        if let Some(infix) = prefix.strip_prefix('*') {
            if !has_name_metacharacters(infix) {
                return name.contains(infix);
            }
        }
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        if !has_name_metacharacters(suffix) {
            return name.ends_with(suffix);
        }
    }
    compiled_name_match(pattern, name)
}

/// Whether a flat-name pattern uses glob operators or backslash escaping.
/// Use this before deriving an exact name matcher from an external identifier.
#[must_use]
pub fn has_name_metacharacters(pattern: &str) -> bool {
    pattern
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'{' | b'\\'))
}

fn compiled_name_match(pattern: &str, name: &str) -> bool {
    match compiled(pattern, Separator::Crossed) {
        Some(matcher) => matcher.is_match(name),
        // Unparsable glob: match the way pre-consolidation call sites did.
        None => {
            if let Some(prefix) = pattern.strip_suffix('*') {
                return name.starts_with(prefix);
            }
            if let Some(suffix) = pattern.strip_prefix('*') {
                return name.ends_with(suffix);
            }
            pattern == name
        }
    }
}

/// Whether `*`, `?` and `[...]` may match `/`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Separator {
    Literal,
    Crossed,
}

/// Compiled matchers keyed by pattern and separator mode; `None` = unparsable.
type GlobCache = std::collections::HashMap<(Box<str>, Separator), Option<globset::GlobMatcher>>;

/// Compile a glob once per thread, or `None` when it does not parse.
///
/// `\` escapes on every platform (globset's default disables it on Windows):
/// patterns are written with forward slashes, and an escaped metacharacter is
/// how a caller asks for it literally.
fn compiled(pattern: &str, separator: Separator) -> Option<globset::GlobMatcher> {
    use std::cell::RefCell;

    thread_local! {
        static COMPILED: RefCell<GlobCache> = RefCell::new(GlobCache::new());
    }

    COMPILED.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Patterns are config/registration-driven and bounded in practice;
        // the cap only guards against pathological dynamic pattern churn.
        if cache.len() > 512 {
            cache.clear();
        }
        cache
            .entry((Box::from(pattern), separator))
            .or_insert_with(|| {
                globset::GlobBuilder::new(pattern)
                    .literal_separator(separator == Separator::Literal)
                    .backslash_escape(true)
                    .build()
                    .ok()
                    .map(|glob| glob.compile_matcher())
            })
            .clone()
    })
}

/// `*`-only ordered-segment matching over free text.
///
/// Splits the pattern on `*` and requires the literal segments to appear in
/// order, anchored at the start/end unless the pattern begins/ends with `*`.
/// Every character other than `*` is literal — including `?`, `[`, and `{` —
/// because prose targets (prompts, transcript text) routinely contain them.
#[must_use]
pub fn match_prose(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }
    let segments: Vec<&str> = pattern.split('*').collect();
    let last = segments.len() - 1;
    let mut remaining = text;
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        if index == 0 {
            match remaining.strip_prefix(segment) {
                Some(rest) => remaining = rest,
                None => return false,
            }
        } else if index == last {
            return remaining.ends_with(segment);
        } else {
            match remaining.split_once(segment) {
                Some((_, rest)) => remaining = rest,
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- match_path: ported from harn-ir and harn-cli (skills) test suites ---

    #[test]
    fn path_single_star_stays_within_a_directory() {
        assert!(match_path("src/*.rs", "src/main.rs"));
        assert!(!match_path("src/*.rs", "src/nested/main.rs"));
        assert!(!match_path("src/*.rs", "other/main.rs"));
    }

    #[test]
    fn path_double_star_crosses_directories() {
        assert!(match_path("src/**/*.rs", "src/nested/main.rs"));
        assert!(match_path("src/**/*.rs", "src/main.rs"));
        assert!(match_path("infra/**", "infra/terraform/main.tf"));
        assert!(match_path("**", "anything/at/all"));
        assert!(match_path("**/*.rs", "main.rs"));
        assert!(match_path("**/*.rs", "deep/tree/main.rs"));
    }

    #[test]
    fn path_double_star_slash_stays_on_directory_boundaries() {
        assert!(match_path("**/bar", "bar"));
        assert!(match_path("**/bar", "foo/bar"));
        assert!(!match_path("**/bar", "foobar"));
        assert!(match_path("src/**/main.rs", "src/main.rs"));
        assert!(match_path("src/**/main.rs", "src/bin/main.rs"));
        assert!(!match_path("src/**/main.rs", "src/binmain.rs"));
    }

    #[test]
    fn path_question_mark_matches_one_non_separator() {
        assert!(match_path("src/ma?n.rs", "src/main.rs"));
        assert!(!match_path("src/ma?n.rs", "src/man.rs"));
        assert!(!match_path("a?b", "a/b"));
        assert!(match_path("*.rs", "é.rs"));
        assert!(!match_path("a?b", "aé/b"));
    }

    /// globset matches bytes, as git's wildmatch does: over a non-ASCII name,
    /// `?` and `[...]` consume one UTF-8 byte, never one character, and still
    /// never `/`. Pinned so a semantics change in the crate is noticed.
    #[test]
    fn path_single_character_wildcards_match_bytes() {
        assert!(!match_path("?.rs", "é.rs"));
        assert!(match_path("??.rs", "é.rs"));
        assert!(!match_path("[é]", "é"));
    }

    #[test]
    fn path_literal_and_edge_cases() {
        assert!(match_path("exact.rs", "exact.rs"));
        assert!(!match_path("exact.rs", "exact.rs.bak"));
        assert!(!match_path("src/**", "src"));
        assert!(match_path("src/**", "src/anything"));
        assert!(match_path("", ""));
        assert!(!match_path("", "x"));
    }

    #[test]
    fn path_classes_match_one_character_within_a_segment() {
        assert!(match_path("src/[ab].rs", "src/b.rs"));
        assert!(!match_path("src/[ab].rs", "src/c.rs"));
        assert!(match_path("v[0-9].txt", "v7.txt"));
        assert!(match_path("v[!0-9].txt", "vx.txt"));
        assert!(!match_path("v[^0-9].txt", "v7.txt"));
        assert!(match_path("[]x]", "]"));
        assert!(
            match_path("a[/]b", "a/b"),
            "a class that names / explicitly may match it"
        );
        assert!(
            match_path("a[!x]b", "a/b"),
            "globset lets a negated class match /"
        );
        assert!(
            !match_path("src/[!x]*.rs", "src/a/b.rs"),
            "but * after it still stays in one segment"
        );
        assert!(match_path("open[bracket", "open[bracket"));
        assert!(
            !match_path("open[*", "open[anything"),
            "an unparsable glob matches only itself"
        );
        assert!(match_path("open[*", "open[*"));
    }

    #[test]
    fn path_brace_alternates_expand_like_a_shell() {
        assert!(match_path("src/{a,b}.ts", "src/b.ts"));
        assert!(!match_path("src/{a,b}.ts", "src/c.ts"));
        assert!(match_path("{src,lib}/**/*.rs", "lib/x/y.rs"));
        assert!(match_path("x.{a,{b,c}}", "x.c"));
        assert!(!match_path("{src,lib}/*.rs", "lib/x/y.rs"));
        assert!(match_path("a{b}c", "a{b}c"));
        assert!(match_path("a{b,c", "a{b,c"));
    }

    #[test]
    fn path_literal_with_glob_syntax_matches_itself() {
        assert!(match_path("app/[slug]/page.tsx", "app/[slug]/page.tsx"));
        assert!(!match_path("app/[slug]/page.tsx", "app/other/page.tsx"));
        assert!(match_path("app/[slug]/page.tsx", "app/s/page.tsx"));
        assert!(match_path("odd/{a,b}.txt", "odd/{a,b}.txt"));
    }

    // --- match_name: ported from hooks, llm_config, capabilities,
    //     merge_captain_audit test suites ---

    #[test]
    fn name_star_matches_everything() {
        assert!(match_name("*", "anything"));
        assert!(match_name("*", ""));
    }

    #[test]
    fn name_prefix_suffix_and_exact() {
        assert!(match_name("claude-*", "claude-sonnet-4-20250514"));
        assert!(match_name("gpt-*", "gpt-4o"));
        assert!(!match_name("claude-*", "gpt-4o"));
        assert!(match_name("*-latest", "llama3.2-latest"));
        assert!(!match_name("*-latest", "llama3.2"));
        assert!(match_name("gpt-4o", "gpt-4o"));
        assert!(!match_name("gpt-4o", "gpt-4o-mini"));
    }

    #[test]
    fn name_substring_and_middle_star() {
        assert!(match_name("*gpt*", "openai/gpt-5.4"));
        assert!(match_name("*claude*", "anthropic/claude-opus-4-7"));
        assert!(!match_name("*xyz*", "openai/gpt-5.4"));
        assert!(match_name("claude-*-latest", "claude-sonnet-latest"));
        assert!(!match_name("claude-*-latest", "claude-sonnet-beta"));
    }

    #[test]
    fn name_star_crosses_separators() {
        assert!(match_name("tool/*", "tool/a/b"));
        assert!(match_name("*svc", "a/b/svc"));
    }

    #[test]
    fn name_multi_star_segments_in_order() {
        assert!(match_name("a*b*c", "a-x-b-y-c"));
        assert!(!match_name("a*b*c", "a-x-c-y-b"));
        assert!(match_name("pre*mid*", "pre-anything-mid-tail"));
    }

    #[test]
    fn name_question_mark_and_classes() {
        assert!(match_name("gpt-?o", "gpt-4o"));
        assert!(!match_name("gpt-?o", "gpt-44o"));
        assert!(match_name("file[12]", "file1"));
        assert!(!match_name("file[12]", "file3"));
    }

    #[test]
    fn name_escaped_star_is_literal_on_every_path() {
        assert!(match_name("foo\\*", "foo*"));
        assert!(!match_name("foo\\*", "foo\\bar"));
    }

    #[test]
    fn name_brace_alternates_use_glob_syntax() {
        assert!(match_name("gpt-{4o,5}", "gpt-4o"));
        assert!(match_name("gpt-{4o,5}", "gpt-5"));
        assert!(!match_name("gpt-{4o,5}", "gpt-4.1"));
    }

    #[test]
    fn name_literal_with_glob_syntax_matches_itself() {
        // Route directories such as `[slug]` are literal path segments; the
        // class reading still applies to every other name.
        assert!(match_name("app/[slug]/page.tsx", "app/[slug]/page.tsx"));
        assert!(match_name("app/[slug]/page.tsx", "app/s/page.tsx"));
        assert!(!match_name("app/[slug]/page.tsx", "app/x/page.tsx"));
        assert!(match_name("src/{a,b}.ts", "src/{a,b}.ts"));
        assert!(match_name("src/{a,b}.ts", "src/b.ts"));
    }

    #[test]
    fn name_unparsable_glob_falls_back_to_legacy_affix_matching() {
        // Unclosed `[` fails glob compilation; legacy behavior treated the
        // pattern as a literal prefix when it ends in `*`.
        assert!(match_name("f[oo*", "f[oo-bar"));
        assert!(!match_name("f[oo*", "g[oo-bar"));
        assert!(match_name("f[oo", "f[oo"));
    }

    // --- match_prose: ported from the llm mock matcher tests ---

    #[test]
    fn prose_segments_in_order_with_literal_punctuation() {
        assert!(match_prose("*", "anything"));
        assert!(match_prose("hello", "hello"));
        assert!(!match_prose("hello", "hello world"));
        assert!(match_prose("hello*", "hello world"));
        assert!(match_prose("*world", "hello world"));
        assert!(match_prose("*llo wo*", "hello world"));
        assert!(match_prose("he*wo*ld", "hello world"));
        assert!(!match_prose("he*xx*ld", "hello world"));
    }

    #[test]
    fn prose_treats_glob_metacharacters_as_literals() {
        assert!(match_prose("what is [x]?*", "what is [x]? tell me"));
        assert!(!match_prose("what is [x]?*", "what is x? tell me"));
        assert!(match_prose("*{json}*", "respond with {json} only"));
    }
}
