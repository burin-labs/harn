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

#[cfg(test)]
mod tests {
    #[test]
    fn can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("load Harn parser");
    }
}
