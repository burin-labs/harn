/// Embedded standard library modules.
///
/// Each module is a `.harn` source file compiled into the binary via `include_str!`.
/// They are only parsed/executed when a script does `import "std/<module>"`.
pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    match module {
        "text" => Some(include_str!("stdlib_text.harn")),
        "collections" => Some(include_str!("stdlib_collections.harn")),
        "math" => Some(include_str!("stdlib_math.harn")),
        "path" => Some(include_str!("stdlib_path.harn")),
        "json" => Some(include_str!("stdlib_json.harn")),
        "async" => Some(include_str!("stdlib_async.harn")),
        _ => None,
    }
}
