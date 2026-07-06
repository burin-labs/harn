// portal-dist/ is a gitignored build artifact produced by `npm run build`
// in crates/harn-cli/portal. It is embedded at compile time via `include_dir!`
// in src/commands/portal/assets.rs, which proc-macro-panics if the directory
// is missing. On a fresh clone (or in any context where the portal has not
// been built yet), drop a minimal placeholder so `cargo check` / `cargo build`
// succeeds without requiring npm. The placeholder is only created when a real
// build has not already populated the directory; real `npm run build` output
// uses `emptyOutDir: true`, so it transparently overwrites the placeholder.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CLI_AOT_SKIPLIST: &[(&str, &str)] = &[(
    "codemod",
    "uses host-gated rules_apply/rules_fold paths that are intentionally runtime-only",
)];

fn main() {
    ensure_git_hooks_installed();
    emit_cli_script_bytecode();
    emit_demo_sibling_assets();

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

fn emit_rerun_if_changed_recursive(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    children.sort();

    for child in children {
        if child.is_dir() {
            emit_rerun_if_changed_recursive(&child);
        } else {
            println!("cargo:rerun-if-changed={}", child.display());
        }
    }
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

/// AOT-compile every embedded CLI script into the on-disk bytecode-cache
/// artifact the runtime loader already understands (header + bincode
/// payload, identical to what `harn precompile` writes). Each artifact is
/// written under `$OUT_DIR/cli-bytecode/` and registered in a generated
/// `cli_bytecode_table.rs` that `harn-cli` includes at compile time.
///
/// This is part of G7 (harn#2300) under the CLI self-host epic
/// (harn#2293). Cold-start cost for ported subcommands drops because
/// the runtime can skip parse + typecheck + compile entirely — at
/// dispatch time the wedge drops the embedded `.harnbc` next to the
/// temp source and the existing `bytecode_cache::load` path picks it up.
///
/// A compile failure here would block the whole build; that's
/// intentional. The CLI scripts are versioned in this repo, so a
/// regression in any of them needs to surface at build time rather than
/// silently degrade cold-start. If a future script can't be statically
/// compiled (e.g. relies on runtime-only typing), it should be added to
/// `BYTECODE_SKIPLIST` below with a reason.
fn emit_cli_script_bytecode() {
    use harn_vm::bytecode_cache::{serialize_chunk_artifact, CacheKey};
    use harn_vm::compile_source;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let bytecode_dir = out_dir.join("cli-bytecode");
    fs::create_dir_all(&bytecode_dir).expect("create cli-bytecode dir");

    // Rebuild whenever any embedded CLI script changes. CARGO_MANIFEST_DIR
    // is the harn-cli crate, but the scripts live in ../harn-stdlib/src/
    // stdlib/cli/. Recursing the directory listing keeps the watch list
    // accurate as the script set grows.
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let cli_scripts_dir = manifest_dir
        .join("..")
        .join("harn-stdlib")
        .join("src")
        .join("stdlib")
        .join("cli");
    emit_rerun_if_changed_recursive(&cli_scripts_dir);
    // Watch the stdlib lib.rs too because that's where script registration
    // lives — adding/removing entries from STDLIB_CLI_SCRIPTS must rerun
    // this build script even if no `.harn` file changed.
    let stdlib_lib = manifest_dir
        .join("..")
        .join("harn-stdlib")
        .join("src")
        .join("lib.rs");
    println!("cargo:rerun-if-changed={}", stdlib_lib.display());
    // Opt-out for hermetic builds that can't tolerate any compile-time
    // bytecode generation (e.g. cross-compiling in a sandbox without
    // enough memory). When unset (the default) we always emit.
    println!("cargo:rerun-if-env-changed=HARN_SKIP_AOT_CLI_BUILD");

    let table_path = out_dir.join("cli_bytecode_table.rs");

    if std::env::var_os("HARN_SKIP_AOT_CLI_BUILD").is_some() {
        // Emit an empty table so `include!` still works. Dispatch falls
        // back to source compilation transparently.
        write_table(&table_path, &[], &[]);
        return;
    }

    // Windows hits STATUS_STACK_OVERFLOW invoking `compile_source` from
    // the build-script default thread (8 MiB on Linux/macOS, ~1 MiB on
    // Windows). The compiler's recursive walks need more headroom than
    // the default Windows stack provides, and the build script runs
    // before `RUST_MIN_STACK` can take effect. Skip AOT on Windows —
    // dispatch falls back to source compilation transparently and the
    // first-run bytecode cache (HARN_BYTECODE_CACHE) still kicks in.
    // Re-enable when the compiler hot path is rewritten to be
    // iteration-bounded, or when a spawn-thread-with-stack-size shim
    // wraps `compile_source` in this build script.
    if cfg!(target_os = "windows") {
        write_table(&table_path, &[], &[]);
        return;
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for script in harn_stdlib::STDLIB_CLI_SCRIPTS {
        let name = script.name;
        let source = script.source;
        let safe = safe_filename(name);

        if let Some(reason) = cli_aot_skip_reason(name) {
            skipped.push((name.to_string(), reason.to_string()));
            continue;
        }

        let chunk = match compile_source(source) {
            Ok(chunk) => chunk,
            Err(err) => panic!(
                "AOT compile failed for CLI script `{name}`: {err}\n\
                 (this is a build-time failure; the script must compile cleanly \
                 or be guarded with a skiplist entry in build.rs)"
            ),
        };

        // Build a key against a synthetic source path. The runtime loader
        // recomputes the key from the tempfile path at dispatch time, but
        // the import-graph hash for these scripts is over zero user
        // imports (they only import std/*), and the source hash is over
        // content alone — so the build-time and runtime keys match by
        // construction. A future script that grows user imports would
        // break this assumption and silently fall back to source; that's
        // acceptable since dispatch already handles miss gracefully.
        let synthetic_path = bytecode_dir.join(format!("{safe}.harn"));
        let key = CacheKey::from_source(&synthetic_path, source);
        let buf = serialize_chunk_artifact(&key, &chunk).unwrap_or_else(|err| {
            panic!("serialize bytecode for CLI script `{name}` failed: {err}");
        });

        let dest = bytecode_dir.join(format!("{safe}.harnbc"));
        fs::write(&dest, &buf).unwrap_or_else(|err| {
            panic!(
                "write bytecode for CLI script `{name}` to {}: {err}",
                dest.display()
            );
        });

        entries.push((name.to_string(), dest.to_string_lossy().into_owned()));
    }

    write_table(&table_path, &entries, &skipped);
}

fn cli_aot_skip_reason(name: &str) -> Option<&'static str> {
    CLI_AOT_SKIPLIST
        .iter()
        .find_map(|(script, reason)| (*script == name).then_some(*reason))
}

/// Embed every demo *sibling* file (anything under `assets/demo/<id>/` other
/// than `scenario.harn`, `tape.jsonl`, and the gitignored `.harn/` run-record
/// dir) so `harn demo` can materialize multi-file scenarios into its tempdir:
/// `.harn.prompt` templates, imported modules, fixtures, and so on. The
/// scenario script + tape stay embedded separately via `include_str!` in
/// `demo.rs`; this table carries only the extra files.
///
/// We do this in `build.rs` rather than `include_dir!("assets/demo")` so the
/// `.harn/` artifacts a local `harn demo` run leaves behind are never embedded:
/// the binary stays reproducible and free of local cruft regardless of the
/// developer's working-tree state.
fn emit_demo_sibling_assets() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let demo_root = manifest_dir.join("assets").join("demo");
    emit_rerun_if_changed_recursive(&demo_root);

    let mut entries: Vec<(String, String)> = Vec::new();
    collect_demo_siblings(&demo_root, &demo_root, &mut entries);
    entries.sort();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    write_demo_assets_table(&out_dir.join("demo_assets_table.rs"), &entries);
}

/// Recurse `dir`, recording each embeddable sibling file as
/// (`<id>/<relative path>`, absolute source path). Skips the demo's primary
/// `scenario.harn` / `tape.jsonl` (embedded elsewhere) and the `.harn/` dir.
fn collect_demo_siblings(root: &Path, dir: &Path, entries: &mut Vec<(String, String)>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();

    for path in children {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name == ".harn" {
                continue;
            }
            collect_demo_siblings(root, &path, entries);
        } else {
            if name == "scenario.harn" || name == "tape.jsonl" {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            entries.push((rel, path.to_string_lossy().into_owned()));
        }
    }
}

fn write_demo_assets_table(path: &Path, entries: &[(String, String)]) {
    let mut body = String::new();
    body.push_str("// @generated by build.rs. Demo sibling files embedded for\n");
    body.push_str("// `harn demo` multi-file scenarios. Do not edit by hand.\n");
    body.push_str("pub(crate) const DEMO_SIBLING_FILES: &[(&str, &[u8])] = &[\n");
    for (rel, file_path) in entries {
        body.push_str("    (\"");
        body.push_str(&escape_str(rel));
        body.push_str("\", include_bytes!(\"");
        body.push_str(&escape_str(file_path));
        body.push_str("\")),\n");
    }
    body.push_str("];\n");
    fs::write(path, body).expect("write demo_assets_table.rs");
}

/// Tempfiles produced by the dispatch wedge use a single-segment name
/// derived from the script id with `/` → `-`. Mirror that here so the
/// build-time and runtime sources agree on filename layout (the actual
/// content addressing happens via the key inside the artifact header).
fn safe_filename(name: &str) -> String {
    name.replace('/', "-")
}

fn write_table(path: &Path, entries: &[(String, String)], skipped: &[(String, String)]) {
    let mut body = String::new();
    body.push_str("// @generated by build.rs (harn#2300, G7 AOT bytecode embedding).\n");
    body.push_str("// Do not edit by hand. Rerun `cargo build -p harn-cli` to regenerate.\n");
    body.push_str("pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE: &[(&str, &[u8])] = &[\n");
    for (name, file_path) in entries {
        body.push_str("    (\"");
        body.push_str(&escape_str(name));
        body.push_str("\", include_bytes!(\"");
        body.push_str(&escape_str(file_path));
        body.push_str("\")),\n");
    }
    body.push_str("];\n");
    body.push_str("#[allow(dead_code)]\n");
    body.push_str("pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE_SKIPPED: &[(&str, &str)] = &[\n");
    for (name, reason) in skipped {
        body.push_str("    (\"");
        body.push_str(&escape_str(name));
        body.push_str("\", \"");
        body.push_str(&escape_str(reason));
        body.push_str("\"),\n");
    }
    body.push_str("];\n");
    fs::write(path, body).expect("write cli_bytecode_table.rs");
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}
