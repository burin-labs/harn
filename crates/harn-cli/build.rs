// portal-dist/ is a gitignored build artifact produced by `npm run build`
// in crates/harn-cli/portal. It is embedded at compile time via `include_dir!`
// in src/commands/portal/assets.rs, which proc-macro-panics if the directory
// is missing. On a fresh clone (or in any context where the portal has not
// been built yet), drop a minimal placeholder so `cargo check` / `cargo build`
// succeeds without requiring npm. The placeholder is only created when a real
// build has not already populated the directory; real `npm run build` output
// uses `emptyOutDir: true`, so it transparently overwrites the placeholder.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    ensure_git_hooks_installed();

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let portal_dist = manifest_dir.join("portal-dist");
    let index = portal_dist.join("index.html");

    if !index.exists() {
        fs::create_dir_all(&portal_dist).expect("create portal-dist");
        fs::write(
            &index,
            "<!doctype html><html><head><title>Harn portal not built</title></head>\
             <body><h1>Harn portal not built</h1>\
             <p>Run <code>./scripts/dev_setup.sh</code> or <code>make setup</code> \
             to install portal dependencies and build the frontend, or run \
             <code>npm --prefix crates/harn-cli/portal run build</code> directly, \
             to populate \
             <code>crates/harn-cli/portal-dist</code>.</p></body></html>",
        )
        .expect("write placeholder portal index.html");

        // The portal router also serves static assets from
        // portal-dist/assets/portal/. Emit empty stubs for the entry
        // points a real build produces so asset-routing tests pass
        // without requiring npm. `emptyOutDir: true` in vite config
        // overwrites these on a real build.
        let assets = portal_dist.join("assets").join("portal");
        fs::create_dir_all(&assets).expect("create portal-dist assets dir");
        for stub in ["app.js", "api.js", "styles.css"] {
            let path = assets.join(stub);
            if !path.exists() {
                fs::write(&path, b"").expect("write placeholder portal asset");
            }
        }
    }

    println!("cargo:rerun-if-changed=portal-dist");
}

/// Self-heal `core.hooksPath` to `.githooks` when building inside the
/// Harn working tree. Without this, contributors who set up the repo
/// before `make install-hooks` existed (or whose config drifted to the
/// default `.git/hooks` for any reason) can commit code that the
/// pre-commit + pre-push hooks would have caught — `harn fmt --check`
/// drift on freshly added conformance fixtures, markdown-lint
/// regressions, etc. — only to discover the failure in CI.
///
/// Safe to no-op:
/// - Skip when `HARN_DISABLE_AUTO_HOOK_SETUP=1` so downstream
///   consumers (and CI runners that run the binary as a published
///   crate) can opt out.
/// - Skip when `git` is not on PATH or the working tree isn't a Harn
///   checkout (no `.githooks` dir adjacent to the resolved repo root).
/// - Never fail the build: any error short-circuits silently and the
///   real cargo build proceeds.
fn ensure_git_hooks_installed() {
    if std::env::var_os("HARN_DISABLE_AUTO_HOOK_SETUP").is_some() {
        return;
    }
    // Resolve the repo's top level. If we're not in a git repo (e.g.
    // installed via `cargo install`), skip silently.
    let Ok(top) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return;
    };
    if !top.status.success() {
        return;
    }
    let toplevel = String::from_utf8_lossy(&top.stdout).trim().to_string();
    if toplevel.is_empty() {
        return;
    }
    let hooks_dir = PathBuf::from(&toplevel).join(".githooks");
    if !hooks_dir.is_dir() {
        // Not a Harn checkout — don't touch a foreign repo's config.
        return;
    }

    let current = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if current == ".githooks" {
        return;
    }
    let _ = Command::new("git")
        .args(["config", "core.hooksPath", ".githooks"])
        .status();
}
