//! Native host adapter for the Portable Harn Kernel.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::cli::{
    PortableCommand, PortableCompileArgs, PortableEntryKindArg, PortablePackageArgs,
    PortableResumeArgs, PortableStartArgs,
};
use crate::commands::portable_source::PortableSourceInput;
use crate::json_envelope::JsonEnvelope;
use harn_kernel::{
    ArtifactLimits, CapabilityRequest, CapabilityResult, DataValue, Diagnostic, EntryKind,
    Execution, GrantSet, ProgramArtifact, PORTABLE_MAX_GRANTS_JSON_BYTES,
    PORTABLE_MAX_SNAPSHOT_BYTES, PORTABLE_MAX_VALUE_JSON_BYTES,
};

pub(crate) const PORTABLE_CLI_SCHEMA_VERSION: u32 = 1;

pub(crate) fn run(command: PortableCommand) -> Result<(), String> {
    match command {
        PortableCommand::Compile(args) => compile(args),
        PortableCommand::Package(args) => package(args),
        PortableCommand::Start(args) => start(args),
        PortableCommand::Resume(args) => resume(args),
    }
}

fn package(args: PortablePackageArgs) -> Result<(), String> {
    let source = PortableSourceInput::load(&args.source)?;
    let mut expected = serde_json::to_vec_pretty(&source.source_package())
        .map_err(|error| format!("serialize portable source package: {error}"))?;
    expected.push(b'\n');
    if args.check {
        let actual = fs::read(&args.output)
            .map_err(|error| format!("failed to read {}: {error}", args.output.display()))?;
        if actual != expected {
            return Err(format!(
                "portable source package is stale: regenerate {}",
                args.output.display()
            ));
        }
    } else {
        write(&args.output, &expected)?;
    }
    print_json(&PackageOutput {
        status: if args.check { "checked" } else { "packaged" },
        package_path: &args.output,
        package_bytes: expected.len(),
    })
}

fn compile(args: PortableCompileArgs) -> Result<(), String> {
    let source = PortableSourceInput::load(&args.source)?;
    let program = source.compile(&args.entry, entry_kind(args.entry_kind))?;
    write(&args.output, program.bytes())?;
    print_json(&CompileOutput {
        status: "compiled",
        artifact_path: &args.output,
        artifact_digest: program.digest_hex(),
        artifact_bytes: program.bytes().len(),
    })
}

fn start(args: PortableStartArgs) -> Result<(), String> {
    let program = read_program(&args.artifact)?;
    let input = read_data_value(&args.input, "input")?;
    let grants = read_grants(args.grants.as_deref())?;
    emit_execution(
        harn_kernel::start(&program, input, &grants),
        &args.snapshot_out,
    )
}

fn resume(args: PortableResumeArgs) -> Result<(), String> {
    let program = read_program(&args.artifact)?;
    let snapshot = read_bounded(&args.snapshot, PORTABLE_MAX_SNAPSHOT_BYTES, "snapshot")?;
    let result_bytes = read_bounded(
        &args.result,
        PORTABLE_MAX_VALUE_JSON_BYTES,
        "capability result",
    )?;
    let result = serde_json::from_slice::<CapabilityResult>(&result_bytes)
        .map_err(|error| format!("invalid capability result JSON: {error}"))?;
    let grants = read_grants(Some(&args.grants))?;
    emit_execution(
        harn_kernel::resume(&program, &snapshot, result, &grants),
        &args.snapshot_out,
    )
}

fn emit_execution(execution: Execution, snapshot_out: &Path) -> Result<(), String> {
    match execution {
        Execution::Completed { value } => print_json(&ExecutionOutput::Completed { value }),
        Execution::Suspended { request, snapshot } => {
            write(snapshot_out, &snapshot)?;
            print_json(&ExecutionOutput::Suspended {
                request,
                snapshot_path: snapshot_out,
            })
        }
        Execution::Failed { diagnostic } => print_json(&ExecutionOutput::Failed { diagnostic }),
    }
}

fn read_program(path: &Path) -> Result<ProgramArtifact, String> {
    let limits = ArtifactLimits::default();
    let bytes = read_bounded(path, limits.max_bytes, "artifact")?;
    ProgramArtifact::decode(&bytes, limits)
        .map_err(|error| format!("{}: {}", error.code, error.message))
}

fn read_data_value(path: &Path, kind: &str) -> Result<DataValue, String> {
    let bytes = read_bounded(path, PORTABLE_MAX_VALUE_JSON_BYTES, kind)?;
    let value =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid {kind} JSON: {error}"))?;
    DataValue::from_json(value).map_err(|error| format!("{}: {}", error.code, error.message))
}

