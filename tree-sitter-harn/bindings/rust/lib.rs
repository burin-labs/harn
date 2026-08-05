//! Harn language support for the tree-sitter parsing library.

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_harn() -> *const ();
}

/// The tree-sitter language function for Harn.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_harn) };

/// The content of the generated `node-types.json` file.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// The syntax highlighting query for Harn.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// The language injection query for Harn.
pub const INJECTIONS_QUERY: &str = include_str!("../../queries/injections.scm");

/// The code-folding query for Harn.
pub const FOLDS_QUERY: &str = include_str!("../../queries/folds.scm");

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use tree_sitter::{Query, QueryCursor, StreamingIterator};

    fn crate_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    /// Crate-relative paths of the `.scm` files actually present in `queries/`.
    fn query_files_on_disk() -> BTreeSet<String> {
        std::fs::read_dir(crate_root().join("queries"))
            .expect("read queries directory")
            .map(|entry| entry.expect("read queries entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "scm"))
            .map(|path| {
                format!(
                    "queries/{}",
                    path.file_name().expect("query file name").to_string_lossy()
                )
            })
            .collect()
    }

    /// Query paths `tree-sitter.json` advertises for the grammar. Editors read
    /// this manifest, so anything not listed here does not reach them.
    fn registered_query_paths() -> BTreeSet<String> {
        let manifest = std::fs::read_to_string(crate_root().join("tree-sitter.json"))
            .expect("read tree-sitter.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("parse tree-sitter.json");
        manifest["grammars"]
            .as_array()
            .expect("tree-sitter.json should list grammars")
            .iter()
            .flat_map(|grammar| {
                grammar
                    .as_object()
                    .expect("each grammar should be an object")
                    .values()
            })
            .filter_map(|value| value.as_str())
            .filter(|value| value.ends_with(".scm"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("load Harn parser");
    }

    /// A query file editors never load is dead weight, and a registration with
    /// no file behind it is broken. Deriving both sides from their real sources
    /// rather than a hand-listed set means a query added later is covered the
    /// moment it lands.
    #[test]
    fn every_query_file_is_registered() {
        assert_eq!(query_files_on_disk(), registered_query_paths());
    }

    /// A renamed or removed node type, field name, or token literal stops a
    /// query from compiling. Without this the queries are unverified text and
    /// highlighting, injection, or folding degrades silently in every
    /// tree-sitter editor.
    #[test]
    fn bundled_queries_compile() {
        let language = super::LANGUAGE.into();
        for relative in query_files_on_disk() {
            let source =
                std::fs::read_to_string(crate_root().join(&relative)).expect("read query file");
            if let Err(error) = Query::new(&language, &source) {
                panic!("{relative} should compile against the grammar: {error}");
            }
        }
    }

    #[test]
    fn injection_query_captures_postgres_template_content() {
        let source = r#"fn build_sql(user_id) {
  let q = sql("""
select *
from users
where id = {user_id}
""", {user_id: user_id})
  let named = named_sql("users_by_id", "one", "select * from users where id = {user_id}", {user_id: user_id})
  let raw = query.sql(r"select * from users where name = {name}", {name: "Ada"})
  let other = render("select * from ignored")
}"#;

        let language = super::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).expect("load Harn parser");
        let tree = parser.parse(source, None).expect("parse Harn source");
        assert!(
            !tree.root_node().has_error(),
            "fixture should parse cleanly"
        );

        let query =
            Query::new(&language, super::INJECTIONS_QUERY).expect("injection query should compile");
        let capture_index = query
            .capture_names()
            .iter()
            .position(|name| *name == "injection.content")
            .expect("query should expose injection.content") as u32;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut captured = Vec::new();
        while let Some(query_match) = matches.next() {
            for capture in query_match.captures {
                if capture.index == capture_index {
                    captured.push(capture.node.utf8_text(source.as_bytes()).unwrap());
                }
            }
        }

        assert_eq!(
            captured,
            vec![
                "\nselect *\nfrom users\nwhere id = {user_id}\n",
                "select * from users where id = {user_id}",
                "select * from users where name = {name}",
            ]
        );
    }
}
