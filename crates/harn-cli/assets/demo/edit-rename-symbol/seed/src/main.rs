// Demo seed for `edit_rename_symbol`. Cargo never compiles files under
// `assets/`, so this only exists to feed tree-sitter through the
// code-index rebuild.

use crate::Widget;

fn main() {
    let w: Widget = Widget::new();
    println!("{}", w.size);
}
