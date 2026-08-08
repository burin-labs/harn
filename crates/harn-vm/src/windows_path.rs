//! Windows extended-length (`\\?\` verbatim) path semantics, owned in one
//! place so the `\\?\` / `\\?\UNC\` prefix rules live at a single source of
//! truth.
//!
//! Two paired directions:
//! - [`strip_windows_verbatim_prefix`] removes the prefix so a canonicalized
//!   path parses under normal Windows rules (and can re-enter `/`-joining
//!   code, or prefix-match a plain `C:\` mount point). It is pure string logic
//!   and cross-platform: a well-formed absolute path off Windows never carries
//!   the prefix, so callers on any platform may normalize a Windows-shaped
//!   string through it.
//! - [`wide_maybe_verbatim`] adds the prefix to a `MoveFileExW` operand so a
//!   durable write survives a path longer than the legacy 260-char `MAX_PATH`.
//!   It mirrors the standard library's conservative `maybe_verbatim` rules and
//!   is Windows-only.

use std::borrow::Cow;

/// Strip a Windows verbatim (`\\?\`) prefix from a path string:
/// `\\?\UNC\server\share` -> `\\server\share`, `\\?\C:\dir` -> `C:\dir`. Inputs
/// without the prefix — every well-formed Unix path, plain Windows paths, and
/// `\\.\` device paths — are returned unchanged.
pub fn strip_windows_verbatim_prefix(text: &str) -> Cow<'_, str> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        Cow::Borrowed(rest)
    } else {
        Cow::Borrowed(text)
    }
}

/// Build a NUL-terminated UTF-16 buffer for `path`, prepending the `\\?\`
/// extended-length prefix so `MoveFileExW` accepts paths longer than the legacy
/// 260-char `MAX_PATH`.
///
/// std applies this automatically for its own file APIs (`File::open`,
/// `create_dir_all`), but a hand-rolled `MoveFileExW` call does not, so any
/// operand over the limit fails with `ERROR_PATH_NOT_FOUND`. Mirroring std's
/// conservative rules, only drive/UNC-absolute paths already in backslash
/// normal form are rewritten; a verbatim/device path, a relative path, or one
/// containing `/`, `.`, or `..` (which a verbatim prefix would treat literally)
/// is passed through unchanged.
#[cfg(windows)]
pub(crate) fn wide_maybe_verbatim(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};

    fn nul_terminated(mut wide: Vec<u16>) -> Vec<u16> {
        wide.push(0);
        wide
    }

    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    const BACKSLASH: u16 = b'\\' as u16;
    const SLASH: u16 = b'/' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    // Already verbatim (`\\?\`) or a device path (`\\.\`): leave untouched.
    if raw.starts_with(&[BACKSLASH, BACKSLASH, QUESTION, BACKSLASH])
        || raw.starts_with(&[BACKSLASH, BACKSLASH, DOT, BACKSLASH])
    {
        return nul_terminated(raw);
    }
    // A forward slash is a literal filename character under a verbatim prefix,
    // so such paths are ineligible.
    if raw.contains(&SLASH) {
        return nul_terminated(raw);
    }
    let mut components = path.components();
    let is_supported_prefix = matches!(
        components.next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _))
    );
    if !is_supported_prefix || !matches!(components.next(), Some(Component::RootDir)) {
        return nul_terminated(raw);
    }
    // `.`/`..` components would resolve literally under a verbatim prefix.
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return nul_terminated(raw);
    }
    // Re-read the prefix kind to choose the correct verbatim form.
    let prefix_kind = path
        .components()
        .next()
        .and_then(|component| match component {
            Component::Prefix(prefix) => Some(prefix.kind()),
            _ => None,
        });
    let prefixed = match prefix_kind {
        // `C:\...` -> `\\?\C:\...`
        Some(Prefix::Disk(_)) => {
            let mut out: Vec<u16> = r"\\?\".encode_utf16().collect();
            out.extend_from_slice(&raw);
            out
        }
        // `\\server\share\...` -> `\\?\UNC\server\share\...` (drop one leading `\`)
        Some(Prefix::UNC(_, _)) => {
            let mut out: Vec<u16> = r"\\?\UNC".encode_utf16().collect();
            out.extend_from_slice(&raw[1..]);
            out
        }
        _ => raw,
    };
    nul_terminated(prefixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_drive_and_unc_verbatim_prefixes() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\C:\Users\runner\Temp\harn-abc"),
            r"C:\Users\runner\Temp\harn-abc"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\dir"),
            r"\\server\share\dir"
        );
    }

    #[test]
    fn passes_through_non_verbatim_paths_unchanged() {
        // Plain Windows, every Unix path, `\\.\` device paths, relative paths,
        // slash-bearing and dot paths all lack the `\\?\` prefix.
        assert_eq!(
            strip_windows_verbatim_prefix(r"C:\Temp\harn-abc"),
            r"C:\Temp\harn-abc"
        );
        assert_eq!(
            strip_windows_verbatim_prefix("/tmp/harn-abc/child"),
            "/tmp/harn-abc/child"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\.\PhysicalDrive0"),
            r"\\.\PhysicalDrive0"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"relative\dir"),
            r"relative\dir"
        );
        assert_eq!(strip_windows_verbatim_prefix(r"C:\a\..\b"), r"C:\a\..\b");
        assert_eq!(
            strip_windows_verbatim_prefix("C:/forward/slash"),
            "C:/forward/slash"
        );
    }

    #[test]
    fn strips_prefix_from_paths_longer_than_max_path() {
        let deep = format!(r"\\?\C:\{}", "segment\\".repeat(40));
        let stripped = strip_windows_verbatim_prefix(&deep);
        assert!(stripped.len() > 260, "test path must exceed MAX_PATH");
        assert_eq!(stripped, &deep[r"\\?\".len()..]);
        assert!(!stripped.starts_with(r"\\?\"));
    }
}
