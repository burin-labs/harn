// Demo seed for `edit_rename_symbol`. Cargo never compiles files under
// `assets/`, so this only exists to feed tree-sitter through the
// code-index rebuild.

pub struct Widget {
    pub size: u32,
}

impl Widget {
    pub fn new() -> Widget {
        Widget { size: 0 }
    }
}
