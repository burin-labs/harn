use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_typescript() -> *const ();
    fn tree_sitter_tsx() -> *const ();
}

// SAFETY: these functions return the bundled tree-sitter language descriptors.
pub(crate) const LANGUAGE_TYPESCRIPT: LanguageFn =
    unsafe { LanguageFn::from_raw(tree_sitter_typescript) };
pub(crate) const LANGUAGE_TSX: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_tsx) };
