//! The pattern compiler: a code snippet with metavariable holes → a
//! tree-sitter query.
//!
//! This is the atomic-tier `pattern` form. The idea (from ast-grep) is to
//! let rule authors write a *snippet of real code* with `$VAR` holes
//! instead of hand-authoring a tree-sitter S-expression query:
//!
//! ```text
//!   $SRC?.$KEY ?? $DEFAULT
//! ```
//!
//! compiles to
//!
//! ```text
//!   ((binary_expression
//!      left: (member_expression object: (_) @SRC (optional_chain) property: (_) @KEY)
//!      "??"
//!      right: (_) @DEFAULT) @__match)
//! ```
//!
//! ## How it works
//!
//! 1. Each `$VAR` is replaced with a unique placeholder identifier so the
//!    snippet parses as ordinary code in the target grammar.
//! 2. We parse the substituted snippet — bare, then in a per-language
//!    wrapper context (e.g. a function body) when the fragment is not a
//!    valid compilation unit — and locate the snippet's own subtree by its
//!    byte range in the parsed source.
//! 3. We walk that subtree and mirror it into a query: every named child is
//!    emitted with its field name, every anonymous token (operators,
//!    keywords, punctuation) is emitted as a quoted literal so the structure
//!    is matched precisely, and every placeholder becomes a `(_) @VAR`
//!    wildcard capture.
//! 4. Repeated metavariables unify: the second and later occurrences get
//!    helper captures plus an `(#eq? …)` predicate so `$X … $X` only matches
//!    when both holes carry identical text.
//!
//! Variadic `$$$` holes are not yet supported (tracked for the relational
//! tier, #2833); they compile to a clear error.

use std::collections::HashMap;

use harn_hostlib::ast::{api, Language};
use tree_sitter::Node;

/// The capture name bound to the whole matched pattern, used for range
/// extraction. Chosen to not collide with a user metavar (which are
/// uppercase by convention and never start with `__`).
pub const ROOT_CAPTURE: &str = "__match";

/// Placeholder identifier stem substituted for each `$VAR`. Lowercase +
/// `__` prefix keeps it a valid identifier across grammars and unlikely to
/// collide with real snippet text.
const PLACEHOLDER_STEM: &str = "__harn_hole_";

/// A snippet pattern compiled to a tree-sitter query string.
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    /// The generated S-expression query. Always binds the pattern root to
    /// `@__match` ([`ROOT_CAPTURE`]).
    pub query: String,
    /// Metavar names in first-appearance order (without the leading `$`).
    pub metavars: Vec<String>,
}

/// Compile a `pattern` snippet for `language` into a tree-sitter query.
///
/// A snippet is often a *fragment* (`a + a`, `foo(bar)`) that is not a
/// valid compilation unit on its own. We therefore try the snippet bare
/// first (works for expression-statement languages like TS/JS/Python),
/// then in a small set of per-language wrapper contexts (e.g. a function
/// body for Rust/Go), and locate the snippet's own subtree by byte range.
pub fn compile_pattern(snippet: &str, language: Language) -> Result<CompiledPattern, String> {
    let sub = substitute(snippet)?;
    let mut last_err: Option<String> = None;

    for (prefix, suffix) in contexts(language) {
        let wrapped = format!("{prefix}{}{suffix}", sub.text);
        let tree = api::parse_tree(&wrapped, language).map_err(|err| err.to_string())?;
        let root = tree.root_node();
        if root.has_error() {
            last_err = Some(format!(
                "snippet did not parse cleanly in `{}`: `{snippet}`",
                language.name()
            ));
            continue;
        }

        // The snippet occupies `[start, end)` inside the wrapped source; the
        // deepest node spanning that range is its own subtree (no need to
        // descend wrappers — and no risk of over-descending a single-child
        // node like a unary expression).
        let start = prefix.len();
        let end = start + sub.text.len();
        let Some(pattern_root) = root.descendant_for_byte_range(start, end.saturating_sub(1))
        else {
            last_err = Some(format!(
                "could not locate snippet subtree in `{}`",
                language.name()
            ));
            continue;
        };

        let bytes = wrapped.as_bytes();
        let mut builder = QueryBuilder::new(bytes, &sub.placeholder_to_metavar);
        let body = builder.build(pattern_root);
        let predicates = builder.predicates();
        let query = if predicates.is_empty() {
            format!("({body} @{ROOT_CAPTURE})")
        } else {
            format!("({body} @{ROOT_CAPTURE} {predicates})")
        };
        return Ok(CompiledPattern {
            query,
            metavars: sub.metavar_order,
        });
    }

    Err(last_err.unwrap_or_else(|| format!("snippet did not parse in `{}`", language.name())))
}

