/// Embedded standard library modules.
pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    harn_stdlib::get_stdlib_source(module)
}
