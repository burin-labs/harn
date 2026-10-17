// Demo seed for `edit_rename_symbol`. Cargo never compiles files under
// `assets/`, so this only exists to feed tree-sitter through the
// code-index rebuild.

use rename_demo::Widget;

fn main() {
    let w: Widget = Widget::new();
    let _ = w.size;
}
