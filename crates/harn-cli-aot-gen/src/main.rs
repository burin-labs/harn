use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use harn_vm::bytecode_cache::{serialize_chunk_artifact, CacheKey};
use harn_vm::compile_source;

#[path = "../../harn-cli/build_support/cli_aot_manifest.rs"]
#[allow(dead_code)]
mod cli_aot_manifest;
#[path = "../../harn-vm/build_support/codegen_fingerprint.rs"]
#[allow(dead_code)]
mod codegen_fingerprint;

use cli_aot_manifest::{
    canonical_manifest_bytes, canonical_source_text, sha256_bytes, sha256_source_bytes,
    CliAotArtifactRecord, CliAotManifest, CliAotScriptRecord, CLI_AOT_MANIFEST_SCHEMA_VERSION,
};

const CLI_AOT_SKIPLIST: &[(&str, &str)] = &[(
    "codemod",
    "uses host-gated rules_apply/rules_fold paths that are intentionally runtime-only",
)];

struct ExpectedArtifacts {
    manifest: Vec<u8>,
    table: Vec<u8>,
    artifacts: BTreeMap<PathBuf, Vec<u8>>,
}

struct CheckoutContract {
    manifest_dir: PathBuf,
    harn_version: String,
    compiler_fingerprint: String,
}

#[derive(serde::Deserialize)]
struct WorkspaceManifest {
    workspace: WorkspaceTable,
}

#[derive(serde::Deserialize)]
struct WorkspaceTable {
    package: WorkspacePackage,
}

#[derive(serde::Deserialize)]
struct WorkspacePackage {
    version: String,
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (workspace_root, check) = match args.as_slice() {
        [root_flag, root] if root_flag == "--workspace-root" => (PathBuf::from(root), false),
        [root_flag, root, check_flag]
            if root_flag == "--workspace-root" && check_flag == "--check" =>
        {
            (PathBuf::from(root), true)
        }
        _ => {
            eprintln!("usage: harn-cli-aot-gen --workspace-root <path> [--check]");
            return ExitCode::from(2);
        }
    };

    match run(&workspace_root, check) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(workspace_root: &Path, check: bool) -> Result<&'static str, String> {
    let contract = checkout_contract(workspace_root)?;
    let expected = build_expected(&contract)?;
    if check {
        check_expected(&contract.manifest_dir, &expected)?;
        Ok("CLI AOT artifacts are current")
    } else {
        write_expected(&contract.manifest_dir, &expected)?;
        Ok("regenerated CLI AOT artifacts")
    }
}

fn checkout_contract(workspace_root: &Path) -> Result<CheckoutContract, String> {
    let manifest_dir = workspace_root.join("crates/harn-cli");
    if !manifest_dir.is_dir() {
        return Err(format!(
            "workspace root {} does not contain crates/harn-cli",
            workspace_root.display()
        ));
    }

    let workspace_manifest_path = workspace_root.join("Cargo.toml");
    let workspace_manifest = fs::read_to_string(&workspace_manifest_path)
        .map_err(|error| format!("read {}: {error}", workspace_manifest_path.display()))?;
    let workspace_manifest = toml::from_str::<WorkspaceManifest>(&workspace_manifest)
        .map_err(|error| format!("parse {}: {error}", workspace_manifest_path.display()))?;
    let harn_version = workspace_manifest.workspace.package.version;

    let vm_manifest_dir = workspace_root.join("crates/harn-vm");
    let compiler_inputs = codegen_fingerprint::compiler_inputs(&vm_manifest_dir);
    let compiler_fingerprint = codegen_fingerprint::fingerprint_inputs(&compiler_inputs);
    Ok(CheckoutContract {
        manifest_dir,
        harn_version,
        compiler_fingerprint,
    })
}

