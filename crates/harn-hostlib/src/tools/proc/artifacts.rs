use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use sha2::{Digest, Sha256};

use crate::error::HostlibError;

static ARTIFACTS: LazyLock<Mutex<BTreeMap<String, CommandArtifacts>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[derive(Clone, Debug)]
pub(crate) struct CommandArtifacts {
    pub(crate) output_path: PathBuf,
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
    pub(crate) line_count: u64,
    pub(crate) byte_count: u64,
    pub(crate) output_sha256: String,
}

pub(crate) fn persist_artifacts(
    command_id: &str,
    stdout: &[u8],
    stderr: &[u8],
    handle_id: Option<&str>,
) -> Result<CommandArtifacts, HostlibError> {
    let artifacts = planned_artifact_paths(command_id);
    std::fs::create_dir_all(artifacts.output_path.parent().unwrap()).map_err(|e| {
        HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create command artifact dir: {e}"),
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            artifacts.output_path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        );
    }
    std::fs::write(&artifacts.stdout_path, stdout).map_err(|e| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to write stdout artifact: {e}"),
    })?;
    std::fs::write(&artifacts.stderr_path, stderr).map_err(|e| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to write stderr artifact: {e}"),
    })?;
    let mut combined = Vec::with_capacity(stdout.len() + stderr.len());
    combined.extend_from_slice(stdout);
    combined.extend_from_slice(stderr);
    std::fs::write(&artifacts.output_path, &combined).map_err(|e| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to write combined output artifact: {e}"),
    })?;
    let output_sha256 = format!("sha256:{}", hex::encode(Sha256::digest(&combined)));
    let artifacts = CommandArtifacts {
        output_path: artifacts.output_path,
        stdout_path: artifacts.stdout_path,
        stderr_path: artifacts.stderr_path,
        line_count: line_count(&combined),
        byte_count: combined.len() as u64,
        output_sha256,
    };
    register_artifacts(command_id, handle_id, &artifacts);
    Ok(artifacts)
}

pub(crate) fn planned_artifact_paths(command_id: &str) -> CommandArtifacts {
    let dir = std::env::temp_dir().join(format!("harn-command-{command_id}"));
    CommandArtifacts {
        output_path: dir.join("combined.txt"),
        stdout_path: dir.join("stdout.txt"),
        stderr_path: dir.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    }
}

pub(crate) fn resolve_output_path(
    command_id: Option<&str>,
    handle_id: Option<&str>,
    path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = path {
        return Some(PathBuf::from(path));
    }
    let artifacts = ARTIFACTS.lock().expect("command artifact store poisoned");
    command_id
        .and_then(|id| artifacts.get(id))
        .or_else(|| handle_id.and_then(|id| artifacts.get(id)))
        .map(|a| a.output_path.clone())
}

fn register_artifacts(command_id: &str, handle_id: Option<&str>, artifacts: &CommandArtifacts) {
    let mut store = ARTIFACTS.lock().expect("command artifact store poisoned");
    store.insert(command_id.to_string(), artifacts.clone());
    if let Some(handle_id) = handle_id {
        store.insert(handle_id.to_string(), artifacts.clone());
    }
}

fn line_count(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    if bytes.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}
