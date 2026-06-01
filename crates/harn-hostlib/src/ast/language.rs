//! Tree-sitter language registry.
//!
//! The set of languages, their canonical names, and their file extensions
//! form the hostlib AST wire contract. Adding or dropping a language
//! requires coordinated schema, fixture, and host-bridge updates.
//!
//! ## Per-language onboarding contract (B.7)
//!
//! Each [`Language`] variant carries the full adapter contract on the enum
//! itself — there is no separate `LanguageAdapter` object to keep in sync:
//!
//! 1. **grammar binding** — [`Language::ts_language`]
//! 2. **wire name + aliases** — [`Language::name`] / [`Language::from_name`]
//! 3. **extension detection** — [`Language::from_extension`]
//! 4. **symbol-graph projection** (drives `rename_symbol`) —
//!    [`Language::rename_identifier_kinds`]
//! 5. **symbol/outline extraction** — `ast::symbols::extract`
//! 6. **test fixture** — `tests/fixtures/ast/<name>/`
//!
//! Format-preserving span replacement and trivia/indentation handling are
//! grammar-agnostic (byte-span splice + inferred indent), so they need no
//! per-language code. The result is that adding a language is a bounded
//! ticket: register the grammar, add the four mapping arms, drop in a
//! fixture, and (optionally) an identifier-kind table for rename support.

use tree_sitter::Language as TsLanguage;

/// Languages with tree-sitter grammar support.
///
/// The string returned by [`Language::name`] is the canonical wire name;
/// callers (and the JSON schemas) refer to languages by that string. The
/// trailing group (`Json`..`Markdown`) are data/markup/config grammars:
/// they support the query-driven edit primitives but have no symbol-graph
/// projection (see [`Language::edit_capabilities`]).
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Python,
    Go,
    Rust,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Kotlin,
    Php,
    Scala,
    Bash,
    Swift,
    Zig,
    Elixir,
    Lua,
    Haskell,
    R,
    Json,
    Yaml,
    Toml,
    Css,
    Html,
    Sql,
    Markdown,
}

/// The text-level fallback the agent loop should reach for when an
/// AST-precise edit is unavailable for a file. Surfaced verbatim as the
/// `fallback_suggestion` field on every `unsupported_*` edit response so
/// the loop can degrade gracefully without per-call branching.
pub const TEXT_PATCH_FALLBACK: &str =
    "fall back to a text-level edit (std/edit `edit_safe_text_patch`)";

/// Which AST-precise edit primitives are available for a language.
///
/// `apply_node` and `insert_at_anchor` are query-driven and work against
/// any registered tree-sitter grammar, so they are always `true`.
/// `rename_symbol` needs a per-language identifier-kind projection (see
/// [`Language::rename_identifier_kinds`]); `symbols`/`outline` need a
/// per-language extractor (see `ast::symbols`). The matrix is the
/// onboarding contract: it tells the agent loop which primitive to reach
/// for and is rendered into the capability-matrix docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditCapabilities {
    /// Tree-sitter query → format-preserving replace.
    pub apply_node: bool,
    /// Anchored sibling/child insertion.
    pub insert_at_anchor: bool,
    /// Cross-file safe rename via the symbol graph.
    pub rename_symbol: bool,
    /// Symbol + outline extraction.
    pub symbols: bool,
}

