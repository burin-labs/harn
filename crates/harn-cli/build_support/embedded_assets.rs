use std::fs;
use std::path::Path;

const EMBEDDED_ASSET_DIRS: &[&str] = &["assets/persona-templates", "portal-dist"];
const PORTAL_FALLBACK_HTML: &str =
    "<!doctype html><html><head><title>Harn portal not built</title></head>\
     <body><h1>Harn portal not built</h1>\
     <p>Run <code>./scripts/dev_setup.sh</code> or <code>make setup</code> \
     to install portal dependencies and build the frontend, or run \
     <code>npm --prefix crates/harn-cli/portal run build</code> directly, \
     to populate <code>crates/harn-cli/portal-dist</code>.</p></body></html>";
const PORTAL_ENTRY_ASSETS: &[&str] = &["app.js", "styles.css"];

/// Materialize the same stable entry points as the production portal build.
/// Keeping ghost placeholders here makes Cargo's prior dep-info name files
/// that Vite must delete, forcing freshness recovery before the next build.
pub(crate) fn ensure_portal_fallback(manifest_dir: &Path) {
    let portal_dist = manifest_dir.join("portal-dist");
    let index = portal_dist.join("index.html");
    if index.exists() {
        return;
    }

    fs::create_dir_all(&portal_dist).expect("create portal-dist");
    fs::write(&index, PORTAL_FALLBACK_HTML).expect("write placeholder portal index.html");
    let assets = portal_dist.join("assets").join("portal");
    fs::create_dir_all(&assets).expect("create portal-dist assets dir");
    for entry in PORTAL_ENTRY_ASSETS {
        let path = assets.join(entry);
        if !path.exists() {
            fs::write(&path, b"").expect("write placeholder portal asset");
        }
    }
}

/// Keep every directory embedded by `include_dir!` under one Cargo-owned watch
/// contract. The production build calls this after the portal fallback exists,
/// so Cargo observes nested portal assets on fresh checkouts and real builds.
pub(crate) fn emit_watches(manifest_dir: &Path) {
    for relative_dir in EMBEDDED_ASSET_DIRS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(relative_dir).display()
        );
    }
}
