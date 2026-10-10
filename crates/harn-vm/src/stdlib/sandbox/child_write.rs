//! Whether a confined child could write one path, read from the rules the
//! backend actually installs.
//!
//! The approval boundary refuses a command under a `read` external root
//! because, by itself, it cannot tell whether the command could write there.
//! The backends can, but they disagree: the macOS profile re-denies writes to
//! a read-only root nested in a writable one, while Linux Landlock rules only
//! add access, so the same root inherits its parent's write grant. A third
//! list kept here would drift from both. So the answer is computed from the
//! rendered seatbelt profile and from the Landlock rule set the spawn path
//! builds, and anything this module does not fully understand is `Unknown`.

use std::path::{Path, PathBuf};

use crate::orchestration::{CapabilityPolicy, SandboxProfile};

/// What a confined child spawned under a policy may do to one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildWriteDisposition {
    /// The OS mechanism refuses every write to the path: measured from the
    /// installed rules, under a profile that refuses to spawn unconfined.
    EnforcedReadOnly,
    /// Some installed rule grants a write to the path.
    Writable,
    /// Nothing measured: no `os_hardened` policy, a platform without a
    /// mechanism, a mechanism that is absent, or a rule this module does not
    /// read. Never treat this as read-only.
    Unknown,
}

/// The disposition of `path` for a child spawned under `policy`.
///
/// Only `os_hardened` can answer `EnforcedReadOnly`: it is the profile whose
/// spawn refuses outright when the platform mechanism is missing, so a rule
/// read here is a rule the child really runs under. `worktree` falls back to
/// an unconfined child on such a host.
pub fn child_write_disposition(policy: &CapabilityPolicy, path: &Path) -> ChildWriteDisposition {
    if policy.sandbox_profile != SandboxProfile::OsHardened {
        return ChildWriteDisposition::Unknown;
    }
    let writes_enforced = super::enforcement::active_enforcement().is_some_and(|row| {
        row.cell(super::enforcement::ConfinementDimension::Writes)
            == super::enforcement::Enforcement::Enforced
    });
    if !writes_enforced {
        return ChildWriteDisposition::Unknown;
    }
    let Some(target) = resolved_target(path) else {
        return ChildWriteDisposition::Unknown;
    };
    platform_disposition(policy, &target)
}

/// The path a write would land on: symlinks resolved through the deepest
/// existing ancestor, since both mechanisms judge the resolved path.
fn resolved_target(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&existing) {
            let mut resolved = canonical;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return Some(super::normalize_for_policy(&resolved));
        }
        let name = existing.file_name()?.to_os_string();
        if name == ".." || name == "." {
            return None;
        }
        rest.push(name);
        if !existing.pop() {
            return None;
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_disposition(policy: &CapabilityPolicy, target: &Path) -> ChildWriteDisposition {
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return ChildWriteDisposition::Unknown;
    }
    seatbelt_write_disposition(&super::macos::rendered_profile(policy), target)
}

