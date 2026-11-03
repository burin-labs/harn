//! What GitHub CLI can execute with its environment, and how a granted spawn
//! takes each of those paths away.
//!
//! A `for_command: "gh"` grant hands a credential to one `gh` process. gh
//! itself starts other programs with that same environment: a pager, a
//! browser, an editor, an extension binary, git, and any `!`-prefixed alias,
//! which gh runs through `sh -c`. [`GH_PROFILE`] closes each of them for a
//! granted spawn (see `process_command_grant` for the shared git and `PATH`
//! hardening):
//!
//! - `GH_PAGER` and `PAGER` are empty, which gh reads as "no pager".
//! - `GH_BROWSER`, `BROWSER`, `GH_EDITOR`, `EDITOR` and `VISUAL` name a
//!   command that exits non-zero without running anything.
//! - `XDG_DATA_HOME` points at an empty per-spawn directory. gh finds
//!   extensions under `$XDG_DATA_HOME/gh/extensions` (falling back to
//!   `~/.local/share` and, on Windows, `%LocalAppData%`), and the variable
//!   wins over both.
//! - `GH_CONFIG_DIR` points at a per-spawn `config.yml` that keeps only a few
//!   display settings and inert aliases. `hosts.yml` is never copied.
//! - `gh auth` subcommands that print the credential are refused.
//! - Only read subcommands receive the grant at all; see
//!   `gh_invocation_is_read_only`.

use std::path::Path;

use crate::stdlib::process::ExecProfile;

pub(crate) const GH_PROFILE: ExecProfile = ExecProfile {
    program: "gh",
    empty_env: &["GH_PAGER", "PAGER"],
    refusing_env: &["GH_BROWSER", "BROWSER", "GH_EDITOR", "EDITOR", "VISUAL"],
    dir_env: &[("GH_CONFIG_DIR", "config"), ("XDG_DATA_HOME", "data")],
    seed: seed_granted_config,
    refusal: gh_refusal,
    // Reads only: a write, clone, checkout, extension, alias, or anything
    // unrecognized runs without the credential, and with it any git work in
    // the repository that could start a filter driver or diff program.
    grants: crate::orchestration::gh_invocation_is_read_only,
};

/// Top-level keys a granted spawn's stand-in keeps. Every other key either
/// names a program (`pager`, `browser`, `editor`), a transport
/// (`http_unix_socket`), or is not needed to talk to the API.
const GRANTED_KEYS: &[&str] = &[
    "git_protocol",
    "prompt",
    "spinner",
    "color_labels",
    "accessible_colors",
    "accessible_prompter",
    "aliases",
];
/// Top-level keys no stand-in keeps, granted or not.
const EXEC_KEYS: &[&str] = &["pager", "browser", "editor", "http_unix_socket"];

/// Why a granted gh must not run these arguments at all.
///
/// - `--` hands the rest of the line to another program: `gh repo clone o/r
///   -- -c filter.x.smudge=...` passes it to `git clone`.
/// - `--jq`/`-q` filters can read the environment (`$ENV`, `env`) and print
///   the credential in a form exact-value redaction cannot see. `--template`
///   and `-t` are refused with them: the template functions are not known to
///   reach the environment, but they format the same output.
/// - `gh auth` prints the credential for `token`, `status --show-token` and
///   `git-credential`, and changes git or gh configuration for the rest.
///
/// Short flags are refused by letter anywhere in a cluster (`-qX`, `-Lq`),
/// except a `-R` cluster, whose remainder is the repository name. Only a read,
/// which would otherwise receive the grant, is refused for `--` or a filter;
/// anything else already runs without it, so `gh pr create -t x` still works.
fn gh_refusal(args: &[String]) -> Option<&'static str> {
    if let Some(reason) = auth_refusal(args) {
        return Some(reason);
    }
    if !crate::orchestration::gh_invocation_is_read_only(args) {
        return None;
    }
    for arg in args {
        if arg == "--" {
            return Some("`--` would pass the rest of the command to another program");
        }
        let long_filter = ["--jq", "--template"]
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")));
        let short_filter = arg.len() > 1
            && arg.starts_with('-')
            && !arg.starts_with("--")
            && !arg.starts_with("-R")
            && arg[1..].contains(['q', 't']);
        if long_filter || short_filter {
            return Some("an output filter can read the credential from the environment");
        }
    }
    None
}

/// Only a plain `gh auth status` is not refused; it still receives no grant,
/// because it is not a read.
fn auth_refusal(args: &[String]) -> Option<&'static str> {
    let mut words = args.iter().map(String::as_str);
    if words.find(|word| !word.starts_with('-')) != Some("auth") {
        return None;
    }
    if words.next() != Some("status") {
        return Some("`gh auth` would print or change the credential");
    }
    let mut rest = words;
    while let Some(word) = rest.next() {
        match word {
            "-a" | "--active" => {}
            "-h" | "--hostname" => {
                rest.next()?;
            }
            _ if word.starts_with("--hostname=") => {}
            _ => return Some("`gh auth status` with these flags would print the credential"),
        }
    }
    None
}

