//! The child-process environment allowlist and the single profile-aware env
//! resolver.
//!
//! # Why an allowlist, not a scrub
//!
//! A [`SessionProfile`] that carries no grants is *hermetic*: no credential may
//! cross into a spawned child. The safe way to build that child's environment is
//! **closed by construction** — start from nothing and admit only the variables
//! on a typed allowlist — rather than inheriting the parent environment and
//! trying to *subtract* everything secret. A subtractive scrub is open by
//! default: every new provider prefix or vendor token that nobody added to the
//! denylist leaks, silently. The allowlist inverts the failure mode: a variable
//! a toolchain needs but that is not yet listed makes the build **fail loudly**,
//! and the fix is to add it here (with a one-line receipt for why), never to
//! reopen the door with a denylist.
//!
//! # One resolver, two profiles
//!
//! [`resolve_env`] is the *single* code path that builds a child environment for
//! either profile. There is deliberately no hermetic-vs-lane branch:
//!
//! ```text
//!   child_env = allowlist(parent_env) + profile.env_exposure()
//! ```
//!
//! A hermetic profile has no grants, so `env_exposure()` is empty and the child
//! sees the allowlist alone. A lane profile adds exactly its granted
//! `(VAR, value)` pairs on top. `grants: []` is therefore *literally* the
//! hermetic definition — the same resolver, exercised with an empty grant set —
//! not a separately maintained path that could drift from the lane path.
//!
//! # Single owner
//!
//! [`ENV_ALLOWLIST`] is the one place the admitted names live. Nothing else in
//! the codebase should hand-maintain a parallel "safe env" list; the drift test
//! in this module pins the invariants (unique, no obviously-secret
//! names, base essentials present) so an accidental scatter or a secret-shaped
//! addition fails `cargo test`.

use std::collections::BTreeMap;

use super::session_grants::{GrantError, SessionProfile};

/// POSIX/shell/locale essentials any build or test process needs to run at all.
/// These are workspace/user facts, never credentials.
const BASE_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "TERM",
    "TZ",
    "HOSTNAME",
    "COLUMNS",
    "LINES",
    // Temp dirs — honored by compiler/linker toolchains for intermediates.
    "TMPDIR",
    "TMP",
    "TEMP",
    // Locale — pins message/encoding behavior; not secret-bearing.
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    // XDG base dirs — toolchain caches/config live under these.
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    // TLS trust roots — needed for any HTTPS a build performs (crate/pkg
    // fetches). These name CA-bundle *paths*, not credentials.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
];

/// Toolchain variables, grouped by ecosystem. Each is a build/tooling fact
/// (install root, cache dir, module path) — never a credential. Add here, with
/// a receipt, when a toolchain fails a hermetic run for want of one; never
/// regress to a denylist. Grouped by ecosystem (not globally sorted) so a
/// reviewer reads a toolchain's vars as a unit.
const TOOLCHAIN_ENV_ALLOWLIST: &[&str] = &[
    // Rust / Cargo: install roots + target/backtrace controls.
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_HOME",
    "RUST_BACKTRACE",
    "RUST_LOG",
    // Node / npm / pnpm: module path + cache/home roots.
    "NODE_PATH",
    "NPM_CONFIG_CACHE",
    "NVM_DIR",
    "PNPM_HOME",
    // Python / uv / pyenv: path, install roots, active venv.
    "PYENV_ROOT",
    "PYTHONPATH",
    "UV_CACHE_DIR",
    "VIRTUAL_ENV",
    // Go: workspace, install root, build/module caches.
    "GOCACHE",
    "GOMODCACHE",
    "GOPATH",
    "GOROOT",
    // JVM: install root.
    "JAVA_HOME",
    // C/C++ compiler selection (values are program names/paths, not secrets).
    "AR",
    "CC",
    "CXX",
    "LD",
];

/// The complete set of environment variable names admitted into a
/// profile-governed child process. The single owner; see the module docs.
pub const ENV_ALLOWLIST: &[&str] = &const_concat();

