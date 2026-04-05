//! std/path — pure string-level path manipulation.
//!
//! All functions normalise input on `/` and emit forward slashes. No
//! filesystem I/O happens here; that lives in `stdlib/fs.rs`. The goal is
//! that these helpers are deterministic and OS-agnostic so they can be
//! called from Harn code without surprises when a Windows-style path
//! crosses the wire.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Convert all backslashes to forward slashes.
fn to_posix(s: &str) -> String {
    s.replace('\\', "/")
}

/// Returns true if the path is absolute (leading `/` on posix or `X:` drive
/// letter on windows).
fn is_absolute_str(p: &str) -> bool {
    let p = to_posix(p);
    if p.starts_with('/') {
        return true;
    }
    let bytes = p.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Split a path into segments, preserving whether it was absolute.
fn split_segments(p: &str) -> (bool, Option<String>, Vec<String>) {
    let posix = to_posix(p);
    let mut drive: Option<String> = None;
    let mut rest: &str = &posix;
    let bytes = posix.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        drive = Some(posix[..2].to_string());
        rest = &posix[2..];
    }
    let absolute = rest.starts_with('/');
    let segments: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    (absolute, drive, segments)
}

/// Normalise a path: collapse `..`, dedupe `/`, strip trailing slashes.
fn normalize(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    let (absolute, drive, segments) = split_segments(p);
    let mut stack: Vec<String> = Vec::new();
    for seg in segments {
        match seg.as_str() {
            "." => continue,
            ".." => {
                if let Some(top) = stack.last() {
                    if top != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !absolute {
                    stack.push("..".into());
                }
            }
            _ => stack.push(seg),
        }
    }
    let mut out = String::new();
    if let Some(d) = drive {
        out.push_str(&d);
    }
    if absolute {
        out.push('/');
    }
    out.push_str(&stack.join("/"));
    if out.is_empty() {
        ".".into()
    } else {
        out
    }
}

/// Extract the file name (last segment) of a path.
fn basename(p: &str) -> String {
    let (_, _, segments) = split_segments(p);
    segments.last().cloned().unwrap_or_default()
}

/// Extract the parent directory of a path (everything before the last
/// segment). Returns empty string for single-segment relative paths and
/// `/` for root paths.
fn parent(p: &str) -> String {
    let (absolute, drive, mut segments) = split_segments(p);
    if segments.len() <= 1 && !absolute {
        return String::new();
    }
    segments.pop();
    let mut out = String::new();
    if let Some(d) = drive {
        out.push_str(&d);
    }
    if absolute {
        out.push('/');
    }
    out.push_str(&segments.join("/"));
    if out.is_empty() && absolute {
        "/".into()
    } else {
        out
    }
}

/// Extract the extension including the leading dot, or empty string if none.
fn extension(p: &str) -> String {
    let name = basename(p);
    if let Some(idx) = name.rfind('.') {
        if idx == 0 {
            // Leading dot → hidden file, no extension.
            return String::new();
        }
        return name[idx..].to_string();
    }
    String::new()
}

/// Extract the file stem (basename minus extension).
fn stem(p: &str) -> String {
    let name = basename(p);
    if let Some(idx) = name.rfind('.') {
        if idx == 0 {
            return name;
        }
        return name[..idx].to_string();
    }
    name
}

/// Replace the extension on a path. `new_ext` may include or omit the
/// leading dot.
fn with_extension(p: &str, new_ext: &str) -> String {
    let normalized_ext = if new_ext.is_empty() || new_ext.starts_with('.') {
        new_ext.to_string()
    } else {
        format!(".{new_ext}")
    };
    let parent_dir = parent(p);
    let stem_name = stem(p);
    let new_name = format!("{stem_name}{normalized_ext}");
    if parent_dir.is_empty() {
        new_name
    } else if parent_dir == "/" {
        format!("/{new_name}")
    } else {
        format!("{parent_dir}/{new_name}")
    }
}

/// Replace the file stem on a path, keeping the extension.
fn with_stem(p: &str, new_stem: &str) -> String {
    let ext = extension(p);
    let parent_dir = parent(p);
    let new_name = format!("{new_stem}{ext}");
    if parent_dir.is_empty() {
        new_name
    } else if parent_dir == "/" {
        format!("/{new_name}")
    } else {
        format!("{parent_dir}/{new_name}")
    }
}

/// Compute the relative path from `base` to `p`. Returns `None` if `p` is
/// not reachable as a descendant of `base` via relative traversal.
fn relative_to(p: &str, base: &str) -> Option<String> {
    let (p_abs, _, p_segs) = split_segments(&normalize(p));
    let (b_abs, _, b_segs) = split_segments(&normalize(base));
    if p_abs != b_abs {
        return None;
    }
    let common = p_segs
        .iter()
        .zip(b_segs.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let up = b_segs.len() - common;
    let mut out: Vec<String> = std::iter::repeat_n("..".to_string(), up).collect();
    out.extend(p_segs[common..].iter().cloned());
    if out.is_empty() {
        Some(".".into())
    } else {
        Some(out.join("/"))
    }
}

pub(crate) fn register_path_helper_builtins(vm: &mut Vm) {
    vm.register_builtin("path_parts", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
        map.insert("dir".into(), VmValue::String(Rc::from(parent(&p))));
        map.insert("file".into(), VmValue::String(Rc::from(basename(&p))));
        map.insert("stem".into(), VmValue::String(Rc::from(stem(&p))));
        map.insert("ext".into(), VmValue::String(Rc::from(extension(&p))));
        let (_, _, segments) = split_segments(&p);
        map.insert(
            "segments".into(),
            VmValue::List(Rc::new(
                segments
                    .into_iter()
                    .map(|s| VmValue::String(Rc::from(s.as_str())))
                    .collect(),
            )),
        );
        Ok(VmValue::Dict(Rc::new(map)))
    });

    vm.register_builtin("path_parent", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(parent(&p))))
    });

    vm.register_builtin("path_basename", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(basename(&p))))
    });

    vm.register_builtin("path_stem", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(stem(&p))))
    });

    vm.register_builtin("path_extension", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(extension(&p))))
    });

    vm.register_builtin("path_with_extension", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        let ext = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(with_extension(&p, &ext))))
    });

    vm.register_builtin("path_with_stem", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        let s = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(with_stem(&p, &s))))
    });

    vm.register_builtin("path_is_absolute", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(is_absolute_str(&p)))
    });

    vm.register_builtin("path_is_relative", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(!is_absolute_str(&p)))
    });

    vm.register_builtin("path_normalize", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(normalize(&p))))
    });

    vm.register_builtin("path_relative_to", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        let base = args.get(1).map(|a| a.display()).unwrap_or_default();
        match relative_to(&p, &base) {
            Some(rel) => Ok(VmValue::String(Rc::from(rel))),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("path_to_posix", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(to_posix(&p))))
    });

    vm.register_builtin("path_to_native", |args, _out| {
        // The Harn runtime normalises on `/` regardless of OS, so
        // path_to_native currently mirrors path_to_posix. Reserved for
        // future Windows-host specialisation.
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(to_posix(&p))))
    });

    vm.register_builtin("path_segments", |args, _out| {
        let p = args.first().map(|a| a.display()).unwrap_or_default();
        let (_, _, segments) = split_segments(&p);
        Ok(VmValue::List(Rc::new(
            segments
                .into_iter()
                .map(|s| VmValue::String(Rc::from(s.as_str())))
                .collect(),
        )))
    });

    // Silence unused-import warnings if the VmError type ever goes unused
    // in a future refactor of this module.
    let _ = std::marker::PhantomData::<VmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dot_dot() {
        assert_eq!(normalize("a/b/../c"), "a/c");
        assert_eq!(normalize("../a"), "../a");
        assert_eq!(normalize("a/../../b"), "../b");
        assert_eq!(normalize("a/b/"), "a/b");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        assert_eq!(normalize("a\\b\\c"), "a/b/c");
    }

    #[test]
    fn basename_and_parent() {
        assert_eq!(basename("a/b/c.rs"), "c.rs");
        assert_eq!(parent("a/b/c.rs"), "a/b");
        assert_eq!(parent("a"), "");
        assert_eq!(parent("/"), "/");
        assert_eq!(parent("/a"), "/");
    }

    #[test]
    fn stem_and_extension() {
        assert_eq!(stem("a/b/c.rs"), "c");
        assert_eq!(extension("a/b/c.rs"), ".rs");
        assert_eq!(extension("a/b/c"), "");
        assert_eq!(extension(".gitignore"), "");
        assert_eq!(stem(".gitignore"), ".gitignore");
        assert_eq!(extension("a/b/c.tar.gz"), ".gz");
        assert_eq!(stem("a/b/c.tar.gz"), "c.tar");
    }

    #[test]
    fn with_extension_and_stem() {
        assert_eq!(with_extension("a/b/c.rs", "txt"), "a/b/c.txt");
        assert_eq!(with_extension("a/b/c.rs", ".txt"), "a/b/c.txt");
        assert_eq!(with_extension("c.rs", "py"), "c.py");
        assert_eq!(with_stem("a/b/c.rs", "main"), "a/b/main.rs");
        assert_eq!(with_stem("c.rs", "main"), "main.rs");
    }

    #[test]
    fn is_absolute_detection() {
        assert!(is_absolute_str("/a/b"));
        assert!(is_absolute_str("C:/a/b"));
        assert!(!is_absolute_str("a/b"));
        assert!(!is_absolute_str("./a"));
        assert!(!is_absolute_str(""));
    }

    #[test]
    fn relative_to_walks_up() {
        assert_eq!(relative_to("/a/b/c", "/a/b").as_deref(), Some("c"));
        assert_eq!(relative_to("/a/c", "/a/b").as_deref(), Some("../c"));
        assert_eq!(relative_to("a/b/c", "a/b").as_deref(), Some("c"));
        assert_eq!(relative_to("/a", "b"), None);
    }
}
