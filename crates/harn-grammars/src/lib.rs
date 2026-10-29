//! Tree-sitter grammars for Harn.
//!
//! Each function returns its grammar's [`Language`]. Parse tables are most of
//! a grammar's size and compress about 40 to 1, so the build script stores
//! them compressed and the first call for a grammar unpacks them into memory,
//! in a few milliseconds. Later calls are free.
//!
//! Enable a grammar with the feature of the same name; `typescript` provides
//! both [`typescript`] and [`tsx`].

#[cfg(harn_grammars_packed)]
mod packed;
#[cfg(all(feature = "typescript", any(not(harn_grammars_packed), test)))]
mod typescript;

pub use tree_sitter::Language;

macro_rules! grammars {
    ($($feature:literal => $name:ident, $packed:ident, $upstream:path;)*) => {
        $(
            #[doc = concat!("The `", stringify!($name), "` grammar.")]
            #[cfg(feature = $feature)]
            #[must_use]
            pub fn $name() -> Language {
                #[cfg(harn_grammars_packed)]
                {
                    packed::$packed.language()
                }
                #[cfg(not(harn_grammars_packed))]
                {
                    $upstream.into()
                }
            }
        )*
    };
}

grammars! {
    "bash" => bash, BASH, tree_sitter_bash::LANGUAGE;
    "c" => c, C, tree_sitter_c::LANGUAGE;
    "c-sharp" => c_sharp, C_SHARP, tree_sitter_c_sharp::LANGUAGE;
    "cpp" => cpp, CPP, tree_sitter_cpp::LANGUAGE;
    "css" => css, CSS, tree_sitter_css::LANGUAGE;
    "elixir" => elixir, ELIXIR, tree_sitter_elixir::LANGUAGE;
    "go" => go, GO, tree_sitter_go::LANGUAGE;
    "harn" => harn, HARN, tree_sitter_harn::LANGUAGE;
    "haskell" => haskell, HASKELL, tree_sitter_haskell::LANGUAGE;
    "html" => html, HTML, tree_sitter_html::LANGUAGE;
    "java" => java, JAVA, tree_sitter_java::LANGUAGE;
    "javascript" => javascript, JAVASCRIPT, tree_sitter_javascript::LANGUAGE;
    "json" => json, JSON, tree_sitter_json::LANGUAGE;
    "kotlin" => kotlin, KOTLIN, tree_sitter_kotlin_ng::LANGUAGE;
    "lua" => lua, LUA, tree_sitter_lua::LANGUAGE;
    "markdown" => markdown, MARKDOWN, tree_sitter_md::LANGUAGE;
    "php" => php, PHP, tree_sitter_php::LANGUAGE_PHP;
    "powershell" => powershell, POWERSHELL, tree_sitter_powershell::LANGUAGE;
    "python" => python, PYTHON, tree_sitter_python::LANGUAGE;
    "r" => r, R, tree_sitter_r::LANGUAGE;
    "ruby" => ruby, RUBY, tree_sitter_ruby::LANGUAGE;
    "rust" => rust, RUST, tree_sitter_rust::LANGUAGE;
    "scala" => scala, SCALA, tree_sitter_scala::LANGUAGE;
    "sql" => sql, SQL, tree_sitter_sequel::LANGUAGE;
    "swift" => swift, SWIFT, tree_sitter_swift::LANGUAGE;
    "toml" => toml, TOML, tree_sitter_toml_ng::LANGUAGE;
    "typescript" => typescript, TYPESCRIPT, typescript::LANGUAGE_TYPESCRIPT;
    "typescript" => tsx, TSX, typescript::LANGUAGE_TSX;
    "yaml" => yaml, YAML, tree_sitter_yaml::LANGUAGE;
    "zig" => zig, ZIG, tree_sitter_zig::LANGUAGE;
}

#[cfg(test)]
mod tests;
