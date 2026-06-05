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

#[cfg(test)]
mod tests {
    use tree_sitter::{Query, QueryCursor, StreamingIterator};

    #[test]
    fn can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("load Harn parser");
    }

    #[test]
    fn bundled_queries_compile() {
        let language = super::LANGUAGE.into();
        tree_sitter::Query::new(&language, super::HIGHLIGHTS_QUERY)
            .expect("highlight query should compile");
        tree_sitter::Query::new(&language, super::INJECTIONS_QUERY)
            .expect("injection query should compile");
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
