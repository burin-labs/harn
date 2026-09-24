//! The paths a user's Git configuration names, which a confined `git` reads.
//!
//! `git` reads more than its config files. A global `core.hooksPath` is a
//! directory of scripts it runs on every commit, and `include.path`,
//! `core.excludesFile`, `core.attributesFile`, and an absolute credential
//! helper are files it opens. Any of them outside the readable roots makes a
//! confined `git commit` fail, so each is granted read (and, for hooks,
//! execute) through the package-manager-config preset, which is read-only on
//! every backend.
//!
//! Only the global config is followed. The repository's own config lives in
//! the workspace, and the system config lives under `/etc`, which the
//! system-runtime preset already reads.

use std::path::{Path, PathBuf};

use super::super::paths::normalize_for_policy;

/// The global config file `git` reads for a confined child: the launcher's
/// `GIT_CONFIG_GLOBAL` when it names an absolute path, else `~/.gitconfig`.
///
/// One answer for both sides: the child's `GIT_CONFIG_GLOBAL` is set from
/// this, and the roots below are read from it, so the config the sandbox
/// grants is the config the child reads.
pub(crate) fn git_global_config(home: &Path) -> PathBuf {
    let explicit = match crate::stdlib::process::current_session_environment() {
        Some(environment) => environment
            .launcher_value("GIT_CONFIG_GLOBAL")
            .map(str::to_string),
        None => crate::test_env::env_var_seamed("GIT_CONFIG_GLOBAL"),
    };
    explicit
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".gitconfig"))
}

/// Every path the user's global Git configuration names, plus the global
/// config file itself when it lies outside the home defaults.
pub(crate) fn git_config_read_roots(home: &Path) -> Vec<PathBuf> {
    let global = git_global_config(home);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("git/config");
    let mut roots = vec![global.clone()];
    let mut pending = vec![(global, 0usize), (xdg, 0usize)];
    while let Some((file, depth)) = pending.pop() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let base = file.parent().map(Path::to_path_buf).unwrap_or_default();
        for (key, value) in config_entries(&text) {
            let Some(path) = named_path(&key, &value, home, &base) else {
                continue;
            };
            // Includes nest; git itself stops at ten levels.
            if is_include(&key) && depth < 10 {
                pending.push((path.clone(), depth + 1));
            }
            roots.push(path);
        }
    }
    let mut roots: Vec<PathBuf> = roots
        .iter()
        .map(|path| normalize_for_policy(path))
        .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

fn is_include(key: &str) -> bool {
    key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path"))
}

/// The path a config entry names, if it names one.
fn named_path(key: &str, value: &str, home: &Path, base: &Path) -> Option<PathBuf> {
    let raw = match key {
        "core.hookspath" | "core.excludesfile" | "core.attributesfile" => value,
        _ if is_include(key) => value,
        // Only an absolute helper program is a file; a bare name is a
        // `git-credential-<name>` on PATH, and `!` is a shell snippet.
        _ if key.starts_with("credential.") && key.ends_with(".helper") => {
            let program = value.split_whitespace().next()?;
            if !Path::new(program).is_absolute() {
                return None;
            }
            program
        }
        _ => return None,
    };
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    };
    Some(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}

/// `section[.subsection].key` (lowercased section and key) and value for each
/// entry, following git's config syntax closely enough to find paths.
fn config_entries(text: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let Some(header) = header.split(']').next() else {
                continue;
            };
            section = match header.split_once(char::is_whitespace) {
                // `[includeIf "gitdir:~/work/"]`: the subsection keeps its case.
                Some((name, sub)) => format!(
                    "{}.{}",
                    name.to_ascii_lowercase(),
                    sub.trim().trim_matches('"')
                ),
                None => header.to_ascii_lowercase(),
            };
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => (line, "true"),
        };
        entries.push((
            format!("{section}.{}", key.to_ascii_lowercase()),
            unquote(value),
        ));
    }
    entries
}

/// Strip a trailing comment and surrounding quotes, and resolve escapes.
fn unquote(value: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            },
            '#' | ';' if !quoted => break,
            _ => out.push(ch),
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_paths_a_global_config_names() {
        let home = tempfile::tempdir().expect("home");
        let home = home.path();
        std::fs::write(
            home.join("extra.inc"),
            "[core]\n\tattributesFile = ~/attrs\n",
        )
        .expect("include");
        std::fs::write(
            home.join(".gitconfig"),
            "[core]\n\thooksPath = \"/opt/hooks\" # team hooks\n\texcludesFile = ~/ignore\n\
             [include]\n\tpath = extra.inc\n\
             [includeIf \"gitdir:~/work/\"]\n\tpath = /opt/work.inc\n\
             [credential]\n\thelper = /usr/local/bin/helper --store\n\
             [credential \"https://example.com\"]\n\thelper = store\n\
             [user]\n\tname = Someone\n",
        )
        .expect("config");
        let roots = git_config_read_roots(home);
        for expected in [
            home.join(".gitconfig"),
            PathBuf::from("/opt/hooks"),
            home.join("ignore"),
            home.join("extra.inc"),
            home.join("attrs"),
            PathBuf::from("/opt/work.inc"),
            PathBuf::from("/usr/local/bin/helper"),
        ] {
            let expected = normalize_for_policy(&expected);
            assert!(
                roots.contains(&expected),
                "{expected:?} missing from {roots:?}"
            );
        }
        assert!(
            !roots.iter().any(|root| root.ends_with("store")),
            "a helper named by bare word is on PATH, not a file: {roots:?}"
        );
    }
}