/// Write the granted stand-in `config.yml` into `dir/config`. A missing user
/// config writes nothing; an unreadable one is an error.
fn seed_granted_config(dir: &Path) -> std::io::Result<()> {
    let home = crate::user_dirs::home_dir().unwrap_or_default();
    let source = super::gh_config_source(&home).join("config.yml");
    let raw = match std::fs::read_to_string(&source) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match project_gh_config(&raw, Some(GRANTED_KEYS)) {
        Some(config) => {
            super::grant_spawn::write_private_file(&dir.join("config/config.yml"), &config)
        }
        None => Ok(()),
    }
}

/// The credential-free, exec-free stand-in a confined, ungranted gh reads.
pub(crate) fn sanitize_gh_config(raw: &str) -> Option<String> {
    project_gh_config(raw, None)
}

/// The credential-free, exec-free projection of a gh `config.yml`, or `None`
/// when the file must not be copied at all. With `allowed`, only those
/// top-level keys survive.
///
/// gh runs an alias whose expansion starts with `!` through `sh -c`. This
/// parser is line-based and conservative: it keeps an `aliases:` entry only
/// when it is a one-line `name: value` whose value is a plain or simply quoted
/// scalar that cannot start with `!` or expand to `auth`. Anything else inside
/// the block (block scalars, anchors, tags, flow mappings, continuation lines,
/// escapes) is dropped, as is every key that names a program to run. A file
/// whose top level is not plain `key:` lines, or that still carries a pre-1.0
/// `oauth_token`, is not copied.
fn project_gh_config(raw: &str, allowed: Option<&[&str]>) -> Option<String> {
    if raw.contains("oauth_token") {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut in_aliases = false;
    let mut skip_block = false;
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
            skip_block = EXEC_KEYS.contains(&key)
                || allowed.is_some_and(|allowed| !allowed.contains(&key))
                // An inline (flow) alias mapping is dropped whole.
                || (in_aliases && !rest.is_empty() && !rest.starts_with('#'));
            if skip_block {
                continue;
            }
        } else if skip_block || (in_aliases && !alias_entry_is_inert(content)) {
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
    if unquoted.split_whitespace().next() == Some("auth") {
        return false;
    }
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
    use super::{gh_refusal, project_gh_config, sanitize_gh_config, GRANTED_KEYS};

    #[test]
    fn shell_aliases_never_reach_the_stand_in() {
        let raw = "\
# gh config
git_protocol: ssh
editor: vim
pager: less
browser: firefox
http_unix_socket: /run/x.sock
aliases:
    co: pr checkout
    tok: auth token
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
            "co: pr checkout",
            "quoted: 'issue list --limit 5'",
            "prompt: enabled",
        ] {
            assert!(sanitized.contains(kept), "{kept} was dropped:\n{sanitized}");
        }
        for dropped in ["env", "vim", "less", "firefox", "sock", "auth"] {
            assert!(
                !sanitized.contains(dropped),
                "an exec path survived:\n{sanitized}"
            );
        }
        let granted = project_gh_config(
            "git_protocol: ssh\nversion: 1\nfuture:\n  x: y\n",
            Some(GRANTED_KEYS),
        )
        .unwrap();
        assert_eq!(granted, "git_protocol: ssh\n");
    }

    #[test]
    fn credential_printing_arguments_are_refused() {
        let refused = |args: &[&str]| {
            gh_refusal(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()).is_some()
        };
        for allowed in [
            &["issue", "list"][..],
            &["auth", "status"],
            &["auth", "status", "-h", "github.com", "--active"],
            &["api", "auth"],
            &["-R", "o/test", "pr", "view", "1"],
            &["-Rkit/test", "pr", "list"],
            &["pr", "list", "--json", "number"],
            // Not reads: they run without the grant, so nothing is refused.
            &["pr", "create", "-t", "x"],
            &["repo", "clone", "o/r", "--", "-c", "filter.a.smudge=x"],
        ] {
            assert!(!refused(allowed), "{allowed:?}");
        }
        for args in [
            &["auth", "token"][..],
            &["auth", "status", "-t"],
            &["auth", "status", "--show-token"],
            &["auth", "git-credential", "get"],
            &["auth", "setup-git"],
            &["--help", "auth", "token"],
            &["auth"],
            &["pr", "view", "1", "--", "-c", "filter.a.smudge=x"],
            &["api", "rate_limit", "--jq", "$ENV.X"],
            &["api", "rate_limit", "--jq=env.X"],
            &["pr", "list", "--json", "n", "-q", ".[]"],
            &["pr", "list", "-q.[]"],
            &["pr", "list", "-Lq", "5"],
            &["api", "x", "--template", "{{.}}"],
            &["api", "x", "--template={{.}}"],
            &["api", "x", "-t", "{{.}}"],
            &["api", "x", "-t{{.}}"],
        ] {
            assert!(refused(args), "{args:?}");
        }
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