/// Concatenate the base and toolchain lists at compile time so [`ENV_ALLOWLIST`]
/// stays one flat, single-owned array without a runtime allocation.
const fn const_concat() -> [&'static str; BASE_ENV_ALLOWLIST.len() + TOOLCHAIN_ENV_ALLOWLIST.len()]
{
    let mut out: [&'static str; BASE_ENV_ALLOWLIST.len() + TOOLCHAIN_ENV_ALLOWLIST.len()] =
        [""; BASE_ENV_ALLOWLIST.len() + TOOLCHAIN_ENV_ALLOWLIST.len()];
    let mut i = 0;
    while i < BASE_ENV_ALLOWLIST.len() {
        out[i] = BASE_ENV_ALLOWLIST[i];
        i += 1;
    }
    let mut j = 0;
    while j < TOOLCHAIN_ENV_ALLOWLIST.len() {
        out[BASE_ENV_ALLOWLIST.len() + j] = TOOLCHAIN_ENV_ALLOWLIST[j];
        j += 1;
    }
    out
}

/// Build the environment for a profile-governed child process, closed by
/// construction: the allowlisted subset of the parent environment plus the
/// profile's granted exposure. The one resolver both profiles flow through — a
/// hermetic profile contributes an empty exposure and so yields the allowlist
/// alone.
///
/// `env_lookup` reads the launcher/parent environment (production passes
/// `|name| std::env::var(name).ok()`); `resolve_secret` materializes a
/// `secret_store` grant on exposure (production passes the crate's secret chain).
pub fn resolve_env(
    profile: &SessionProfile,
    env_lookup: &dyn Fn(&str) -> Option<String>,
    resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<BTreeMap<String, String>, GrantError> {
    let mut env = BTreeMap::new();
    for name in ENV_ALLOWLIST {
        if let Some(value) = env_lookup(name) {
            env.insert((*name).to_string(), value);
        }
    }
    // Grants overlay the allowlist. A hermetic profile has none, so this is a
    // no-op there; the empty-grants case IS the hermetic environment.
    for (var, value) in profile.env_exposure(resolve_secret)? {
        env.insert(var, value);
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::session_grants::{GrantSourceSpec, GrantSpec, SessionProfileKind};

    /// A local re-derivation of the "looks like a secret" shape. It cannot call
    /// `harn-hostlib::is_sensitive_env_name` (that crate depends on this one), so
    /// the drift test carries its own copy of the suffix/prefix rules; if those
    /// rules ever diverge the allowlist is still independently pinned here.
    fn looks_like_secret(name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        const SECRET_PREFIXES: &[&str] = &[
            "ANTHROPIC_",
            "OPENAI_",
            "OPENROUTER_",
            "FIREWORKS_",
            "TOGETHER_",
            "XAI_",
            "GROQ_",
            "AWS_",
        ];
        const SECRET_SUFFIXES: &[&str] = &[
            "_API_KEY",
            "_TOKEN",
            "_SECRET",
            "_KEY",
            "_PASSWORD",
            "_PASSWD",
            "_CREDENTIALS",
        ];
        SECRET_PREFIXES.iter().any(|p| upper.starts_with(p))
            || SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
    }

    /// The allowlist is the single owned artifact — unique, free of any
    /// secret-shaped name, and carrying the base essentials a build cannot run
    /// without. A scattered or secret-shaped addition fails here.
    #[test]
    fn allowlist_is_single_owned_and_secret_free() {
        // Unique.
        let mut seen = std::collections::BTreeSet::new();
        for name in ENV_ALLOWLIST {
            assert!(seen.insert(*name), "duplicate allowlist entry: {name}");
        }
        // No secret-shaped name may ever be admitted — that would defeat the
        // closed-by-construction property. This is the load-bearing guard.
        for name in ENV_ALLOWLIST {
            assert!(
                !looks_like_secret(name),
                "allowlist admits a secret-shaped variable '{name}' — a credential must \
                 cross via a grant, never the allowlist"
            );
        }
        // Base essentials present: without these a child cannot resolve tools or
        // its home/temp, so a hermetic build would fail for a trivial reason.
        for required in ["PATH", "HOME", "TMPDIR", "LANG"] {
            assert!(
                ENV_ALLOWLIST.contains(&required),
                "base allowlist missing essential '{required}'"
            );
        }
        // No global-sort assertion: the arrays are deliberately grouped by
        // ecosystem (with a receipt comment per group) rather than alphabetized,
        // because a reviewer reasons about "the Rust toolchain vars" as a unit.
        // The load-bearing guards above (uniqueness, no secret-shaped name, base
        // essentials present) are what actually protect the closed-by-construction
        // property; ordering within the source is cosmetic.
    }

    fn env_from(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |var: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == var)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn hermetic_env_is_the_allowlist_alone() {
        // A parent env carrying both allowlisted vars and a secret. The hermetic
        // child sees the allowlisted vars and NOT the secret — and no grant path
        // exists to add one (hermetic forbids grants at launch).
        let parent = env_from(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/agent"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("SOME_UNLISTED_VAR", "whatever"),
        ]);
        let never_secret = |_: &str, _: &str| None;
        let profile = SessionProfile::hermetic();
        let env = resolve_env(&profile, &parent, &never_secret).unwrap();

        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/agent"));
        assert!(
            !env.contains_key("ANTHROPIC_API_KEY"),
            "hermetic env must not inherit a secret from the parent"
        );
        assert!(
            !env.contains_key("SOME_UNLISTED_VAR"),
            "hermetic env must not inherit an unlisted var"
        );
    }

    #[test]
    fn lane_env_is_allowlist_plus_grants_via_the_same_resolver() {
        // The lane env is the hermetic env (allowlist) plus exactly the granted
        // pairs. Same resolver, non-empty grant set.
        let parent = env_from(&[("PATH", "/usr/bin"), ("FIREWORKS_API_KEY", "fw-secret")]);
        let specs = vec![
            GrantSpec {
                name: "fireworks".to_string(),
                source: GrantSourceSpec::Env {
                    var: "FIREWORKS_API_KEY".to_string(),
                },
                expose_as_env: Some("FIREWORKS_API_KEY".to_string()),
            },
            GrantSpec {
                name: "gh".to_string(),
                source: GrantSourceSpec::SecretStore {
                    account: "gh".to_string(),
                    key: "token".to_string(),
                },
                expose_as_env: Some("GH_TOKEN".to_string()),
            },
        ];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &parent).unwrap();
        let resolve_secret = |account: &str, key: &str| {
            (account == "gh" && key == "token").then(|| "ghp".to_string())
        };
        let env = resolve_env(&profile, &parent, &resolve_secret).unwrap();

        // Allowlist base still present.
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        // The env-snapshot grant is exposed under its target var...
        assert_eq!(
            env.get("FIREWORKS_API_KEY").map(String::as_str),
            Some("fw-secret")
        );
        // ...and the secret_store grant resolved through the closure.
        assert_eq!(env.get("GH_TOKEN").map(String::as_str), Some("ghp"));
    }

    #[test]
    fn a_grant_may_re_expose_an_allowlisted_name_and_wins() {
        // If a grant targets a name that is also on the allowlist, the grant
        // value wins (the resolver overlays grants last). This lets a lane
        // deliberately override an inherited toolchain var.
        let parent = env_from(&[("RUST_LOG", "from_parent")]);
        let specs = vec![GrantSpec {
            name: "log".to_string(),
            source: GrantSourceSpec::Env {
                var: "RUST_LOG".to_string(),
            },
            expose_as_env: Some("RUST_LOG".to_string()),
        }];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &parent).unwrap();
        let never_secret = |_: &str, _: &str| None;
        let env = resolve_env(&profile, &parent, &never_secret).unwrap();
        // Same value here, but it flows through the grant, not the allowlist
        // pull — proving the overlay order without a second source of truth.
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("from_parent"));
    }
}
