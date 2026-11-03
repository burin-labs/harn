//! What GitHub CLI can execute with its environment, and how a granted spawn
//! takes each of those paths away.
//!
//! A `for_command: "gh"` grant hands a credential to one `gh` process. gh
//! itself starts other programs with that same environment: a pager, a
//! browser, an editor, an extension binary, and any `!`-prefixed alias, which
//! gh runs through `sh -c`. Each of those is a program the grant never named,
//! so a granted gh spawn closes all of them:
//!
//! - `GH_PAGER` and `PAGER` are empty, which gh reads as "no pager".
//! - `GH_BROWSER`, `BROWSER`, `GH_EDITOR`, `EDITOR` and `VISUAL` name a
//!   command that exits non-zero without running anything.
//! - `XDG_DATA_HOME` points at an empty directory. gh finds extensions under
//!   `$XDG_DATA_HOME/gh/extensions` (falling back to `~/.local/share` and, on
//!   Windows, `%LocalAppData%`), and the variable wins over both.
//! - `GH_CONFIG_DIR` points at a credential-free `config.yml` stand-in with
//!   every shell alias removed. `hosts.yml` is never copied.

use std::path::PathBuf;

use crate::value::{VmError, VmValue};

/// Program gh runs in place of a browser or editor. `/usr/bin/false` exists
/// on every supported Unix host and is not on a workspace-controlled path.
#[cfg(unix)]
const REFUSING_COMMAND: &str = "/usr/bin/false";
/// Windows has no `false`; a device name is not an executable, so gh's spawn
/// of it fails before anything runs.
#[cfg(not(unix))]
const REFUSING_COMMAND: &str = "NUL";

const EMPTY_KEYS: [&str; 2] = ["GH_PAGER", "PAGER"];
const REFUSING_KEYS: [&str; 5] = ["GH_BROWSER", "BROWSER", "GH_EDITOR", "EDITOR", "VISUAL"];

/// The stand-in root for granted gh spawns, under Harn's per-user cache.
fn stand_in_root() -> Option<PathBuf> {
    crate::user_dirs::package_cache_dir().map(|cache| cache.join("gh-grant"))
}

fn refusal(message: String) -> VmError {
    VmError::Thrown(VmValue::string(format!(
        "[environment_policy.gh_grant_unconfined] {message}; the GitHub CLI grant was not exposed"
    )))
}

/// The environment entries a granted `gh` spawn must carry, applied after
/// every other layer so no caller overlay can restore an exec path.
///
/// Fails closed: when the stand-in cannot be prepared, or the empty extension
/// directory is no longer empty, the spawn is refused instead of running gh
/// with a credential and an open exec path.
pub(crate) fn granted_gh_env() -> Result<Vec<(String, String)>, VmError> {
    let root =
        stand_in_root().ok_or_else(|| refusal("no Harn cache directory resolves".to_string()))?;
    let data = root.join("data");
    if std::fs::read_dir(&data).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(refusal(format!(
            "the empty extension directory {} has entries",
            data.display()
        )));
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| refusal(format!("create {}: {error}", root.display())))?;
    let home = crate::user_dirs::home_dir().unwrap_or_default();
    let config = super::seed_gh_config_dir(&super::gh_config_source(&home), &root, "config");
    let mut env: Vec<(String, String)> = EMPTY_KEYS
        .iter()
        .map(|key| (key.to_string(), String::new()))
        .collect();
    env.extend(
        REFUSING_KEYS
            .iter()
            .map(|key| (key.to_string(), REFUSING_COMMAND.to_string())),
    );
    env.push(("XDG_DATA_HOME".to_string(), data.display().to_string()));
    env.push(("GH_CONFIG_DIR".to_string(), config.display().to_string()));
    Ok(env)
}

/// The credential-free, exec-free projection of a gh `config.yml`, or `None`
/// when the file must not be copied at all.
///
/// gh runs an alias whose expansion starts with `!` through `sh -c`. This
/// parser is line-based and conservative: it keeps an `aliases:` entry only
/// when it is a one-line `name: value` whose value is a plain or simply quoted
/// scalar that cannot start with `!`. Anything else inside the block (block
/// scalars, anchors, tags, flow mappings, continuation lines, escapes) is
/// dropped. A file whose top level is not plain `key:` lines, or that still
/// carries a pre-1.0 `oauth_token`, is not copied.
pub(crate) fn sanitize_gh_config(raw: &str) -> Option<String> {
    if raw.contains("oauth_token") {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut in_aliases = false;
    for line in raw.lines() {
        let content = line.trim();
        if content.is_empty() || content.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            let (key, rest) = split_mapping_entry(content)?;
            in_aliases = key == "aliases";
            if in_aliases && !rest.is_empty() && !rest.starts_with('#') {
                // An inline (flow) alias mapping is dropped whole.
                continue;
            }
        } else if in_aliases && !alias_entry_is_inert(content) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

/// `key: rest` with a bare or simply quoted key, or `None`.
fn split_mapping_entry(content: &str) -> Option<(&str, &str)> {
    let (key, rest) = content.split_once(':')?;
    let key = key.trim();
    let key = key
        .strip_prefix('"')
        .and_then(|key| key.strip_suffix('"'))
        .or_else(|| {
            key.strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
        })
        .unwrap_or(key);
    let simple = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
    simple.then(|| (key, rest.trim()))
}

fn alias_entry_is_inert(content: &str) -> bool {
    let Some((_, value)) = split_mapping_entry(content) else {
        return false;
    };
    let unquoted = if let Some(inner) = value.strip_prefix('\'') {
        match inner.strip_suffix('\'') {
            Some(inner) if !inner.contains('\'') => inner,
            _ => return false,
        }
    } else if let Some(inner) = value.strip_prefix('"') {
        match inner.strip_suffix('"') {
            Some(inner) if !inner.contains(['"', '\\']) => inner,
            _ => return false,
        }
    } else {
        if value.contains(" #") {
            return false;
        }
        value
    };
    let first = unquoted.trim_start().chars().next();
    first.is_some_and(|ch| {
        !matches!(
            ch,
            '!' | '|' | '>' | '&' | '*' | '{' | '[' | '%' | '@' | '`'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::sanitize_gh_config;

    #[test]
    fn shell_aliases_never_reach_the_stand_in() {
        let raw = "\
# gh config
git_protocol: ssh
editor: vim
aliases:
    co: pr checkout
    quoted: 'issue list --limit 5'
    leak: '!env'
    bare: !env
    dq: \"!env\"
    escaped: \"\\x21env\"
    spaced: '  !env'
    block: |
        !env
    anchor: *other
    tagged: !!str '!env'
    trailing: issue list # !env
prompt: enabled
";
        let sanitized = sanitize_gh_config(raw).unwrap();
        for kept in [
            "git_protocol: ssh",
            "editor: vim",
            "co: pr checkout",
            "quoted: 'issue list --limit 5'",
            "prompt: enabled",
        ] {
            assert!(sanitized.contains(kept), "{kept} was dropped:\n{sanitized}");
        }
        assert!(
            !sanitized.contains("env"),
            "an exec path survived:\n{sanitized}"
        );
    }

    #[test]
    fn unparseable_or_credentialed_configs_are_not_copied() {
        assert!(
            sanitize_gh_config("aliases: {leak: '!env'}\nprompt: enabled\n")
                .is_some_and(|out| !out.contains("env"))
        );
        assert!(sanitize_gh_config("? aliases\n: {leak: '!env'}\n").is_none());
        assert!(sanitize_gh_config("hosts:\n  github.com:\n    oauth_token: gho_x\n").is_none());
    }
}