fn read_grants(path: Option<&Path>) -> Result<GrantSet, String> {
    let Some(path) = path else {
        return Ok(GrantSet::pure());
    };
    let bytes = read_bounded(path, PORTABLE_MAX_GRANTS_JSON_BYTES, "grants")?;
    let json =
        std::str::from_utf8(&bytes).map_err(|error| format!("grants are not UTF-8: {error}"))?;
    GrantSet::from_host_json(json).map_err(|error| format!("{}: {}", error.code, error.message))
}

fn read_bounded(path: &Path, max_bytes: usize, kind: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {kind} {}: {error}", path.display()))?;
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{kind} {} has {} bytes; limit is {max_bytes}",
            path.display(),
            metadata.len()
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read {kind} {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{kind} {} grew beyond the {max_bytes}-byte limit while being read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    harn_vm::atomic_io::atomic_write(path, bytes)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(&JsonEnvelope::ok(PORTABLE_CLI_SCHEMA_VERSION, value))
            .map_err(|error| format!("serialize output: {error}"))?
    );
    Ok(())
}

fn entry_kind(value: PortableEntryKindArg) -> EntryKind {
    match value {
        PortableEntryKindArg::Function => EntryKind::Function,
        PortableEntryKindArg::Pipeline => EntryKind::Pipeline,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompileOutput<'a> {
    status: &'static str,
    artifact_path: &'a Path,
    artifact_digest: String,
    artifact_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageOutput<'a> {
    status: &'static str,
    package_path: &'a Path,
    package_bytes: usize,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ExecutionOutput<'a> {
    Completed {
        value: DataValue,
    },
    Suspended {
        request: CapabilityRequest,
        #[serde(rename = "snapshotPath")]
        snapshot_path: &'a Path,
    },
    Failed {
        diagnostic: Diagnostic,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_command_projects_the_canonical_module_graph() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root.harn");
        let dependency = dir.path().join("math.harn");
        let output = dir.path().join("package.json");
        fs::write(
            &root,
            "import { increment } from \"math\"\npub fn reduce(value: int) -> int { return increment(value) }",
        )
        .unwrap();
        fs::write(
            dependency,
            "pub fn increment(value: int) -> int { return value + 1 }",
        )
        .unwrap();

        package(PortablePackageArgs {
            source: root,
            output: output.clone(),
            check: false,
        })
        .unwrap();
        let projected: harn_kernel::PortableSourcePackage =
            serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(projected.modules.len(), 1);
        assert_eq!(projected.root_imports[0].path, "math");
        assert_eq!(projected.root_imports[0].target, "module/0");
        assert_eq!(
            projected.modules[0].exports.get("increment"),
            Some(&harn_kernel::PortableExportKind::Function)
        );

        package(PortablePackageArgs {
            source: dir.path().join("root.harn"),
            output,
            check: true,
        })
        .unwrap();
    }

    #[test]
    fn native_host_compiles_starts_and_resumes_the_kernel_contract() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("app.harn");
        let artifact = dir.path().join("app.hbc");
        let input = dir.path().join("input.json");
        let grants = dir.path().join("grants.json");
        let snapshot = dir.path().join("snapshot.bin");
        let result = dir.path().join("result.json");
        fs::write(
            &source,
            "fn greet(harness: Harness, input: string) { return harness.interaction.ask(input) }",
        )
        .unwrap();
        fs::write(&input, r#""continue""#).unwrap();
        fs::write(
            &grants,
            r#"{"capabilities":["interaction.ask"],"snapshotKey":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7]}"#,
        )
        .unwrap();

        compile(PortableCompileArgs {
            source,
            entry: "greet".to_string(),
            entry_kind: PortableEntryKindArg::Function,
            output: artifact.clone(),
        })
        .unwrap();
        let program = read_program(&artifact).unwrap();
        let host_grants = read_grants(Some(&grants)).unwrap();
        let Execution::Suspended {
            request,
            snapshot: expected,
        } = harn_kernel::start(
            &program,
            DataValue::String("continue".to_string()),
            &host_grants,
        )
        else {
            panic!("capability did not suspend")
        };

        start(PortableStartArgs {
            artifact: artifact.clone(),
            input,
            grants: Some(grants.clone()),
            snapshot_out: snapshot.clone(),
        })
        .unwrap();
        assert_eq!(fs::read(&snapshot).unwrap(), expected);
        fs::write(
            &result,
            serde_json::to_vec(&CapabilityResult::Ok {
                request_id: request.id,
                value: DataValue::String("approved".to_string()),
            })
            .unwrap(),
        )
        .unwrap();
        resume(PortableResumeArgs {
            artifact,
            snapshot,
            result,
            grants,
            snapshot_out: dir.path().join("next.bin"),
        })
        .unwrap();
    }
}