/// Candidate parse contexts for a snippet, tried in order. The bare context
/// (`""`, `""`) comes first; item-required languages add a wrapper that
/// makes an expression/statement fragment parse. Languages whose top level
/// already accepts expression statements (TS/JS/Python/Ruby/…) only need
/// the bare context.
fn contexts(language: Language) -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("", "")];
    let wrapper = match language {
        Language::Rust => Some(("fn __harn_probe() { ", " }")),
        Language::Go => Some(("package p\nfunc __harn_probe() { ", " }")),
        Language::Java | Language::CSharp => {
            Some(("class __HarnProbe { void __harn_probe() { ", " } }"))
        }
        Language::C | Language::Cpp => Some(("void __harn_probe() { ", " }")),
        Language::Kotlin => Some(("fun __harn_probe() { ", " }")),
        Language::Swift => Some(("func __harn_probe() { ", " }")),
        Language::Scala => Some(("def __harn_probe() = { ", " }")),
        _ => None,
    };
    v.extend(wrapper);
    v
}

// ---------------------------------------------------------------------------
// Step 1: metavar substitution
// ---------------------------------------------------------------------------

struct Substituted {
    /// Snippet with `$VAR` replaced by placeholder identifiers.
    text: String,
    /// placeholder identifier → metavar name.
    placeholder_to_metavar: HashMap<String, String>,
    /// Metavar names in first-appearance order.
    metavar_order: Vec<String>,
}

fn substitute(snippet: &str) -> Result<Substituted, String> {
    let mut text = String::with_capacity(snippet.len());
    let mut placeholder_to_metavar = HashMap::new();
    let mut metavar_to_placeholder: HashMap<String, String> = HashMap::new();
    let mut metavar_order: Vec<String> = Vec::new();

    let bytes = snippet.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy this UTF-8 scalar verbatim. Indexing the &str at byte
            // boundaries is safe because we only special-case ASCII `$`.
            let ch = snippet[i..].chars().next().unwrap();
            text.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if snippet[i..].starts_with("$$$") {
            return Err(
                "variadic `$$$` metavariables are not yet supported (tracked in #2833)".into(),
            );
        }
        // Parse `$NAME` where NAME is `[A-Za-z_][A-Za-z0-9_]*`.
        let name_start = i + 1;
        let mut j = name_start;
        if j < bytes.len() && is_ident_start(bytes[j]) {
            j += 1;
            while j < bytes.len() && is_ident_continue(bytes[j]) {
                j += 1;
            }
        }
        if j == name_start {
            // A lone `$` that is not a metavar — keep it literal.
            text.push('$');
            i += 1;
            continue;
        }
        let name = &snippet[name_start..j];
        let placeholder = metavar_to_placeholder
            .entry(name.to_string())
            .or_insert_with(|| {
                let placeholder = format!("{PLACEHOLDER_STEM}{}", metavar_order.len());
                metavar_order.push(name.to_string());
                placeholder
            })
            .clone();
        placeholder_to_metavar.insert(placeholder.clone(), name.to_string());
        text.push_str(&placeholder);
        i = j;
    }

    // A pattern with no metavars is a valid *literal* pattern (it matches a
    // fixed structure), so we do not require one.

    Ok(Substituted {
        text,
        placeholder_to_metavar,
        metavar_order,
    })
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ---------------------------------------------------------------------------
// Step 2: walk the located subtree into a query
// ---------------------------------------------------------------------------

struct QueryBuilder<'a> {
    src: &'a [u8],
    placeholder_to_metavar: &'a HashMap<String, String>,
    /// occurrence count per metavar, to mint unification helper captures.
    occurrences: HashMap<String, usize>,
    /// `(#eq? …)` predicates for repeated metavars and literal leaves.
    eq_predicates: Vec<String>,
    /// counter for literal-leaf text-constraint captures.
    literal_count: usize,
}

