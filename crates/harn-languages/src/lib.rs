//! Canonical file-language registry.
//!
//! One table answers "what language is this path" for every Harn surface:
//! the code index, the project scanner, the file outline tool, the project
//! fingerprint, and the edit compass. Each [`LanguageSpec`] row carries the
//! canonical wire name, accepted aliases, file extensions, exact file names
//! (`Dockerfile`, `Makefile`), a coarse [`LanguageClass`], and the
//! structural (tree-sitter) edit support the host library provides.
//!
//! `harn-hostlib` binds the tree-sitter grammars on top of this table; its
//! tests assert that every row marked structural has a compiled grammar and
//! that the capability columns match the edit builtins' own tables.

use std::path::Path;

/// Every language Harn recognizes by path.
///
/// Variant order matches [`LANGUAGES`]; [`Language::spec`] indexes by it.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    ObjectiveC,
    ObjectiveCpp,
    Dart,
    FSharp,
    Julia,
    Elm,
    Fish,
    Erlang,
    Vue,
    Svelte,
    Scss,
    Less,
    GraphQl,
    Protobuf,
    Xml,
    Mdx,
    ReStructuredText,
    RMarkdown,
    Dockerfile,
    Makefile,
}

/// Coarse role of a language's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageClass {
    /// Programming, scripting, and build languages with nameable symbols.
    Code,
    /// Data, config, query, and schema formats.
    Data,
    /// Document markup and prose.
    Markup,
    /// Stylesheets.
    Style,
}

impl LanguageClass {
    /// Lowercase wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            LanguageClass::Code => "code",
            LanguageClass::Data => "data",
            LanguageClass::Markup => "markup",
            LanguageClass::Style => "style",
        }
    }
}

/// AST-precise edit support for a language with a tree-sitter grammar.
///
/// Query-driven `apply_node` and `insert_at_anchor` work on every grammar,
/// so only the refactorings that need per-language models are columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuralSupport {
    /// Cross-file safe rename via the symbol graph.
    pub rename_symbol: bool,
    /// Move a declaration to another file and rewrite imports and uses.
    pub move_symbol: bool,
    /// Lift a span into a new function and replace it with a call.
    pub extract_function: bool,
    /// Change a function's parameters and rewrite every call site.
    pub change_signature: bool,
}

/// One registry row.
#[derive(Debug, Clone, Copy)]
pub struct LanguageSpec {
    /// The language this row describes.
    pub language: Language,
    /// Canonical lowercase wire name.
    pub name: &'static str,
    /// Other names [`Language::from_name`] accepts.
    pub aliases: &'static [&'static str],
    /// Lowercase extensions without the dot. The first is the primary one.
    pub extensions: &'static [&'static str],
    /// Lowercase exact file names for extensionless files.
    pub file_names: &'static [&'static str],
    /// Coarse role of the language's files.
    pub class: LanguageClass,
    /// Tree-sitter support, or `None` when no grammar is registered.
    pub structural: Option<StructuralSupport>,
}

const GRAMMAR_ONLY: Option<StructuralSupport> = Some(StructuralSupport {
    rename_symbol: false,
    move_symbol: false,
    extract_function: false,
    change_signature: false,
});

const RENAME_ONLY: Option<StructuralSupport> = Some(StructuralSupport {
    rename_symbol: true,
    move_symbol: false,
    extract_function: false,
    change_signature: false,
});

const REFACTOR_NO_SIGNATURE: Option<StructuralSupport> = Some(StructuralSupport {
    rename_symbol: true,
    move_symbol: true,
    extract_function: true,
    change_signature: false,
});

const REFACTOR_ALL: Option<StructuralSupport> = Some(StructuralSupport {
    rename_symbol: true,
    move_symbol: true,
    extract_function: true,
    change_signature: true,
});

const fn row(
    language: Language,
    name: &'static str,
    aliases: &'static [&'static str],
    extensions: &'static [&'static str],
    class: LanguageClass,
    structural: Option<StructuralSupport>,
) -> LanguageSpec {
    LanguageSpec {
        language,
        name,
        aliases,
        extensions,
        file_names: &[],
        class,
        structural,
    }
}

const fn named_files(mut spec: LanguageSpec, file_names: &'static [&'static str]) -> LanguageSpec {
    spec.file_names = file_names;
    spec
}

use LanguageClass::{Code, Data, Markup, Style};