fn build_expected(contract: &CheckoutContract) -> Result<ExpectedArtifacts, String> {
    if contract.compiler_fingerprint != harn_vm::bytecode_cache::CODEGEN_FINGERPRINT {
        return Err(format!(
            "compiler fingerprint mismatch: source={}, runtime={}",
            contract.compiler_fingerprint,
            harn_vm::bytecode_cache::CODEGEN_FINGERPRINT
        ));
    }
    if contract.harn_version != harn_vm::bytecode_cache::HARN_VERSION {
        return Err(format!(
            "Harn version mismatch: workspace={}, runtime={}",
            contract.harn_version,
            harn_vm::bytecode_cache::HARN_VERSION
        ));
    }

    let mut scripts = Vec::new();
    let mut artifacts = BTreeMap::new();
    for script in harn_stdlib::STDLIB_CLI_SCRIPTS {
        let source_path = format!("../harn-stdlib/src/stdlib/cli/{}.harn", script.name);
        let source_disk_path = contract.manifest_dir.join(&source_path);
        let source = fs::read_to_string(&source_disk_path)
            .map_err(|error| format!("read {}: {error}", source_disk_path.display()))?;
        let source = canonical_source_text(&source);
        let embedded_source = canonical_source_text(script.source);
        if source != embedded_source {
            return Err(format!(
                "embedded source for `{}` differs from {}",
                script.name,
                source_disk_path.display()
            ));
        }

        let source_sha256 = sha256_source_bytes(source.as_bytes());
        if let Some(reason) = cli_aot_skip_reason(script.name) {
            scripts.push(CliAotScriptRecord {
                name: script.name.to_string(),
                source_path,
                source_sha256,
                artifact: None,
                skip_reason: Some(reason.to_string()),
            });
            continue;
        }

        let chunk = compile_source(&source)
            .map_err(|error| format!("compile CLI script `{}`: {error}", script.name))?;
        let safe_name = safe_filename(script.name);
        let synthetic_source = PathBuf::from("stdlib-cli").join(format!("{safe_name}.harn"));
        let key = CacheKey::from_source(&synthetic_source, &source);
        let bytes = serialize_chunk_artifact(&key, &chunk)
            .map_err(|error| format!("serialize CLI script `{}`: {error}", script.name))?;
        let artifact_path = PathBuf::from("generated")
            .join("cli-bytecode")
            .join(format!("{safe_name}.harnbc"));
        scripts.push(CliAotScriptRecord {
            name: script.name.to_string(),
            source_path,
            source_sha256,
            artifact: Some(CliAotArtifactRecord {
                path: normalize_path(&artifact_path),
                sha256: sha256_bytes(&bytes),
            }),
            skip_reason: None,
        });
        artifacts.insert(artifact_path, bytes);
    }

    let manifest = CliAotManifest {
        schema_version: CLI_AOT_MANIFEST_SCHEMA_VERSION,
        harn_version: contract.harn_version.clone(),
        compiler_fingerprint: contract.compiler_fingerprint.clone(),
        scripts,
    };
    let table = render_table(&manifest)?;
    Ok(ExpectedArtifacts {
        manifest: canonical_manifest_bytes(&manifest)?,
        table,
        artifacts,
    })
}

fn render_table(manifest: &CliAotManifest) -> Result<Vec<u8>, String> {
    let mut body = String::new();
    writeln!(body, "// @generated by harn-cli-aot-gen. Do not edit.").expect("write String");
    writeln!(
        body,
        "pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE: &[(&str, &[u8])] = &["
    )
    .expect("write String");
    for script in &manifest.scripts {
        if let Some(artifact) = &script.artifact {
            let relative = artifact.path.strip_prefix("generated/").ok_or_else(|| {
                format!(
                    "artifact path must live under generated/: {}",
                    artifact.path
                )
            })?;
            writeln!(
                body,
                "    ({:?}, include_bytes!({:?})),",
                script.name, relative
            )
            .expect("write String");
        }
    }
    writeln!(body, "];").expect("write String");
    writeln!(body, "#[allow(dead_code)]").expect("write String");
    writeln!(
        body,
        "pub(crate) const STDLIB_CLI_SCRIPT_BYTECODE_SKIPPED: &[(&str, &str)] = &["
    )
    .expect("write String");
    for script in &manifest.scripts {
        if let Some(reason) = &script.skip_reason {
            writeln!(body, "    ({:?}, {:?}),", script.name, reason).expect("write String");
        }
    }
    writeln!(body, "];").expect("write String");
    Ok(body.into_bytes())
}

fn check_expected(manifest_dir: &Path, expected: &ExpectedArtifacts) -> Result<(), String> {
    let manifest_path = manifest_dir.join("generated/cli-bytecode-manifest.json");
    compare_file(&manifest_path, &expected.manifest)?;
    compare_file(
        &manifest_dir.join("generated/cli_bytecode_table.rs"),
        &expected.table,
    )?;
    for (relative, bytes) in &expected.artifacts {
        compare_file(&manifest_dir.join(relative), bytes)?;
    }

    let artifact_dir = manifest_dir.join("generated/cli-bytecode");
    let expected_paths = expected
        .artifacts
        .keys()
        .map(|path| manifest_dir.join(path))
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(&artifact_dir)
        .map_err(|error| format!("read {}: {error}", artifact_dir.display()))?
    {
        let path = entry
            .map_err(|error| format!("read {} entry: {error}", artifact_dir.display()))?
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == "harnbc")
            && !expected_paths.contains(&path)
        {
            return Err(format!(
                "unexpected generated artifact {}; run `make gen-cli-aot`",
                path.display()
            ));
        }
    }
    Ok(())
}