impl<'a> QueryBuilder<'a> {
    fn new(src: &'a [u8], placeholder_to_metavar: &'a HashMap<String, String>) -> Self {
        QueryBuilder {
            src,
            placeholder_to_metavar,
            occurrences: HashMap::new(),
            eq_predicates: Vec::new(),
            literal_count: 0,
        }
    }

    fn build(&mut self, node: Node<'_>) -> String {
        // A placeholder leaf is a metavar hole.
        if node.child_count() == 0 {
            let text = self.node_text(node);
            if let Some(metavar) = self.placeholder_to_metavar.get(text) {
                return format!("(_) @{}", self.capture_for(metavar));
            }
            if node.is_named() {
                // A literal named leaf (a specific identifier / literal in
                // the snippet): constrain it to its exact text so `foo()`
                // matches calls to `foo`, not any call.
                let cap = format!("__lit_{}", self.literal_count);
                self.literal_count += 1;
                self.eq_predicates
                    .push(format!("(#eq? @{cap} {})", quote_literal(text)));
                return format!("({}) @{cap}", node.kind());
            }
            return quote_literal(text);
        }

        let mut parts: Vec<String> = Vec::new();
        let mut cursor = node.walk();
        for (i, child) in node.children(&mut cursor).enumerate() {
            let sub = self.build(child);
            // Field names only attach to named children; an anonymous token
            // in a field slot is matched positionally as a literal, which
            // tree-sitter accepts where `field: "literal"` may not.
            match node.field_name_for_child(i as u32) {
                Some(field) if child.is_named() => parts.push(format!("{field}: {sub}")),
                _ => parts.push(sub),
            }
        }
        format!("({} {})", node.kind(), parts.join(" "))
    }

    /// Mint the capture name for this occurrence of `metavar`. The first
    /// occurrence is `@NAME`; later ones are `@NAME.k` plus an `(#eq? …)`
    /// predicate tying them to the first (metavar unification).
    fn capture_for(&mut self, metavar: &str) -> String {
        let count = self.occurrences.entry(metavar.to_string()).or_insert(0);
        *count += 1;
        if *count == 1 {
            metavar.to_string()
        } else {
            let helper = format!("{metavar}.{count}");
            self.eq_predicates
                .push(format!("(#eq? @{metavar} @{helper})"));
            helper
        }
    }

    fn predicates(&self) -> String {
        self.eq_predicates.join(" ")
    }

