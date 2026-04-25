//! Tree-sitter language registry.
//!
//! Mirrors the Swift `TreeSitterLanguage` enum in
//! `~/projects/burin-code/Sources/ASTEngine/TreeSitterIntegration.swift`
//! verbatim. The set of languages, their canonical names, and their file
//! extensions all match Swift exactly so the bridged outputs round-trip
//! across the harn ↔ burin-code boundary without translation. Adding or
//! dropping a language requires a coordinated change in both repos.

use tree_sitter::Language as TsLanguage;

/// Languages with tree-sitter symbol extraction support.
///
/// The string returned by [`Language::name`] is the canonical wire name;
/// callers (and the JSON schemas) refer to languages by that string.
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
        }
    }

    /// Tree-sitter grammar handle. Cheap; the underlying `LANGUAGE`
    /// constants are static.
    pub fn ts_language(self) -> TsLanguage {
        match self {
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::JavaScript | Language::Jsx => tree_sitter_javascript::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::Swift => tree_sitter_swift::LANGUAGE.into(),
            Language::Zig => tree_sitter_zig::LANGUAGE.into(),
            Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            Language::Haskell => tree_sitter_haskell::LANGUAGE.into(),
            Language::R => tree_sitter_r::LANGUAGE.into(),
        }
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
            _ => return None,
        })
    }

    /// Resolve a language from a file extension. The mapping mirrors the
    /// Swift `extensionMap` in `TreeSitterIntegration.swift`.
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
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_is_loadable() {
        for &lang in Language::all() {
            // Constructing the tree-sitter Language must not panic and must
            // produce a non-trivial grammar.
            let ts = lang.ts_language();
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
    fn detect_prefers_hint_over_extension() {
        let path = std::path::Path::new("foo.ts");
        assert_eq!(Language::detect(path, None), Some(Language::TypeScript));
        assert_eq!(
            Language::detect(path, Some("javascript")),
            Some(Language::JavaScript)
        );
    }
}
