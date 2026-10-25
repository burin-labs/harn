//! Tree-sitter grammar binding over the canonical language registry.
//!
//! Names, aliases, extensions, file names, language class, and the
//! structural capability columns live in the `harn-languages` registry,
//! which every Harn surface shares. This module binds a tree-sitter grammar
//! to the registry rows marked structural; a test keeps the two in step.
//!
//! ## Per-language onboarding contract (B.7)
//!
//! 1. **registry row** — names, extensions, class, and capability columns
//!    in `harn_languages::LANGUAGES`
//! 2. **grammar binding** — a [`Language`] variant, its
//!    [`Language::registry`] arm, and [`Language::ts_language`]
//! 3. **symbol-graph projection** (drives `rename_symbol`) —
//!    [`Language::rename_identifier_kinds`]
//! 4. **symbol/outline extraction** — `ast::symbols::extract`
//! 5. **test fixture** — `tests/fixtures/ast/<name>/`
//!
//! Format-preserving span replacement and trivia/indentation handling are
//! grammar-agnostic (byte-span splice + inferred indent), so they need no
//! per-language code.

use harn_languages::Language as RegistryLanguage;
use harn_languages::{LanguageClass, StructuralSupport};
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
    Harn,
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
///
/// Non-exhaustive so a new column is not a breaking change for readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EditCapabilities {
    /// Tree-sitter query → format-preserving replace.
    pub apply_node: bool,
    /// Anchored sibling/child insertion.
    pub insert_at_anchor: bool,
    /// Cross-file safe rename via the symbol graph.
    pub rename_symbol: bool,
    /// Symbol + outline extraction.
    pub symbols: bool,
    /// Move a declaration to another file and rewrite its imports and
    /// qualified uses.
    pub move_symbol: bool,
    /// Lift a span into a new function and replace it with a call.
    pub extract_function: bool,
    /// Change a function's parameters and rewrite every call site.
    pub change_signature: bool,
}

impl Language {
    /// The registry row this grammar binds. Every name, extension, and
    /// capability column below reads from it.
    pub fn registry(self) -> RegistryLanguage {
        match self {
            Language::Harn => RegistryLanguage::Harn,
            Language::TypeScript => RegistryLanguage::TypeScript,
            Language::Tsx => RegistryLanguage::Tsx,
            Language::JavaScript => RegistryLanguage::JavaScript,
            Language::Jsx => RegistryLanguage::Jsx,
            Language::Python => RegistryLanguage::Python,
            Language::Go => RegistryLanguage::Go,
            Language::Rust => RegistryLanguage::Rust,
            Language::Java => RegistryLanguage::Java,
            Language::C => RegistryLanguage::C,
            Language::Cpp => RegistryLanguage::Cpp,
            Language::CSharp => RegistryLanguage::CSharp,
            Language::Ruby => RegistryLanguage::Ruby,
            Language::Kotlin => RegistryLanguage::Kotlin,
            Language::Php => RegistryLanguage::Php,
            Language::Scala => RegistryLanguage::Scala,
            Language::Bash => RegistryLanguage::Bash,
            Language::Swift => RegistryLanguage::Swift,
            Language::Zig => RegistryLanguage::Zig,
            Language::Elixir => RegistryLanguage::Elixir,
            Language::Lua => RegistryLanguage::Lua,
            Language::Haskell => RegistryLanguage::Haskell,
            Language::R => RegistryLanguage::R,
            Language::Json => RegistryLanguage::Json,
            Language::Yaml => RegistryLanguage::Yaml,
            Language::Toml => RegistryLanguage::Toml,
            Language::Css => RegistryLanguage::Css,
            Language::Html => RegistryLanguage::Html,
            Language::Sql => RegistryLanguage::Sql,
            Language::Markdown => RegistryLanguage::Markdown,
        }
    }

