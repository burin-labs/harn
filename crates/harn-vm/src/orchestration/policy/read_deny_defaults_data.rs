//! The default credential denylist: the data file, and the one function
//! that parses it.
//!
//! Split out of `types.rs` to keep that file under the 1500-line source cap.

/// Credential material that is denied to confined children by default.
///
/// The list is DATA (`read_deny_defaults.toml`), not a Rust const, so adding a
/// path is a reviewable one-line diff in a file whose only job is to say what
/// belongs on it. Parsed once.
///
/// Every entry sits under a directory some preset already grants, which is the
/// whole reason the list has to beat presets rather than compete with them.
/// A host may add denials; these are not removable through configuration.
static READ_DENY_DEFAULTS_TOML: &str = include_str!("read_deny_defaults.toml");

/// The parsed default denylist, home-relative.
///
/// A parse failure or an empty list is a HARD ERROR, not a silent empty
/// default. Every other outcome here degrades safely; this one degrades into
/// "no credentials are denied" while every signature of a working denylist
/// (the field exists, the profile renders, the tests that do not check content
/// pass) stays intact. That is the shape that reads as success while being the
/// exact failure.
pub fn default_read_deny_home_paths() -> &'static [String] {
    static PARSED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        let parsed: toml::Value = toml::from_str(READ_DENY_DEFAULTS_TOML)
            .expect("read_deny_defaults.toml must parse; the sandbox denylist is not optional");
        let entries = parsed
            .get("defaults")
            .and_then(|table| table.get("home_relative"))
            .and_then(|value| value.as_array())
            .expect("read_deny_defaults.toml must define defaults.home_relative as an array");
        let paths: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect();
        assert!(
            paths.len() == entries.len(),
            "every read_deny_defaults.toml entry must be a string"
        );
        assert!(
            !paths.is_empty(),
            "read_deny_defaults.toml parsed to an EMPTY denylist; refusing to run with no \
             credential denials rather than silently granting them"
        );
        paths
    })
}