/// The registry, in [`Language`] variant order.
pub static LANGUAGES: &[LanguageSpec] = &[
    row(Language::Harn, "harn", &[], &["harn"], Code, RENAME_ONLY),
    row(
        Language::TypeScript,
        "typescript",
        &["ts"],
        &["ts"],
        Code,
        REFACTOR_ALL,
    ),
    row(Language::Tsx, "tsx", &[], &["tsx"], Code, REFACTOR_ALL),
    row(
        Language::JavaScript,
        "javascript",
        &["js"],
        &["js", "mjs", "cjs"],
        Code,
        REFACTOR_NO_SIGNATURE,
    ),
    row(
        Language::Jsx,
        "jsx",
        &[],
        &["jsx"],
        Code,
        REFACTOR_NO_SIGNATURE,
    ),
    row(
        Language::Python,
        "python",
        &["py"],
        &["py", "pyi"],
        Code,
        REFACTOR_ALL,
    ),
    row(Language::Go, "go", &["golang"], &["go"], Code, RENAME_ONLY),
    row(Language::Rust, "rust", &["rs"], &["rs"], Code, REFACTOR_ALL),
    row(Language::Java, "java", &[], &["java"], Code, GRAMMAR_ONLY),
    row(Language::C, "c", &[], &["c", "h"], Code, GRAMMAR_ONLY),
    row(
        Language::Cpp,
        "cpp",
        &["c++", "cxx"],
        &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(
        Language::CSharp,
        "csharp",
        &["c#", "cs"],
        &["cs", "csx"],
        Code,
        GRAMMAR_ONLY,
    ),
    named_files(
        row(Language::Ruby, "ruby", &["rb"], &["rb"], Code, GRAMMAR_ONLY),
        &["rakefile"],
    ),
    row(
        Language::Kotlin,
        "kotlin",
        &["kt"],
        &["kt", "kts"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(Language::Php, "php", &[], &["php"], Code, GRAMMAR_ONLY),
    row(
        Language::Scala,
        "scala",
        &[],
        &["scala", "sc"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(
        Language::Bash,
        "bash",
        &["shell", "sh", "zsh"],
        &["sh", "bash", "zsh"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(Language::Swift, "swift", &[], &["swift"], Code, RENAME_ONLY),
    row(
        Language::Zig,
        "zig",
        &[],
        &["zig", "zon"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(
        Language::Elixir,
        "elixir",
        &["ex"],
        &["ex", "exs"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(Language::Lua, "lua", &[], &["lua"], Code, GRAMMAR_ONLY),
    row(
        Language::Haskell,
        "haskell",
        &["hs"],
        &["hs", "lhs"],
        Code,
        GRAMMAR_ONLY,
    ),
    row(Language::R, "r", &[], &["r"], Code, GRAMMAR_ONLY),
    row(Language::Json, "json", &[], &["json"], Data, GRAMMAR_ONLY),
    row(
        Language::Yaml,
        "yaml",
        &["yml"],
        &["yaml", "yml"],
        Data,
        GRAMMAR_ONLY,
    ),
    row(Language::Toml, "toml", &[], &["toml"], Data, GRAMMAR_ONLY),
    row(Language::Css, "css", &[], &["css"], Style, GRAMMAR_ONLY),
    row(
        Language::Html,
        "html",
        &["htm"],
        &["html", "htm"],
        Markup,
        GRAMMAR_ONLY,
    ),
    row(Language::Sql, "sql", &[], &["sql"], Data, GRAMMAR_ONLY),
    row(
        Language::Markdown,
        "markdown",
        &["md"],
        &["md", "markdown"],
        Markup,
        GRAMMAR_ONLY,
    ),
    row(
        Language::ObjectiveC,
        "objective-c",
        &["objc", "objectivec"],
        &["m"],
        Code,
        None,
    ),
    row(
        Language::ObjectiveCpp,
        "objective-cpp",
        &["objc++", "objective-c++", "objcpp"],
        &["mm"],
        Code,
        None,
    ),
    row(Language::Dart, "dart", &[], &["dart"], Code, None),
    row(
        Language::FSharp,
        "fsharp",
        &["f#"],
        &["fs", "fsx"],
        Code,
        None,
    ),
    row(Language::Julia, "julia", &["jl"], &["jl"], Code, None),
    row(Language::Elm, "elm", &[], &["elm"], Code, None),
    row(Language::Fish, "fish", &[], &["fish"], Code, None),
    row(
        Language::Erlang,
        "erlang",
        &["erl"],
        &["erl", "hrl"],
        Code,
        None,
    ),
    row(Language::Vue, "vue", &[], &["vue"], Code, None),
    row(Language::Svelte, "svelte", &[], &["svelte"], Code, None),
    row(Language::Scss, "scss", &[], &["scss"], Style, None),
    row(Language::Less, "less", &[], &["less"], Style, None),
    row(
        Language::GraphQl,
        "graphql",
        &["gql"],
        &["graphql"],
        Data,
        None,
    ),
    row(
        Language::Protobuf,
        "protobuf",
        &["proto"],
        &["proto"],
        Data,
        None,
    ),
    row(Language::Xml, "xml", &[], &["xml"], Markup, None),
    row(Language::Mdx, "mdx", &[], &["mdx"], Markup, None),
    row(
        Language::ReStructuredText,
        "restructuredtext",
        &["rst"],
        &["rst"],
        Markup,
        None,
    ),
    row(
        Language::RMarkdown,
        "rmarkdown",
        &["rmd"],
        &["rmd"],
        Markup,
        None,
    ),
    named_files(
        row(
            Language::Dockerfile,
            "dockerfile",
            &["docker"],
            &["dockerfile"],
            Code,
            None,
        ),
        &["dockerfile"],
    ),
    named_files(
        row(Language::Makefile, "makefile", &["make"], &[], Code, None),
        &["makefile"],
    ),
];

impl Language {
    /// This language's registry row.
    pub fn spec(self) -> &'static LanguageSpec {
        &LANGUAGES[self as usize]
    }

    /// Every registered language, in registry order.
    pub fn all() -> impl Iterator<Item = Language> {
        LANGUAGES.iter().map(|spec| spec.language)
    }

    /// Canonical lowercase wire name.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// Coarse role of this language's files.
    pub fn class(self) -> LanguageClass {
        self.spec().class
    }

    /// Lowercase extensions without the dot; the first is the primary one.
    pub fn extensions(self) -> &'static [&'static str] {
        self.spec().extensions
    }

    /// Tree-sitter edit support, or `None` when no grammar is registered.
    pub fn structural(self) -> Option<StructuralSupport> {
        self.spec().structural
    }

    /// Resolve a canonical name or alias, ignoring case and surrounding
    /// whitespace.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        LANGUAGES
            .iter()
            .find(|spec| {
                spec.name.eq_ignore_ascii_case(name)
                    || spec.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
            })
            .map(|spec| spec.language)
    }

    /// Resolve a file extension, with or without the leading dot.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.strip_prefix('.').unwrap_or(extension);
        if extension.is_empty() {
            return None;
        }
        LANGUAGES
            .iter()
            .find(|spec| {
                spec.extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(extension))
            })
            .map(|spec| spec.language)
    }

    /// Resolve an exact file name such as `Dockerfile` or `Makefile`.
    pub fn from_file_name(file_name: &str) -> Option<Self> {
        LANGUAGES
            .iter()
            .find(|spec| {
                spec.file_names
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(file_name))
            })
            .map(|spec| spec.language)
    }

    /// Resolve a path: an exact file name match wins, then the extension.
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        let file_name = path.file_name().and_then(|name| name.to_str())?;
        Self::from_file_name(file_name).or_else(|| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .and_then(Self::from_extension)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_in_variant_order() {
        for (index, spec) in LANGUAGES.iter().enumerate() {
            assert_eq!(
                spec.language as usize, index,
                "{} is out of order",
                spec.name
            );
        }
        assert_eq!(Language::Makefile as usize + 1, LANGUAGES.len());
    }

    #[test]
    fn names_aliases_extensions_and_file_names_are_unique_and_lowercase() {
        let mut names = std::collections::HashSet::new();
        let mut extensions = std::collections::HashSet::new();
        let mut file_names = std::collections::HashSet::new();
        for spec in LANGUAGES {
            for name in std::iter::once(&spec.name).chain(spec.aliases) {
                assert_eq!(*name, name.to_ascii_lowercase());
                assert!(names.insert(*name), "name or alias `{name}` is ambiguous");
            }
            for ext in spec.extensions {
                assert!(!ext.starts_with('.') && *ext == ext.to_ascii_lowercase());
                assert!(extensions.insert(*ext), "extension `{ext}` is ambiguous");
            }
            for file_name in spec.file_names {
                assert_eq!(*file_name, file_name.to_ascii_lowercase());
                assert!(file_names.insert(*file_name));
            }
            assert!(
                !spec.extensions.is_empty() || !spec.file_names.is_empty(),
                "{} is unreachable by path",
                spec.name
            );
        }
    }

    #[test]
    fn every_language_round_trips_through_name_and_paths() {
        for language in Language::all() {
            assert_eq!(Language::from_name(language.name()), Some(language));
            for ext in language.extensions() {
                assert_eq!(
                    Language::from_path(format!("dir/file.{ext}")),
                    Some(language)
                );
            }
            for file_name in language.spec().file_names {
                assert_eq!(
                    Language::from_path(format!("dir/{file_name}")),
                    Some(language)
                );
            }
        }
    }

    #[test]
    fn path_detection_is_case_insensitive_and_prefers_file_names() {
        assert_eq!(Language::from_path("src/App.TSX"), Some(Language::Tsx));
        assert_eq!(
            Language::from_path("Dockerfile"),
            Some(Language::Dockerfile)
        );
        assert_eq!(
            Language::from_path("GNU/Makefile"),
            Some(Language::Makefile)
        );
        assert_eq!(Language::from_path("Rakefile"), Some(Language::Ruby));
        assert_eq!(
            Language::from_path("x.dockerfile"),
            Some(Language::Dockerfile)
        );
        assert_eq!(Language::from_extension(".kts"), Some(Language::Kotlin));
        assert_eq!(Language::from_name(" Shell "), Some(Language::Bash));
    }

    #[test]
    fn unknown_inputs_resolve_to_none() {
        assert_eq!(Language::from_path("notes.xyz"), None);
        assert_eq!(Language::from_path("README"), None);
        assert_eq!(Language::from_path(""), None);
        assert_eq!(Language::from_extension(""), None);
        assert_eq!(Language::from_extension("."), None);
        assert_eq!(Language::from_name("plaintext"), None);
    }
}