    /// The grammar-backed language for a registry row, or `None` when the
    /// registry knows the language but no tree-sitter grammar is bound.
    pub fn from_registry(language: RegistryLanguage) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|lang| lang.registry() == language)
    }

    /// Canonical wire name.
    pub fn name(self) -> &'static str {
        self.registry().name()
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
    /// The first call for a grammar unpacks its parse tables; later calls
    /// are cheap.
    pub fn ts_language(self) -> Option<TsLanguage> {
        // Return `Option` from each arm so a build with zero `grammar-*`
        // features (lean `ast` only) stays warning-clean: the catch-all is
        // then the sole reachable arm rather than an unreachable `Some(...)`
        // wrapper around `return None`.
        match self {
            #[cfg(feature = "grammar-harn")]
            Language::Harn => Some(harn_grammars::harn()),

            #[cfg(feature = "grammar-web")]
            Language::TypeScript => Some(harn_grammars::typescript()),
            #[cfg(feature = "grammar-web")]
            Language::Tsx => Some(harn_grammars::tsx()),
            #[cfg(feature = "grammar-web")]
            Language::JavaScript | Language::Jsx => Some(harn_grammars::javascript()),
            #[cfg(feature = "grammar-web")]
            Language::Html => Some(harn_grammars::html()),
            #[cfg(feature = "grammar-web")]
            Language::Css => Some(harn_grammars::css()),

            #[cfg(feature = "grammar-systems")]
            Language::Rust => Some(harn_grammars::rust()),
            #[cfg(feature = "grammar-systems")]
            Language::C => Some(harn_grammars::c()),
            #[cfg(feature = "grammar-systems")]
            Language::Cpp => Some(harn_grammars::cpp()),
            #[cfg(feature = "grammar-systems")]
            Language::Go => Some(harn_grammars::go()),
            #[cfg(feature = "grammar-systems")]
            Language::Zig => Some(harn_grammars::zig()),

            #[cfg(feature = "grammar-scripting")]
            Language::Python => Some(harn_grammars::python()),
            #[cfg(feature = "grammar-scripting")]
            Language::Ruby => Some(harn_grammars::ruby()),
            #[cfg(feature = "grammar-scripting")]
            Language::Bash => Some(harn_grammars::bash()),
            #[cfg(feature = "grammar-scripting")]
            Language::Lua => Some(harn_grammars::lua()),
            #[cfg(feature = "grammar-scripting")]
            Language::Php => Some(harn_grammars::php()),
            #[cfg(feature = "grammar-scripting")]
            Language::R => Some(harn_grammars::r()),

            #[cfg(feature = "grammar-jvm")]
            Language::Java => Some(harn_grammars::java()),
            #[cfg(feature = "grammar-jvm")]
            Language::Kotlin => Some(harn_grammars::kotlin()),
            #[cfg(feature = "grammar-jvm")]
            Language::Scala => Some(harn_grammars::scala()),

            #[cfg(feature = "grammar-enterprise")]
            Language::CSharp => Some(harn_grammars::c_sharp()),
            #[cfg(feature = "grammar-enterprise")]
            Language::Swift => Some(harn_grammars::swift()),
            #[cfg(feature = "grammar-enterprise")]
            Language::Elixir => Some(harn_grammars::elixir()),
            #[cfg(feature = "grammar-enterprise")]
            Language::Haskell => Some(harn_grammars::haskell()),

            #[cfg(feature = "grammar-data")]
            Language::Json => Some(harn_grammars::json()),
            #[cfg(feature = "grammar-data")]
            Language::Yaml => Some(harn_grammars::yaml()),
            #[cfg(feature = "grammar-data")]
            Language::Toml => Some(harn_grammars::toml()),
            #[cfg(feature = "grammar-data")]
            Language::Sql => Some(harn_grammars::sql()),
            // tree-sitter-md ships a split block/inline grammar; the block
            // grammar is the structural tree the edit primitives operate
            // on (headings, lists, fenced code, …).
            #[cfg(feature = "grammar-data")]
            Language::Markdown => Some(harn_grammars::markdown()),

            // Any language whose family was not compiled into this build.
            // Unreachable under the default (all-families) build.
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Resolve a language from its canonical wire name or a registry
    /// alias (`ts`, `js`, `c++`, `shell`, …).
    pub fn from_name(name: &str) -> Option<Self> {
        RegistryLanguage::from_name(name).and_then(Self::from_registry)
    }

    /// Resolve a language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        RegistryLanguage::from_extension(ext).and_then(Self::from_registry)
    }

    /// Resolve from a file path: prefer explicit `language_hint` if
    /// supplied, otherwise fall back to registry path detection.
    pub fn detect(path: &std::path::Path, language_hint: Option<&str>) -> Option<Self> {
        if let Some(name) = language_hint.and_then(|s| (!s.is_empty()).then_some(s)) {
            return Self::from_name(name);
        }
        RegistryLanguage::from_path(path).and_then(Self::from_registry)
    }

    /// A representative file extension for the language (no leading dot).
    /// Used by docs and the onboarding probe; not necessarily the only
    /// extension [`Language::from_extension`] accepts.
    pub fn primary_extension(self) -> &'static str {
        self.registry().extensions()[0]
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
            Language::Harn => &["identifier"],
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
            Language::Python | Language::Java => &["identifier"],
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

    fn structural(self) -> StructuralSupport {
        self.registry()
            .structural()
            .expect("every grammar-backed language has a structural registry row")
    }

    /// Whether `rename_symbol` can operate on this language. A unit test
    /// keeps the registry column equal to
    /// [`Language::rename_identifier_kinds`].
    pub fn supports_rename(self) -> bool {
        self.structural().rename_symbol
    }

    /// Whether `code_index.change_signature` has a signature grammar for this
    /// language. A unit test keeps it equal to the builtin's own table.
    pub fn supports_change_signature(self) -> bool {
        self.structural().change_signature
    }

    /// Whether `code_index.extract_function` can operate on this language:
    /// it needs a free-name profile and a function form it can synthesize.
    /// Mirrors `code_index::extract_plan::Dialect::of`, which a test pins.
    pub fn supports_extract_function(self) -> bool {
        self.structural().extract_function
    }

    /// Whether `code_index.move_symbol` has an import and module-path model
    /// for this language.
    pub fn supports_move_symbol(self) -> bool {
        self.structural().move_symbol
    }

    /// Whether `symbols`/`outline` produce meaningful results. Data, markup,
    /// and style grammars parse and edit fine but expose no symbol
    /// projection.
    pub fn supports_symbol_extraction(self) -> bool {
        self.is_code()
    }

    /// Whether the grammar is a programming language, as opposed to data,
    /// markup, or style.
    pub fn is_code(self) -> bool {
        self.registry().class() == LanguageClass::Code
    }

    /// The AST-precise edit capability matrix for this language. See
    /// [`EditCapabilities`].
    pub fn edit_capabilities(self) -> EditCapabilities {
        EditCapabilities {
            apply_node: true,
            insert_at_anchor: true,
            rename_symbol: self.supports_rename(),
            symbols: self.supports_symbol_extraction(),
            // Graph-grounded refactorings.
            move_symbol: self.supports_move_symbol(),
            extract_function: self.supports_extract_function(),
            change_signature: self.supports_change_signature(),
        }
    }

    /// Every language we ship support for. Useful for tests + introspection.
    pub fn all() -> &'static [Language] {
        &[
            Language::Harn,
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
            ("harn", Language::Harn),
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
    fn grammar_bindings_match_the_registry_structural_rows() {
        for registry in RegistryLanguage::all() {
            assert_eq!(
                Language::from_registry(registry).is_some(),
                registry.structural().is_some(),
                "{} grammar binding disagrees with its registry row",
                registry.name()
            );
        }
        for &lang in Language::all() {
            assert_eq!(Language::from_registry(lang.registry()), Some(lang));
            for ext in lang.registry().extensions() {
                assert_eq!(Language::from_extension(ext), Some(lang), "ext {ext}");
            }
        }
        assert_eq!(Language::from_extension("dart"), None);
        assert_eq!(Language::from_name("dockerfile"), None);
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
