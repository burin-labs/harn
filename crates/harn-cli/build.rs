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

fn main() {
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
