// portal-dist/ is a gitignored build artifact produced by `npm run build`
// in crates/harn-cli/portal. It is embedded at compile time via `include_dir!`
// in src/commands/portal/assets.rs, which proc-macro-panics if the directory
// is missing. On a fresh clone (or in any context where the portal has not
// been built yet), drop a minimal placeholder so `cargo check` / `cargo build`
// succeeds without requiring npm. The placeholder is only created when a real
// build has not already populated the directory; real `npm run build` output
// uses `emptyOutDir: true`, so it transparently overwrites the placeholder.
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build_support/build_revision.rs"]
mod build_revision;
#[path = "build_support/cli_aot_manifest.rs"]
#[allow(dead_code)]
mod cli_aot_manifest;

const CLI_AOT_MANIFEST: &str = "generated/cli-bytecode-manifest.json";
const CLI_BYTECODE_MAGIC: &[u8; 8] = b"HARNBC\0\0";
const EMBEDDED_ASSET_DIRS: &[&str] = &["assets/persona-templates", "portal-dist"];
const CLI_AOT_REQUIRED_ENV: &str = "HARN_REQUIRE_CLI_AOT";

fn main() {
    build_revision::emit();
    ensure_git_hooks_installed();
    emit_cli_script_bytecode();
    emit_demo_sibling_assets();
    emit_check_fingerprint();

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

    emit_embedded_asset_watches(&manifest_dir);
}

/// Fingerprint the check pipeline's own sources — `harn-lint`, this crate's
/// `commands/check` (typecheck driver, lint bridge, preflight scans, the
/// result cache itself), and `package` (CheckConfig parsing) — and bake the
/// digest in as `HARN_CHECK_FINGERPRINT`. The check-result cache folds it
/// into every key, so a within-version edit to lint or preflight logic
/// invalidates stale cached diagnostics automatically, exactly like
/// `HARN_CODEGEN_FINGERPRINT` does for compiled bytecode (#2621). The
/// lexer/parser/typechecker/compiler are covered separately: the cache key's
/// import-graph hash already folds in the codegen fingerprint.
fn emit_check_fingerprint() {
    use sha2::{Digest, Sha256};
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir
        .parent()
        .expect("harn-cli sits under crates/")
        .to_path_buf();
    let roots = [
        crates_dir.join("harn-lint").join("src"),
        manifest_dir.join("src").join("commands").join("check"),
        manifest_dir.join("src").join("package"),
    ];
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        println!("cargo:rerun-if-changed={}", root.display());
        collect_rs_files(root, &mut files);
    }
    files.sort();
    let mut hasher = Sha256::new();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let content = fs::read(file).unwrap_or_default();
        // Fold the path relative to crates/ so a rename changes the digest
        // but a different checkout location does not.
        let logical = file
            .strip_prefix(&crates_dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(logical.as_bytes());
        hasher.update([0u8]);
        hasher.update(&content);
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    println!("cargo:rustc-env=HARN_CHECK_FINGERPRINT={hex}");
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
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

/// Embed an optional release/package CLI AOT payload.
///
/// The generator validates source and compiler freshness while it has the full
/// workspace. This consumer deliberately validates only package-local facts:
/// an extracted or published `harn-cli` crate has neither sibling workspace
/// sources nor a reason to reject a payload merely because a developer edited
/// them after generating it. Development builds safely use source fallback;
/// release/package verification sets `HARN_REQUIRE_CLI_AOT=1` to fail closed.
fn emit_cli_script_bytecode() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let manifest_path = manifest_dir.join(CLI_AOT_MANIFEST);
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    println!("cargo:rerun-if-env-changed={CLI_AOT_REQUIRED_ENV}");
    let required = cli_aot_required();
    let table = cli_aot_manifest::read_manifest(&manifest_path)
        .and_then(|manifest| render_cli_aot_table(&manifest_dir, &manifest));

    let table = match table {
        Ok(table) => table,
        Err(error) if required => {
            panic!(
                "CLI AOT payload is required ({CLI_AOT_REQUIRED_ENV}=1) but unavailable: {error}; run `make gen-cli-aot` before packaging or releasing"
            );
        }
        // A source checkout normally has no release/package payload. Keep that
        // expected development path quiet; required release/package builds use
        // the branch above and fail with the full validation error instead.
        Err(_) => empty_cli_aot_table(),
    };
    fs::write(out_dir.join("cli_bytecode_table.rs"), table)
        .expect("write CLI AOT table into OUT_DIR");
}

