/// Embedded standard library modules.
pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    harn_stdlib::get_stdlib_source(module)
}

/// Embedded stdlib prompt assets, addressed as `std/<path>.harn.prompt`.
pub fn get_stdlib_prompt_asset(path: &str) -> Option<&'static str> {
    harn_stdlib::get_stdlib_prompt_asset(path)
}