impl Language {
    /// Canonical wire name.
    pub fn name(self) -> &'static str {
        match self {
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Jsx => "jsx",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::CSharp => "csharp",
            Language::Ruby => "ruby",
            Language::Kotlin => "kotlin",
            Language::Php => "php",
            Language::Scala => "scala",
            Language::Bash => "bash",
            Language::Swift => "swift",
            Language::Zig => "zig",
            Language::Elixir => "elixir",
            Language::Lua => "lua",
            Language::Haskell => "haskell",
            Language::R => "r",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::Css => "css",
            Language::Html => "html",
            Language::Sql => "sql",
            Language::Markdown => "markdown",
        }
    }

    /// Tree-sitter grammar handle, or `None` when this build was not
    /// compiled with the grammar family that backs `self`.
    ///
    /// Each arm is gated on its `grammar-*` family feature, so a trimmed
    /// build only links the grammars it asked for. The `name`/extension/
    /// detection metadata above stays complete regardless of features — a
    /// lean build still *recognizes* a `.py` file, it just returns `None`
    /// here and the edit primitives degrade to the text fallback. The full
    /// (default) build enables every family, so `None` never occurs there.
    /// Cheap when present; the underlying `LANGUAGE` constants are static.
    pub fn ts_language(self) -> Option<TsLanguage> {
        Some(match self {
            #[cfg(feature = "grammar-web")]
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "grammar-web")]
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            #[cfg(feature = "grammar-web")]
            Language::JavaScript | Language::Jsx => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "grammar-web")]
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            #[cfg(feature = "grammar-web")]
            Language::Css => tree_sitter_css::LANGUAGE.into(),

            #[cfg(feature = "grammar-systems")]
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "grammar-systems")]
            Language::C => tree_sitter_c::LANGUAGE.into(),
            #[cfg(feature = "grammar-systems")]
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            #[cfg(feature = "grammar-systems")]
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            #[cfg(feature = "grammar-systems")]
            Language::Zig => tree_sitter_zig::LANGUAGE.into(),

            #[cfg(feature = "grammar-scripting")]
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "grammar-scripting")]
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            #[cfg(feature = "grammar-scripting")]
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            #[cfg(feature = "grammar-scripting")]
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            #[cfg(feature = "grammar-scripting")]
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            #[cfg(feature = "grammar-scripting")]
            Language::R => tree_sitter_r::LANGUAGE.into(),

            #[cfg(feature = "grammar-jvm")]
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            #[cfg(feature = "grammar-jvm")]
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            #[cfg(feature = "grammar-jvm")]
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),

            #[cfg(feature = "grammar-enterprise")]
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            #[cfg(feature = "grammar-enterprise")]
            Language::Swift => tree_sitter_swift::LANGUAGE.into(),
            #[cfg(feature = "grammar-enterprise")]
            Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            #[cfg(feature = "grammar-enterprise")]
            Language::Haskell => tree_sitter_haskell::LANGUAGE.into(),

            #[cfg(feature = "grammar-data")]
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            #[cfg(feature = "grammar-data")]
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            #[cfg(feature = "grammar-data")]
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            #[cfg(feature = "grammar-data")]
            Language::Sql => tree_sitter_sequel::LANGUAGE.into(),
            // tree-sitter-md ships a split block/inline grammar; the block
            // grammar is the structural tree the edit primitives operate
            // on (headings, lists, fenced code, …).
            #[cfg(feature = "grammar-data")]
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),

            // Any language whose family was not compiled into this build.
            // Unreachable under the default (all-families) build.
            #[allow(unreachable_patterns)]
            _ => return None,
        })
    }

    /// Resolve a language from its canonical wire name. Accepts a few
    /// historical aliases (`ts`, `js`, `c++`, …) so users don't have to
    /// memorize the exact spelling.
    pub fn from_name(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase();
        Some(match normalized.as_str() {
            "typescript" | "ts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "javascript" | "js" => Language::JavaScript,
            "jsx" => Language::Jsx,
            "python" | "py" => Language::Python,
            "go" | "golang" => Language::Go,
            "rust" | "rs" => Language::Rust,
            "java" => Language::Java,
            "c" => Language::C,
            "cpp" | "c++" | "cxx" => Language::Cpp,
            "csharp" | "c#" | "cs" => Language::CSharp,
            "ruby" | "rb" => Language::Ruby,
            "kotlin" | "kt" => Language::Kotlin,
            "php" => Language::Php,
            "scala" => Language::Scala,
            "bash" | "shell" | "sh" | "zsh" => Language::Bash,
            "swift" => Language::Swift,
            "zig" => Language::Zig,
            "elixir" | "ex" => Language::Elixir,
            "lua" => Language::Lua,
            "haskell" | "hs" => Language::Haskell,
            "r" => Language::R,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "css" => Language::Css,
            "html" | "htm" => Language::Html,
            "sql" => Language::Sql,
            "markdown" | "md" => Language::Markdown,
            _ => return None,
        })
    }

    /// Resolve a language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
        Some(match normalized.as_str() {
            "ts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "jsx" => Language::Jsx,
            "py" => Language::Python,
            "go" => Language::Go,
            "rs" => Language::Rust,
            "java" => Language::Java,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Language::Cpp,
            "cs" | "csx" => Language::CSharp,
            "rb" => Language::Ruby,
            "kt" | "kts" => Language::Kotlin,
            "php" => Language::Php,
            "scala" | "sc" => Language::Scala,
            "sh" | "bash" | "zsh" => Language::Bash,
            "swift" => Language::Swift,
            "zig" | "zon" => Language::Zig,
            "ex" | "exs" => Language::Elixir,
            "lua" => Language::Lua,
            "hs" | "lhs" => Language::Haskell,
            "r" => Language::R,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "css" => Language::Css,
            "html" | "htm" => Language::Html,
            "sql" => Language::Sql,
            "md" | "markdown" => Language::Markdown,
            _ => return None,
        })
    }

    /// Resolve from a file path: prefer explicit `language_hint` if
    /// supplied, otherwise fall back to extension-based detection.
    pub fn detect(path: &std::path::Path, language_hint: Option<&str>) -> Option<Self> {
        if let Some(name) = language_hint.and_then(|s| (!s.is_empty()).then_some(s)) {
            return Self::from_name(name);
        }
        let ext = path.extension().and_then(|s| s.to_str())?;
        Self::from_extension(ext)
    }

    /// A representative file extension for the language (no leading dot).
    /// Used by docs and the onboarding probe; not necessarily the only
    /// extension [`Language::from_extension`] accepts.
    pub fn primary_extension(self) -> &'static str {
        match self {
            Language::TypeScript => "ts",
            Language::Tsx => "tsx",
            Language::JavaScript => "js",
            Language::Jsx => "jsx",
            Language::Python => "py",
            Language::Go => "go",
            Language::Rust => "rs",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::CSharp => "cs",
            Language::Ruby => "rb",
            Language::Kotlin => "kt",
            Language::Php => "php",
            Language::Scala => "scala",
            Language::Bash => "sh",
            Language::Swift => "swift",
            Language::Zig => "zig",
            Language::Elixir => "ex",
            Language::Lua => "lua",
            Language::Haskell => "hs",
            Language::R => "r",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::Css => "css",
            Language::Html => "html",
            Language::Sql => "sql",
            Language::Markdown => "md",
        }
    }

    /// Per-language allow-list of tree-sitter node kinds that represent an
    /// identifier token bound to a name (variables, functions, types,
    /// fields). This is the symbol-graph projection that drives
    /// `rename_symbol`: anything not in this table is treated as a literal
    /// or punctuation node and left alone, which keeps a rename out of
    /// comments and string bodies even though those *contain* identifier
    /// substrings. `None` means the language has no rename projection yet.
    pub fn rename_identifier_kinds(self) -> Option<&'static [&'static str]> {
        Some(match self {
            Language::Rust => &[
                "identifier",
                "type_identifier",
                "field_identifier",
                "shorthand_field_identifier",
            ],
            Language::TypeScript | Language::Tsx => &[
                "identifier",
                "type_identifier",
                "property_identifier",
                "shorthand_property_identifier",
                "shorthand_property_identifier_pattern",
            ],
            Language::JavaScript | Language::Jsx => &[
                "identifier",
                "property_identifier",
                "shorthand_property_identifier",
                "shorthand_property_identifier_pattern",
            ],
            Language::Python => &["identifier"],
            Language::Go => &[
                "identifier",
                "type_identifier",
                "field_identifier",
                "package_identifier",
            ],
            Language::Swift => &["simple_identifier", "type_identifier"],
            _ => return None,
        })
    }

    /// Whether `rename_symbol` can operate on this language (i.e. it has a
    /// [`Language::rename_identifier_kinds`] projection).
    pub fn supports_rename(self) -> bool {
        self.rename_identifier_kinds().is_some()
    }

    /// Data / markup / config grammars that carry no nameable symbols, so
    /// symbol + outline extraction is intentionally empty for them.
    fn is_data_format(self) -> bool {
        matches!(
            self,
            Language::Json
                | Language::Yaml
                | Language::Toml
                | Language::Css
                | Language::Html
                | Language::Sql
                | Language::Markdown
        )
    }

    /// Whether `symbols`/`outline` produce meaningful results. Data/markup
    /// grammars parse and edit fine but expose no symbol projection.
    pub fn supports_symbol_extraction(self) -> bool {
        !self.is_data_format()
    }

    /// The AST-precise edit capability matrix for this language. See
    /// [`EditCapabilities`].
    pub fn edit_capabilities(self) -> EditCapabilities {
        EditCapabilities {
            apply_node: true,
            insert_at_anchor: true,
            rename_symbol: self.supports_rename(),
            symbols: self.supports_symbol_extraction(),
        }
    }

    /// Every language we ship support for. Useful for tests + introspection.
    pub fn all() -> &'static [Language] {
        &[
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Jsx,
            Language::Python,
            Language::Go,
            Language::Rust,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Ruby,
            Language::Kotlin,
            Language::Php,
            Language::Scala,
            Language::Bash,
            Language::Swift,
            Language::Zig,
            Language::Elixir,
            Language::Lua,
            Language::Haskell,
            Language::R,
            Language::Json,
            Language::Yaml,
            Language::Toml,
            Language::Css,
            Language::Html,
            Language::Sql,
            Language::Markdown,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the all-families (default) build links every grammar; under a
    // trimmed grammar set some languages intentionally resolve to `None`.
    #[cfg(feature = "grammars-all")]
    #[test]
    fn every_language_is_loadable() {
        for &lang in Language::all() {
            // Constructing the tree-sitter Language must not panic and must
            // produce a non-trivial grammar.
            let ts = lang
                .ts_language()
                .unwrap_or_else(|| panic!("{} grammar not compiled", lang.name()));
            assert!(ts.node_kind_count() > 0, "{} grammar is empty", lang.name());
        }
    }

    #[test]
    fn extension_detection_round_trips_canonical_extensions() {
        let cases: &[(&str, Language)] = &[
            ("ts", Language::TypeScript),
            ("tsx", Language::Tsx),
            ("js", Language::JavaScript),
            ("jsx", Language::Jsx),
            ("py", Language::Python),
            ("rs", Language::Rust),
            ("go", Language::Go),
            ("java", Language::Java),
            ("c", Language::C),
            ("cpp", Language::Cpp),
            ("cs", Language::CSharp),
            ("rb", Language::Ruby),
            ("kt", Language::Kotlin),
            ("php", Language::Php),
            ("scala", Language::Scala),
            ("sh", Language::Bash),
            ("swift", Language::Swift),
            ("zig", Language::Zig),
            ("ex", Language::Elixir),
            ("lua", Language::Lua),
            ("hs", Language::Haskell),
            ("r", Language::R),
            ("json", Language::Json),
            ("yaml", Language::Yaml),
            ("yml", Language::Yaml),
            ("toml", Language::Toml),
            ("css", Language::Css),
            ("html", Language::Html),
            ("sql", Language::Sql),
            ("md", Language::Markdown),
        ];
        for (ext, want) in cases {
            assert_eq!(Language::from_extension(ext), Some(*want), "ext {ext}");
        }
    }

    #[test]
    fn name_round_trips_for_every_language() {
        for &lang in Language::all() {
            assert_eq!(Language::from_name(lang.name()), Some(lang));
        }
    }

    #[test]
    fn primary_extension_resolves_back_to_the_language() {
        for &lang in Language::all() {
            assert_eq!(
                Language::from_extension(lang.primary_extension()),
                Some(lang),
                "primary extension for {} does not round-trip",
                lang.name()
            );
        }
    }

    #[test]
    fn detect_prefers_hint_over_extension() {
        let path = std::path::Path::new("foo.ts");
        assert_eq!(Language::detect(path, None), Some(Language::TypeScript));
        assert_eq!(
            Language::detect(path, Some("javascript")),
            Some(Language::JavaScript)
        );
    }

    #[test]
    fn edit_primitives_are_universal_rename_is_gated() {
        for &lang in Language::all() {
            let caps = lang.edit_capabilities();
            assert!(caps.apply_node, "{} should support apply_node", lang.name());
            assert!(
                caps.insert_at_anchor,
                "{} should support insert_at_anchor",
                lang.name()
            );
            assert_eq!(
                caps.rename_symbol,
                lang.rename_identifier_kinds().is_some(),
                "{} rename capability must match its identifier-kind table",
                lang.name()
            );
        }
        // Data/markup formats edit but carry no symbol projection.
        assert!(!Language::Json.edit_capabilities().rename_symbol);
        assert!(!Language::Json.edit_capabilities().symbols);
        assert!(Language::Rust.edit_capabilities().rename_symbol);
        assert!(Language::Rust.edit_capabilities().symbols);
    }
}
