//! Small authority-free primitives shared by native and portable runtimes.
//!
//! These functions are the semantic implementation beneath runtime adapters;
//! they do not expose VM values, host handles, or target-specific state.

mod regex;
mod secret_scan;

pub use regex::{
    regex_captures, regex_matches, regex_replace, regex_split, RegexCapture,
    MAX_REGEX_PATTERN_BYTES,
};
pub use secret_scan::{
    compiled_secret_patterns, scan_secrets, secret_patterns_compiled, CompiledSecretPattern,
    SecretFinding,
};

pub fn hex_encode(input: &[u8]) -> String {
    hex::encode(input)
}

pub fn hex_decode_text(input: &str) -> Result<String, String> {
    hex::decode(input.as_bytes())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("hex decode error: {error}"))
}

pub fn trim_text(input: &str) -> &str {
    input.trim()
}

pub fn replace_text(input: &str, old: &str, new: &str) -> String {
    input.replace(old, new)
}

pub fn starts_with_text(input: &str, prefix: &str) -> bool {
    input.starts_with(prefix)
}

pub fn ends_with_text(input: &str, suffix: &str) -> bool {
    input.ends_with(suffix)
}

pub fn sha256_hex(input: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(input))
}

/// Join host-supplied path segments into Harn's target-independent `/` form.
/// Absolute POSIX, UNC, or drive-rooted segments reset the accumulated path,
/// matching `PathBuf::push` without making artifact behavior depend on the
/// machine that compiled the kernel.
pub fn join_path_segments<I, S>(segments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut joined = String::new();
    for segment in segments {
        let segment = segment.as_ref().replace('\\', "/");
        if segment.is_empty() {
            continue;
        }
        let absolute = segment.starts_with('/')
            || segment
                .as_bytes()
                .get(1..3)
                .is_some_and(|suffix| suffix == b":/");
        if absolute {
            joined = segment;
            continue;
        }
        if !joined.is_empty() && !joined.ends_with('/') {
            joined.push('/');
        }
        joined.push_str(segment.trim_start_matches('/'));
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_text_semantics_cover_control_bytes_and_invalid_input() {
        assert_eq!(hex_encode(b"\0\x01\x02"), "000102");
        assert_eq!(hex_decode_text("000102").unwrap(), "\0\x01\x02");
        assert!(hex_decode_text("abc").is_err());
    }

    #[test]
    fn string_primitives_preserve_rust_unicode_semantics() {
        assert_eq!(trim_text(" \tHarn\n"), "Harn");
        assert_eq!(replace_text("aλa", "a", "β"), "βλβ");
        assert!(starts_with_text("Portable Harn", "Portable"));
        assert!(ends_with_text("Portable Harn", "Harn"));
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn path_join_is_target_independent_for_posix_windows_and_unc_roots() {
        assert_eq!(
            join_path_segments(["/repo", ".harn", "state.json"]),
            "/repo/.harn/state.json"
        );
        assert_eq!(
            join_path_segments([r"C:\repo", ".harn", "state.json"]),
            "C:/repo/.harn/state.json"
        );
        assert_eq!(
            join_path_segments([r"\\server\share", "state.json"]),
            "//server/share/state.json"
        );
        assert_eq!(
            join_path_segments(["ignored", "/absolute", "file"]),
            "/absolute/file"
        );
    }
}