fn write_expected(manifest_dir: &Path, expected: &ExpectedArtifacts) -> Result<(), String> {
    let artifact_dir = manifest_dir.join("generated/cli-bytecode");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("create {}: {error}", artifact_dir.display()))?;
    let expected_paths = expected
        .artifacts
        .keys()
        .map(|path| manifest_dir.join(path))
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(&artifact_dir)
        .map_err(|error| format!("read {}: {error}", artifact_dir.display()))?
    {
        let path = entry
            .map_err(|error| format!("read {} entry: {error}", artifact_dir.display()))?
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == "harnbc")
            && !expected_paths.contains(&path)
        {
            fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
        }
    }

    for (relative, bytes) in &expected.artifacts {
        let path = manifest_dir.join(relative);
        fs::write(&path, bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    let manifest_path = manifest_dir.join("generated/cli-bytecode-manifest.json");
    fs::write(&manifest_path, &expected.manifest)
        .map_err(|error| format!("write {}: {error}", manifest_path.display()))?;
    let table_path = manifest_dir.join("generated/cli_bytecode_table.rs");
    fs::write(&table_path, &expected.table)
        .map_err(|error| format!("write {}: {error}", table_path.display()))?;
    Ok(())
}

fn compare_file(path: &Path, expected: &[u8]) -> Result<(), String> {
    let actual = fs::read(path).map_err(|error| {
        format!(
            "read generated artifact {}: {error}; run `make gen-cli-aot`",
            path.display()
        )
    })?;
    if actual != expected {
        return Err(format!(
            "generated artifact {} is stale; run `make gen-cli-aot`",
            path.display()
        ));
    }
    Ok(())
}

fn cli_aot_skip_reason(name: &str) -> Option<&'static str> {
    CLI_AOT_SKIPLIST
        .iter()
        .find_map(|(script, reason)| (*script == name).then_some(*reason))
}

fn safe_filename(name: &str) -> String {
    name.replace('/', "-")
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn manifest(path: &str) -> CliAotManifest {
        CliAotManifest {
            schema_version: CLI_AOT_MANIFEST_SCHEMA_VERSION,
            harn_version: "1.2.3".to_string(),
            compiler_fingerprint: "a".repeat(64),
            scripts: vec![
                CliAotScriptRecord {
                    name: "doctor".to_string(),
                    source_path: "../harn-stdlib/src/stdlib/cli/doctor.harn".to_string(),
                    source_sha256: "b".repeat(64),
                    artifact: Some(CliAotArtifactRecord {
                        path: path.to_string(),
                        sha256: "c".repeat(64),
                    }),
                    skip_reason: None,
                },
                CliAotScriptRecord {
                    name: "codemod".to_string(),
                    source_path: "../harn-stdlib/src/stdlib/cli/codemod.harn".to_string(),
                    source_sha256: "d".repeat(64),
                    artifact: None,
                    skip_reason: Some("runtime only".to_string()),
                },
            ],
        }
    }

    #[test]
    fn rendered_table_is_stable_and_uses_generated_relative_paths() {
        let value = manifest("generated/cli-bytecode/doctor.harnbc");
        let first = render_table(&value).expect("render table");
        let second = render_table(&value).expect("render table again");
        assert_eq!(first, second);
        let text = String::from_utf8(first).expect("generated Rust is UTF-8");
        assert!(text.contains("include_bytes!(\"cli-bytecode/doctor.harnbc\")"));
        assert!(text.contains("(\"codemod\", \"runtime only\")"));
    }

    #[test]
    fn rendered_table_rejects_paths_outside_generated_root() {
        let error = render_table(&manifest("../outside.harnbc")).expect_err("path must fail");
        assert!(error.contains("must live under generated"));
    }

    #[test]
    fn checkout_contract_uses_the_requested_workspace() {
        let first = test_workspace("1.2.3", "first");
        let second = test_workspace("9.8.7", "second");

        let first_contract = checkout_contract(first.path()).expect("first contract");
        let second_contract = checkout_contract(second.path()).expect("second contract");

        assert_eq!(first_contract.harn_version, "1.2.3");
        assert_eq!(second_contract.harn_version, "9.8.7");
        assert_ne!(
            first_contract.compiler_fingerprint,
            second_contract.compiler_fingerprint
        );
        assert_eq!(
            second_contract.manifest_dir,
            second.path().join("crates/harn-cli")
        );
    }

    #[test]
    fn checkout_contract_tracks_source_and_version_mutations() {
        let workspace = test_workspace("1.2.3", "before");
        let before = checkout_contract(workspace.path()).expect("initial contract");

        fs::write(
            workspace.path().join("crates/harn-vm/src/chunk.rs"),
            "after\n",
        )
        .expect("mutate compiler input");
        let after_source = checkout_contract(workspace.path()).expect("source contract");
        assert_ne!(
            before.compiler_fingerprint,
            after_source.compiler_fingerprint
        );

        fs::write(
            workspace.path().join("Cargo.toml"),
            "[workspace.package]\nversion = \"4.5.6\"\n",
        )
        .expect("mutate workspace version");
        let after_version = checkout_contract(workspace.path()).expect("version contract");
        assert_eq!(after_version.harn_version, "4.5.6");
    }

    fn test_workspace(version: &str, compiler_source: &str) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("temp workspace");
        fs::create_dir_all(workspace.path().join("crates/harn-cli")).expect("create harn-cli");
        fs::create_dir_all(workspace.path().join("crates/harn-vm/src")).expect("create harn-vm");
        let mut manifest = fs::File::create(workspace.path().join("Cargo.toml"))
            .expect("create workspace manifest");
        writeln!(manifest, "[workspace.package]\nversion = {version:?}")
            .expect("write workspace manifest");
        fs::write(
            workspace.path().join("crates/harn-vm/src/chunk.rs"),
            compiler_source,
        )
        .expect("write compiler input");
        workspace
    }
}
