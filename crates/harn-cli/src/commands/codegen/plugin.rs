use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use harn_hostlib::process::{
    self, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome,
};
use harn_vm::codegen::{
    CodegenDiagnostic, CodegenDiagnosticCode as Code, CodegenRequest, CodegenResponse,
};
use harn_vm::orchestration::{
    CapabilityPolicy, ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile,
};

use super::artifacts::GeneratedArtifactSet;

const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;
const STDERR_LIMIT: u64 = 64 * 1024;
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(30);

struct PolicyScope;

impl Drop for PolicyScope {
    fn drop(&mut self) {
        harn_vm::orchestration::pop_execution_policy();
    }
}

fn source_denials(documents: &harn_modules::SourceDocuments) -> Vec<String> {
    // The process sandbox adds the credential registry itself. These denials
    // additionally exclude original source even inside a backend read root.
    let mut denied = Vec::new();
    for path in documents.physical_paths() {
        denied.push(path.display().to_string());
    }
    denied.sort();
    denied.dedup();
    denied
}

fn collect(
    mut reader: Box<dyn Read + Send>,
    limit: u64,
    exceeded: Arc<AtomicBool>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    harn_parser::runtime_stack::spawn(move || {
        let mut bytes = Vec::new();
        reader.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            exceeded.store(true, Ordering::Release);
        }
        Ok(bytes)
    })
}

pub(super) fn invoke(
    executable: &Path,
    read_roots: &[PathBuf],
    request: &CodegenRequest,
    documents: &harn_modules::SourceDocuments,
) -> Result<GeneratedArtifactSet, CodegenDiagnostic> {
    let fail = |code| CodegenDiagnostic::new(code);
    let program = which::which(executable)
        .map_err(|_| fail(Code::PluginSpawnFailed))?
        .canonicalize()
        .map_err(|_| fail(Code::PluginSpawnFailed))?;
    let private = tempfile::tempdir().map_err(|_| fail(Code::PluginSpawnFailed))?;
    let private_root = private
        .path()
        .canonicalize()
        .map_err(|_| fail(Code::PluginSpawnFailed))?;
    let mut roots = vec![program
        .parent()
        .ok_or_else(|| fail(Code::PluginSpawnFailed))?
        .to_path_buf()];
    for root in read_roots {
        roots.push(
            root.canonicalize()
                .map_err(|_| fail(Code::PluginSpawnFailed))?,
        );
    }
    let policy = CapabilityPolicy {
        workspace_roots: vec![private_root.display().to_string()],
        read_only_roots: roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        side_effect_level: Some("process_exec".into()),
        sandbox_profile: SandboxProfile::OsHardened,
        process_sandbox: Box::new(ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            read_deny_roots: source_denials(documents),
            deny_network: true,
            allow_child_workspace_write: true,
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    };
    harn_vm::orchestration::push_execution_policy(policy);
    let _policy = PolicyScope;
    let mut environment = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        environment.insert("PATH".into(), path);
    }
    environment.insert("TMPDIR".into(), private_root.display().to_string());
    environment.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    let program = program
        .to_str()
        .ok_or_else(|| fail(Code::PluginSpawnFailed))?
        .to_owned();
    let mut child = harn_vm::stdlib::sandbox::with_declared_path_roots(|| {
        process::spawn_process(SpawnSpec {
            builtin: "codegen_plugin",
            program,
            args: Vec::new(),
            cwd: Some(private_root),
            env: environment,
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: true,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        })
    })
    .map_err(|_| fail(Code::PluginSpawnFailed))?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = collect(
        child
            .take_stdout()
            .ok_or_else(|| fail(Code::PluginFailed))?,
        OUTPUT_LIMIT,
        exceeded.clone(),
    );
    let stderr = collect(
        child
            .take_stderr()
            .ok_or_else(|| fail(Code::PluginFailed))?,
        STDERR_LIMIT,
        exceeded.clone(),
    );
    let mut stdin = child.take_stdin().ok_or_else(|| fail(Code::PluginFailed))?;
    let mut input = harn_vm::canonical_json::to_vec(
        &serde_json::to_value(request).map_err(|_| fail(Code::InvalidIr))?,
    );
    input.push(b'\n');
    let writer = harn_parser::runtime_stack::spawn(move || stdin.write_all(&input));
    let outcome =
        child.wait_with_timeout(Some(PLUGIN_TIMEOUT), &|| exceeded.load(Ordering::Acquire));
    let input_result = writer.join();
    let output = stdout.join();
    let errors = stderr.join();
    if exceeded.load(Ordering::Acquire) {
        return Err(fail(Code::PluginOutputLimit));
    }
    match outcome.map_err(|_| fail(Code::PluginFailed))? {
        WaitOutcome::Exited(status) if status.code == Some(0) && status.signal.is_none() => {}
        WaitOutcome::TimedOut(_) => return Err(fail(Code::PluginTimedOut)),
        _ => return Err(fail(Code::PluginFailed)),
    }
    input_result
        .map_err(|_| fail(Code::PluginFailed))?
        .map_err(|_| fail(Code::PluginFailed))?;
    errors
        .map_err(|_| fail(Code::PluginFailed))?
        .map_err(|_| fail(Code::PluginFailed))?;
    let output = output
        .map_err(|_| fail(Code::PluginFailed))?
        .map_err(|_| fail(Code::PluginFailed))?;
    let response: CodegenResponse =
        serde_json::from_slice(&output).map_err(|_| fail(Code::MalformedPluginResponse))?;
    let artifacts = harn_vm::codegen::validate_response(request, response)?;
    GeneratedArtifactSet::prepare(artifacts)
}