fn cli_aot_required() -> bool {
    let Some(value) = std::env::var_os(CLI_AOT_REQUIRED_ENV) else {
        return false;
    };
    !matches!(
        value.to_string_lossy().trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

fn render_cli_aot_table(
    manifest_dir: &Path,
    manifest: &cli_aot_manifest::CliAotManifest,
) -> Result<String, String> {
    let package_version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    if manifest.harn_version != package_version {
        return Err(format!(
            "CLI AOT payload targets Harn {}; package version is {package_version}",
            manifest.harn_version
        ));
    }

    let mut table = String::new();
    writeln!(table, "// @generated by harn-cli/build.rs. Do not edit.").expect("write String");
    writeln!(table, "#[allow(dead_code)]").expect("write String");
    writeln!(
        table,
        "pub(crate) const CLI_AOT_PAYLOAD_PRESENT: bool = true;"
    )
    .expect("write String");
    writeln!(
        table,
        "pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE: &[(&str, &[u8])] = &["
    )
    .expect("write String");

    let mut artifact_count = 0;
    for script in &manifest.scripts {
        if let Some(artifact) = &script.artifact {
            if !artifact.path.starts_with("generated/cli-bytecode/") {
                return Err(format!(
                    "CLI AOT artifact `{}` must live under generated/cli-bytecode/",
                    script.name
                ));
            }
            let artifact_path = resolve_cli_aot_payload_path(manifest_dir, &artifact.path)?;
            println!("cargo:rerun-if-changed={}", artifact_path.display());
            let bytes = fs::read(&artifact_path)
                .map_err(|error| format!("read {}: {error}", artifact_path.display()))?;
            if !bytes.starts_with(CLI_BYTECODE_MAGIC) {
                return Err(format!(
                    "CLI AOT artifact `{}` has an invalid bytecode header",
                    script.name
                ));
            }
            if artifact.sha256 != cli_aot_manifest::sha256_bytes(&bytes) {
                return Err(format!(
                    "CLI AOT artifact `{}` has a mismatched SHA-256",
                    script.name
                ));
            }
            writeln!(
                table,
                "    (\"{}\", include_bytes!(\"{}\")),",
                escape_str(&script.name),
                escape_str(&artifact_path.to_string_lossy())
            )
            .expect("write String");
            artifact_count += 1;
        }
    }
    if artifact_count == 0 {
        return Err("CLI AOT payload contains no bytecode artifacts".to_string());
    }
    writeln!(table, "];").expect("write String");
    writeln!(table, "#[allow(dead_code)]").expect("write String");
    writeln!(
        table,
        "pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE_SKIPPED: &[(&str, &str)] = &["
    )
    .expect("write String");
    for script in &manifest.scripts {
        if let Some(reason) = &script.skip_reason {
            writeln!(
                table,
                "    (\"{}\", \"{}\"),",
                escape_str(&script.name),
                escape_str(reason)
            )
            .expect("write String");
        }
    }
    writeln!(table, "];").expect("write String");
    Ok(table)
}

fn empty_cli_aot_table() -> String {
    String::from(
        "// No release/package CLI AOT payload was supplied.\n\
     #[allow(dead_code)]\n\
     pub(crate) const CLI_AOT_PAYLOAD_PRESENT: bool = false;\n\
     pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE: &[(&str, &[u8])] = &[];\n\
     #[allow(dead_code)]\n\
     pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE_SKIPPED: &[(&str, &str)] = &[];\n",
    )
}

fn resolve_cli_aot_payload_path(manifest_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(format!("CLI AOT payload path must be relative: {relative}"));
    }
    let root = fs::canonicalize(manifest_dir)
        .map_err(|error| format!("canonicalize {}: {error}", manifest_dir.display()))?;
    let resolved = fs::canonicalize(manifest_dir.join(candidate))
        .map_err(|error| format!("canonicalize CLI AOT payload {relative}: {error}"))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "CLI AOT payload path escapes the package: {relative}"
        ));
    }
    Ok(resolved)
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

/// Keep every directory embedded by `include_dir!` under one Cargo-owned watch
/// contract. This runs after the portal fallback exists so Cargo can observe
/// nested portal assets on fresh checkouts as well as real frontend builds.
fn emit_embedded_asset_watches(manifest_dir: &Path) {
    for relative_dir in EMBEDDED_ASSET_DIRS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(relative_dir).display()
        );
    }
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
