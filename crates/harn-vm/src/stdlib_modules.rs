/// Embedded standard library modules.
///
/// Each module is a `.harn` source file compiled into the binary via `include_str!`.
/// They are only parsed/executed when a script does `import "std/<module>"`.
pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    match module {
        "text" => Some(include_str!("../../../stdlib/text.harn")),
        "collections" => Some(include_str!("../../../stdlib/collections.harn")),
        _ => None,
    }
}