    fn node_text(&self, node: Node<'_>) -> &'a str {
        std::str::from_utf8(&self.src[node.start_byte()..node.end_byte()]).unwrap_or_default()
    }
}

/// Quote an anonymous token as a tree-sitter query literal, escaping `"`
/// and `\`.
fn quote_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Query, QueryCursor};

    /// Compile `snippet`, run the query against `code`, and return the
    /// captured text for each requested metavar from the first match.
    fn run(snippet: &str, language: Language, code: &str) -> Vec<(String, Vec<String>)> {
        let compiled = compile_pattern(snippet, language).expect("compiles");
        let ts_language = language.ts_language().expect("grammar");
        let query = Query::new(&ts_language, &compiled.query)
            .unwrap_or_else(|e| panic!("query rejected: {e}\nquery: {}", compiled.query));
        let tree = api::parse_tree(code, language).expect("parse code");
        let names: Vec<&str> = query.capture_names().to_vec();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            let mut per_capture: HashMap<String, Vec<String>> = HashMap::new();
            for cap in m.captures {
                let name = names[cap.index as usize].to_string();
                let text = code[cap.node.start_byte()..cap.node.end_byte()].to_string();
                per_capture.entry(name).or_default().push(text);
            }
            for (name, texts) in per_capture {
                out.push((name, texts));
            }
        }
        out
    }

    fn capture<'a>(binds: &'a [(String, Vec<String>)], name: &str) -> &'a [String] {
        binds
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    #[test]
    fn compiles_destructuring_default_in_typescript() {
        // The #2824 / burin-code#1629 codemod shape.
        let snippet = "$SRC?.$KEY ?? $DEFAULT";
        let compiled = compile_pattern(snippet, Language::TypeScript).expect("compiles");
        assert_eq!(compiled.metavars, vec!["SRC", "KEY", "DEFAULT"]);
        // It captures the optional-chain object/property and the fallback.
        let binds = run(
            snippet,
            Language::TypeScript,
            "const a = cfg?.timeout ?? 30;",
        );
        assert_eq!(capture(&binds, "SRC"), ["cfg".to_string()]);
        assert_eq!(capture(&binds, "KEY"), ["timeout".to_string()]);
        assert_eq!(capture(&binds, "DEFAULT"), ["30".to_string()]);
    }

    #[test]
    fn operator_is_constrained_not_just_structure() {
        // The `??` literal in the query must reject a `||` with the same
        // structural shape — otherwise the codemod would be unsound.
        let snippet = "$SRC?.$KEY ?? $DEFAULT";
        let binds = run(
            snippet,
            Language::TypeScript,
            "const a = cfg?.timeout || 30;",
        );
        assert!(
            capture(&binds, "SRC").is_empty(),
            "|| must not match the ?? pattern"
        );
    }

    #[test]
    fn round_trips_the_assignment_form() {
        // The literal acceptance pattern: `$NAME = $SRC?.$KEY ?? $DEFAULT`.
        let snippet = "$NAME = $SRC?.$KEY ?? $DEFAULT";
        let compiled = compile_pattern(snippet, Language::TypeScript).expect("compiles");
        assert_eq!(compiled.metavars, vec!["NAME", "SRC", "KEY", "DEFAULT"]);
        let binds = run(
            snippet,
            Language::TypeScript,
            "x = src?.userId ?? fallback;",
        );
        assert_eq!(capture(&binds, "NAME"), ["x".to_string()]);
        assert_eq!(capture(&binds, "SRC"), ["src".to_string()]);
        assert_eq!(capture(&binds, "KEY"), ["userId".to_string()]);
        assert_eq!(capture(&binds, "DEFAULT"), ["fallback".to_string()]);
    }

    #[test]
    fn lifts_metavars_in_rust() {
        let snippet = "let $NAME = $VALUE;";
        let binds = run(snippet, Language::Rust, "fn f() { let total = compute(); }");
        assert_eq!(capture(&binds, "NAME"), ["total".to_string()]);
        assert_eq!(capture(&binds, "VALUE"), ["compute()".to_string()]);
    }

    #[test]
    fn lifts_metavars_in_python() {
        let snippet = "$FN($ARG)";
        let binds = run(snippet, Language::Python, "print(value)");
        assert_eq!(capture(&binds, "FN"), ["print".to_string()]);
        assert_eq!(capture(&binds, "ARG"), ["value".to_string()]);
    }

    #[test]
    fn lifts_metavars_in_go() {
        let snippet = "$FN($ARG)";
        let binds = run(snippet, Language::Go, "package main\nfunc m() { log(err) }");
        assert_eq!(capture(&binds, "FN"), ["log".to_string()]);
        assert_eq!(capture(&binds, "ARG"), ["err".to_string()]);
    }

    #[test]
    fn repeated_metavar_unifies() {
        // `$X + $X` must match `a + a` but not `a + b`.
        let snippet = "$X + $X";
        let same = run(snippet, Language::Rust, "fn f() { let _ = a + a; }");
        assert_eq!(capture(&same, "X"), ["a".to_string()]);
        let different = run(snippet, Language::Rust, "fn f() { let _ = a + b; }");
        assert!(
            capture(&different, "X").is_empty(),
            "unification must reject `a + b`"
        );
    }

    #[test]
    fn rejects_unparseable_snippet() {
        let err = compile_pattern("$A ?? ?? $B", Language::TypeScript).unwrap_err();
        assert!(err.contains("did not parse"), "got: {err}");
    }

    #[test]
    fn rejects_variadic_for_now() {
        let err = compile_pattern("foo($$$ARGS)", Language::TypeScript).unwrap_err();
        assert!(err.contains("variadic"), "got: {err}");
    }

    #[test]
    fn literal_pattern_matches_exact_text() {
        // A metavar-free pattern is a literal pattern: `foo()` matches calls
        // to `foo`, not to other functions.
        let snippet = "foo()";
        let compiled = compile_pattern(snippet, Language::TypeScript).expect("compiles");
        assert!(compiled.metavars.is_empty());
        // It matches `foo()` …
        let hit = run(snippet, Language::TypeScript, "foo();");
        assert!(!hit.is_empty());
        // … but not `bar()` (the literal identifier is constrained).
        let miss = run(snippet, Language::TypeScript, "bar();");
        assert!(
            miss.is_empty(),
            "bar() must not match foo()'s literal pattern: {miss:?}"
        );
    }
}
