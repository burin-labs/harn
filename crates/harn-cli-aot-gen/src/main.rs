use std::collections::{BTreeMap, BTreeSet};
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
    artifacts: BTreeMap<PathBuf, Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
struct CliOptions {
    workspace_root: PathBuf,
    check: bool,
    artifact_version: Option<String>,
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
    let options = match parse_options(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!(
                "usage: harn-cli-aot-gen --workspace-root <path> \
                 [--artifact-version <version>] [--check]"
            );
            return ExitCode::from(2);
        }
    };

    match run(
        &options.workspace_root,
        options.check,
        options.artifact_version.as_deref(),
    ) {
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

fn parse_options(args: &[String]) -> Result<CliOptions, String> {
    let mut workspace_root = None;
    let mut artifact_version = None;
    let mut check = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace-root" => {
                if workspace_root.is_some() {
                    return Err("--workspace-root may be supplied only once".to_string());
                }
                index += 1;
                let value = args
                    .get(index)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--workspace-root requires a path".to_string())?;
                workspace_root = Some(PathBuf::from(value));
            }
            "--artifact-version" => {
                if artifact_version.is_some() {
                    return Err("--artifact-version may be supplied only once".to_string());
                }
                index += 1;
                let value = args
                    .get(index)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--artifact-version requires a version".to_string())?;
                artifact_version = Some(value.clone());
            }
            "--check" => {
                if check {
                    return Err("--check may be supplied only once".to_string());
                }
                check = true;
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
        index += 1;
    }
    Ok(CliOptions {
        workspace_root: workspace_root.ok_or_else(|| "--workspace-root is required".to_string())?,
        check,
        artifact_version,
    })
}

fn run(
    workspace_root: &Path,
    check: bool,
    artifact_version: Option<&str>,
) -> Result<&'static str, String> {
    let contract = checkout_contract(workspace_root)?;
    let expected = build_expected(&contract, artifact_version)?;
    if check {
        check_expected(&contract.manifest_dir, &expected)?;
        Ok("CLI AOT payload is current")
    } else {
        write_expected(&contract.manifest_dir, &expected)?;
        Ok("generated CLI AOT payload")
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

fn validate_artifact_version(
    contract: &CheckoutContract,
    artifact_version: Option<&str>,
) -> Result<(), String> {
    if let Some(requested) = artifact_version {
        if requested != contract.harn_version {
            return Err(format!(
                "artifact version mismatch: requested={requested}, workspace={}",
                contract.harn_version
            ));
        }
        return Ok(());
    }
    if contract.harn_version != harn_vm::bytecode_cache::HARN_VERSION {
        return Err(format!(
            "Harn version mismatch: workspace={}, runtime={}; release preparation must pass \
             --artifact-version {} explicitly",
            contract.harn_version,
            harn_vm::bytecode_cache::HARN_VERSION,
            contract.harn_version
        ));
    }
    Ok(())
}

fn build_expected(
    contract: &CheckoutContract,
    artifact_version: Option<&str>,
) -> Result<ExpectedArtifacts, String> {
    if contract.compiler_fingerprint != harn_vm::bytecode_cache::CODEGEN_FINGERPRINT {
        return Err(format!(
            "compiler fingerprint mismatch: source={}, runtime={}",
            contract.compiler_fingerprint,
            harn_vm::bytecode_cache::CODEGEN_FINGERPRINT
        ));
    }
    validate_artifact_version(contract, artifact_version)?;

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
    Ok(ExpectedArtifacts {
        manifest: canonical_manifest_bytes(&manifest)?,
        artifacts,
    })
}

fn check_expected(manifest_dir: &Path, expected: &ExpectedArtifacts) -> Result<(), String> {
    let manifest_path = manifest_dir.join("generated/cli-bytecode-manifest.json");
    compare_file(&manifest_path, &expected.manifest)?;
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

    #[test]
    fn options_make_artifact_version_an_explicit_typed_input() {
        let options = parse_options(&[
            "--check".to_string(),
            "--workspace-root".to_string(),
            "/tmp/harn".to_string(),
            "--artifact-version".to_string(),
            "1.2.4".to_string(),
        ])
        .expect("options");
        assert_eq!(
            options,
            CliOptions {
                workspace_root: PathBuf::from("/tmp/harn"),
                check: true,
                artifact_version: Some("1.2.4".to_string()),
            }
        );
    }

    #[test]
    fn explicit_artifact_version_can_differ_from_frozen_generator_runtime() {
        let workspace = test_workspace("1.2.4", "source");
        let contract = checkout_contract(workspace.path()).expect("contract");
        validate_artifact_version(&contract, Some("1.2.4")).expect("explicit release target");

        let error = validate_artifact_version(&contract, None)
            .expect_err("implicit cross-version generation must fail");
        assert!(error.contains("must pass --artifact-version 1.2.4 explicitly"));

        let error = validate_artifact_version(&contract, Some("1.2.5"))
            .expect_err("wrong explicit target must fail");
        assert!(error.contains("requested=1.2.5, workspace=1.2.4"));
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
            workspace.path().join("crates/harn-kernel/src/lib.rs"),
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
        fs::create_dir_all(workspace.path().join("crates/harn-kernel/src"))
            .expect("create harn-kernel");
        let mut manifest = fs::File::create(workspace.path().join("Cargo.toml"))
            .expect("create workspace manifest");
        writeln!(manifest, "[workspace.package]\nversion = {version:?}")
            .expect("write workspace manifest");
        fs::write(
            workspace.path().join("crates/harn-kernel/src/lib.rs"),
            compiler_source,
        )
        .expect("write compiler input");
        workspace
    }
}