#[cfg(target_os = "linux")]
fn platform_disposition(policy: &CapabilityPolicy, target: &Path) -> ChildWriteDisposition {
    match super::linux::installed_write_grants(policy) {
        Some(grants) => landlock_write_disposition(&grants, target),
        None => ChildWriteDisposition::Unknown,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_disposition(_policy: &CapabilityPolicy, _target: &Path) -> ChildWriteDisposition {
    ChildWriteDisposition::Unknown
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

// -------------------------------------------------------------------------------------------------
// Seatbelt.
// -------------------------------------------------------------------------------------------------

/// One parsed s-expression of a seatbelt profile.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Sexp {
    Atom(String),
    Str(String),
    List(Vec<Sexp>),
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_sexps(source: &str) -> Option<Vec<Sexp>> {
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut forms = Vec::new();
    loop {
        skip_space(&chars, &mut index);
        if index >= chars.len() {
            return Some(forms);
        }
        forms.push(parse_sexp(&chars, &mut index)?);
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn skip_space(chars: &[char], index: &mut usize) {
    while *index < chars.len() {
        if chars[*index].is_whitespace() {
            *index += 1;
        } else if chars[*index] == ';' {
            while *index < chars.len() && chars[*index] != '\n' {
                *index += 1;
            }
        } else {
            return;
        }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_sexp(chars: &[char], index: &mut usize) -> Option<Sexp> {
    skip_space(chars, index);
    match chars.get(*index)? {
        '(' => {
            *index += 1;
            let mut items = Vec::new();
            loop {
                skip_space(chars, index);
                match chars.get(*index)? {
                    ')' => {
                        *index += 1;
                        return Some(Sexp::List(items));
                    }
                    _ => items.push(parse_sexp(chars, index)?),
                }
            }
        }
        ')' => None,
        '"' => {
            *index += 1;
            let mut text = String::new();
            loop {
                match chars.get(*index)? {
                    '\\' => {
                        text.push(*chars.get(*index + 1)?);
                        *index += 2;
                    }
                    '"' => {
                        *index += 1;
                        return Some(Sexp::Str(text));
                    }
                    other => {
                        text.push(*other);
                        *index += 1;
                    }
                }
            }
        }
        _ => {
            let mut atom = String::new();
            while let Some(ch) = chars.get(*index) {
                if ch.is_whitespace() || *ch == '(' || *ch == ')' {
                    break;
                }
                atom.push(*ch);
                *index += 1;
            }
            Some(Sexp::Atom(atom))
        }
    }
}

/// Whether one filter matches `target`, or `None` for a filter form this
/// module does not read.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_filter_matches(filter: &Sexp, target: &Path) -> Option<bool> {
    let Sexp::List(items) = filter else {
        return None;
    };
    let (Some(Sexp::Atom(kind)), rest) = (items.first(), &items[1..]) else {
        return None;
    };
    match (kind.as_str(), rest) {
        ("subpath", [Sexp::Str(root)]) => Some(within(target, Path::new(root))),
        ("literal", [Sexp::Str(exact)]) => Some(target == Path::new(exact)),
        // A socket-only create grant still makes an entry under the root.
        // Counting it as a write keeps the answer on the safe side.
        ("require-all", filters) => {
            let mut all = true;
            for inner in filters {
                if let Sexp::List(parts) = inner {
                    if parts.first() == Some(&Sexp::Atom("vnode-type".to_string())) {
                        continue;
                    }
                }
                all &= seatbelt_filter_matches(inner, target)?;
            }
            Some(all)
        }
        _ => None,
    }
}

/// Evaluate a rendered profile for a write to `target`: default deny, last
/// matching file-write rule wins, as `sandbox-exec` does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn seatbelt_write_disposition(profile: &str, target: &Path) -> ChildWriteDisposition {
    let Some(forms) = parse_sexps(profile) else {
        return ChildWriteDisposition::Unknown;
    };
    let aliases = path_aliases(target);
    let mut writable = false;
    for form in &forms {
        let Sexp::List(items) = form else {
            return ChildWriteDisposition::Unknown;
        };
        let Some(Sexp::Atom(action)) = items.first() else {
            return ChildWriteDisposition::Unknown;
        };
        let allow = match action.as_str() {
            "allow" => true,
            "deny" => false,
            "version" => continue,
            _ => return ChildWriteDisposition::Unknown,
        };
        let mut writes = false;
        let mut filters = Vec::new();
        for item in &items[1..] {
            match item {
                Sexp::Atom(operation) => {
                    if operation.starts_with("file-write")
                        || (allow && (operation == "file*" || operation == "default"))
                    {
                        writes = true;
                    }
                }
                Sexp::List(_) => filters.push(item),
                Sexp::Str(_) => return ChildWriteDisposition::Unknown,
            }
        }
        if !writes {
            continue;
        }
        let mut matches = filters.is_empty();
        for filter in &filters {
            let mut any_alias = false;
            for alias in &aliases {
                match seatbelt_filter_matches(filter, alias) {
                    Some(hit) => any_alias |= hit,
                    None => return ChildWriteDisposition::Unknown,
                }
            }
            matches |= any_alias;
        }
        if matches {
            writable = allow;
        }
    }
    if writable {
        ChildWriteDisposition::Writable
    } else {
        ChildWriteDisposition::EnforcedReadOnly
    }
}

/// macOS reaches `/tmp`, `/var` and `/etc` through both a logical and a
/// `/private` spelling; a rule for either covers the file.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn path_aliases(target: &Path) -> Vec<PathBuf> {
    let mut aliases = vec![target.to_path_buf()];
    if let Ok(rest) = target.strip_prefix("/private") {
        aliases.push(Path::new("/").join(rest));
    } else if ["/tmp", "/var", "/etc"]
        .iter()
        .any(|root| target.starts_with(root))
    {
        aliases.push(Path::new("/private").join(target.strip_prefix("/").unwrap_or(target)));
    }
    aliases
}

// -------------------------------------------------------------------------------------------------
// Landlock.
// -------------------------------------------------------------------------------------------------

/// The write-side grants a Landlock ruleset installs: each rule's path and
/// access, the access rights the ruleset handles, and the rights that write.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct LandlockWriteGrants {
    pub rules: Vec<(PathBuf, u64)>,
    pub handled_access: u64,
    pub write_access: u64,
}

/// A write is refused only if the ruleset handles every write right and no
/// rule on the path or an ancestor grants one: an unhandled right is not
/// restricted at all.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn landlock_write_disposition(
    grants: &LandlockWriteGrants,
    target: &Path,
) -> ChildWriteDisposition {
    if grants.handled_access & grants.write_access != grants.write_access {
        return ChildWriteDisposition::Writable;
    }
    let granted = grants
        .rules
        .iter()
        .filter(|(root, _)| within(target, root))
        .fold(0, |access, (_, rule)| access | rule);
    if granted & grants.write_access == 0 {
        ChildWriteDisposition::EnforcedReadOnly
    } else {
        ChildWriteDisposition::Writable
    }
}

#[cfg(test)]
#[path = "child_write_tests.rs"]
mod tests;
