use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum Source {
    Explicit,
    XdgCache,
    LocalAppData,
    Home,
}

#[derive(Debug, PartialEq, Eq)]
struct InstallRoot {
    path: String,
    source: Source,
}

fn resolve(lookup: impl Fn(&str) -> Option<String>, windows: bool) -> Option<InstallRoot> {
    // Follow Corepack's own precedence before XDG_CACHE_HOME is relocated.
    // This directory holds installed package managers, not disposable cache.
    if let Some(path) = lookup("COREPACK_HOME") {
        return Some(InstallRoot {
            path,
            source: Source::Explicit,
        });
    }
    let (base, source) = if let Some(path) = lookup("XDG_CACHE_HOME") {
        (PathBuf::from(path), Source::XdgCache)
    } else if let Some(path) = lookup("LOCALAPPDATA") {
        (PathBuf::from(path), Source::LocalAppData)
    } else {
        let home = lookup("HOME");
        let userprofile = lookup("USERPROFILE");
        let home = crate::user_dirs::home_dir_from(
            home.as_deref().map(std::ffi::OsStr::new),
            userprofile.as_deref().map(std::ffi::OsStr::new),
        );
        let home = home.filter(|path| path.is_absolute())?;
        (
            home.join(if windows { "AppData/Local" } else { ".cache" }),
            Source::Home,
        )
    };
    Some(InstallRoot {
        path: base.join("node/corepack").display().to_string(),
        source,
    })
}

pub(super) fn installed_root() -> Option<String> {
    let environment = crate::stdlib::process::current_session_environment();
    let lookup = |key: &str| match &environment {
        Some(environment) => environment.launcher_value(key).map(str::to_string),
        None => crate::test_env::env_var_seamed(key),
    };
    resolve(lookup, cfg!(windows)).map(|root| root.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(values: &[(&str, &str)], windows: bool) -> Option<InstallRoot> {
        resolve(
            |key| {
                values
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.to_string())
            },
            windows,
        )
    }

    #[test]
    fn explicit_presence_wins_without_rewriting_the_selected_installation() {
        for explicit in ["", "relative-install", "/selected/corepack"] {
            let root = resolved(
                &[
                    ("COREPACK_HOME", explicit),
                    ("XDG_CACHE_HOME", "/operator/cache"),
                    ("LOCALAPPDATA", "/operator/local"),
                ],
                false,
            )
            .unwrap();
            assert_eq!(root.path, explicit);
            assert_eq!(root.source, Source::Explicit);
        }
    }

    #[test]
    fn original_cache_selection_precedes_platform_defaults() {
        let xdg = resolved(
            &[
                ("XDG_CACHE_HOME", "selected-cache"),
                ("LOCALAPPDATA", "other-cache"),
            ],
            true,
        )
        .unwrap();
        assert_eq!(
            PathBuf::from(xdg.path),
            PathBuf::from("selected-cache/node/corepack")
        );
        assert_eq!(xdg.source, Source::XdgCache);
        let local = resolved(&[("LOCALAPPDATA", "selected-local")], false).unwrap();
        assert_eq!(
            PathBuf::from(local.path),
            PathBuf::from("selected-local/node/corepack")
        );
        assert_eq!(local.source, Source::LocalAppData);
    }

    #[test]
    fn defaults_retain_installed_managers_and_missing_home_is_not_a_relative_root() {
        let home = std::env::temp_dir().join("corepack-operator-home");
        for (windows, suffix) in [
            (false, ".cache/node/corepack"),
            (true, "AppData/Local/node/corepack"),
        ] {
            let home_value = home.to_str().unwrap();
            let root = resolved(&[("HOME", home_value)], windows).unwrap();
            assert_eq!(PathBuf::from(root.path), home.join(suffix));
            assert_eq!(root.source, Source::Home);
        }
        assert_eq!(resolve(|_| None, false), None);
        assert_eq!(resolved(&[("HOME", "relative-home")], false), None);
        let profile = resolved(
            &[("HOME", ""), ("USERPROFILE", home.to_str().unwrap())],
            true,
        )
        .unwrap();
        assert_eq!(
            PathBuf::from(profile.path),
            home.join("AppData/Local/node/corepack")
        );
    }
}
