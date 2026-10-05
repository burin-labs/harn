//! Host-owned source-module verifier material, captured before task mutation.
//! Ordinary module loading is not reinterpreted as this isolated contract.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The exact isolated source invocation the host admits before execution.
/// This contains no baseline digest or model-supplied executable bytes.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedPythonSourceVerifier {
    pub id: String,
    pub interpreter: PathBuf,
    pub source: PathBuf,
    pub workspace: PathBuf,
    pub args: Vec<String>,
}

impl IsolatedPythonSourceVerifier {
    pub fn invocation_args(&self) -> Vec<String> {
        let mut args = vec![
            "-I".to_string(),
            "-S".to_string(),
            self.source.display().to_string(),
        ];
        args.extend(self.args.clone());
        args
    }

    pub fn matches(&self, program: &str, args: &[String], cwd: &Path) -> bool {
        Path::new(program) == self.interpreter
            && args == self.invocation_args()
            && cwd == self.workspace
    }
}

/// Native material cannot be minted by a serialized lease or agent dictionary.
#[derive(Debug)]
pub struct PreparedVerifier {
    request: IsolatedPythonSourceVerifier,
    #[cfg(target_os = "linux")]
    interpreter: std::fs::File,
    #[cfg(target_os = "linux")]
    source: std::fs::File,
}

impl PreparedVerifier {
    pub fn request(&self) -> &IsolatedPythonSourceVerifier {
        &self.request
    }

    pub(crate) fn capture(request: IsolatedPythonSourceVerifier) -> Result<Self, String> {
        if request.id.trim().is_empty()
            || !request.interpreter.is_absolute()
            || !request.source.is_absolute()
            || !request.workspace.is_absolute()
            || request.source.extension().and_then(|value| value.to_str()) != Some("py")
        {
            return Err("isolated source verifier requires a named absolute interpreter, Python source, and workspace".to_string());
        }
        for path in [&request.interpreter, &request.source, &request.workspace] {
            if path.canonicalize().map_err(|error| error.to_string())? != *path {
                return Err("isolated source verifier paths must already be canonical".to_string());
            }
        }
        if !request.workspace.is_dir() {
            return Err("isolated source verifier workspace is not a directory".to_string());
        }
        #[cfg(target_os = "linux")]
        {
            let interpreter = bounded_read(&request.interpreter, 64 * 1024 * 1024)?;
            if !interpreter.starts_with(b"\x7fELF") {
                return Err(
                    "isolated source verifier interpreter is not a supported ELF executable"
                        .to_string(),
                );
            }
            let source = bounded_read(&request.source, 1024 * 1024)?;
            let seal = crate::process_sandbox::sealed_launch_bytes;
            Ok(Self {
                request,
                interpreter: seal(&interpreter)
                    .map_err(|error| error.to_string())?
                    .into(),
                source: seal(&source).map_err(|error| error.to_string())?.into(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        Err("isolated source verifier pinned execution is unmeasured on this platform".to_string())
    }

    #[cfg(target_os = "linux")]
    pub fn launch(&self) -> Result<PinnedVerifierLaunch, String> {
        use std::os::fd::AsRawFd;

        let interpreter = self
            .interpreter
            .try_clone()
            .map_err(|error| error.to_string())?;
        let source = self.source.try_clone().map_err(|error| error.to_string())?;
        let witness = VerifierExecutionWitness::new().map_err(|error| error.to_string())?;
        let witness_descriptor = witness
            .file
            .try_clone()
            .map_err(|error| error.to_string())?;
        let metadata = serde_json::json!({
            "origin": self.request.source,
            "args": self.request.args,
            "witness_fd": witness_descriptor.as_raw_fd(),
            "witness": witness.expected,
        });
        Ok(PinnedVerifierLaunch {
            program: format!("/proc/self/fd/{}", interpreter.as_raw_fd()),
            args: vec![
                "-I".to_string(),
                "-S".to_string(),
                "-c".to_string(),
                PYTHON_SOURCE_BOOTSTRAP.to_string(),
                source.as_raw_fd().to_string(),
                metadata.to_string(),
            ],
            descriptors: crate::process_sandbox::DescriptorTransfer::new(vec![
                interpreter.into(),
                source.into(),
                witness_descriptor.into(),
            ]),
            witness,
        })
    }
}

/// Proof that the pinned bootstrap reached source execution in the admitted
/// child. Capture never executes the candidate. Completion owners check this
/// bounded private capability after their existing confined wait.
/// The interpreter is host-admitted material; this is execution evidence, not
/// an attestation against a malicious interpreter that fabricates the protocol.
pub struct VerifierExecutionWitness {
    #[cfg(target_os = "linux")]
    file: std::fs::File,
    #[cfg(target_os = "linux")]
    expected: [u8; 16],
}

impl VerifierExecutionWitness {
    #[cfg(target_os = "linux")]
    fn new() -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: static NUL-terminated name and supported memfd flags.
        let fd = unsafe {
            libc::memfd_create(
                c"harn-verifier-witness".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful memfd_create returned a newly owned descriptor.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(16)?;
        // Only these sixteen bytes are writable; neither child nor descendants
        // can grow this capability into an unbounded output channel.
        // SAFETY: the owned descriptor remains open throughout this call.
        if unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            file,
            expected: *uuid::Uuid::new_v4().as_bytes(),
        })
    }

    pub fn validate(&self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::FileExt;
            let mut actual = [0; 16];
            self.file.read_exact_at(&mut actual, 0)?;
            if actual == self.expected {
                return Ok(());
            }
        }
        Err(std::io::Error::other("isolated source verifier execution is unmeasured: pinned Python bootstrap did not witness source execution"))
    }
}

/// The caller carries these descriptors through its existing confinement and
/// owner-death launch, then validates the effective environment at that seam.
#[cfg(target_os = "linux")]
pub struct PinnedVerifierLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub descriptors: crate::process_sandbox::DescriptorTransfer,
    pub witness: VerifierExecutionWitness,
}

/// Use the existing environment policy at the stricter executable boundary.
/// Payload permission does not authorize a mutable loader around sealed bytes.
pub fn validate_pinned_environment(
    command: &std::process::Command,
    closed: bool,
) -> Result<(), String> {
    crate::security::validate_process_environment(
        crate::security::ProcessEnvironmentBoundary::TrustedSetup,
        (!closed).then(std::env::vars_os).into_iter().flatten(),
        command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned))),
    )
    .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn bounded_read(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("isolated source verifier material is not a regular file".to_string());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(
            "isolated source verifier material is empty or exceeds the capture ceiling".to_string(),
        );
    }
    Ok(bytes)
}

// Source reads are positional: clones share an open-file description, so a
// sequential read would consume the baseline for later turns or parallel runs.
#[cfg(target_os = "linux")]
const PYTHON_SOURCE_BOOTSTRAP: &str = r#"import json, os, sys
descriptor = int(sys.argv[1])
metadata = json.loads(sys.argv[2])
source = os.pread(descriptor, os.fstat(descriptor).st_size, 0)
os.close(descriptor)
sys.argv = [metadata['origin'], *metadata['args']]
namespace = {'__name__': '__main__', '__file__': metadata['origin'], '__package__': None, '__spec__': None, '__cached__': None}
witness_fd = metadata['witness_fd']
witness = bytes(metadata['witness'])
write_witness = os.pwrite
close_witness = os.close
compiled = compile(source, metadata['origin'], 'exec')
try:
    exec(compiled, namespace)
finally:
    write_witness(witness_fd, witness, 0)
    close_witness(witness_fd)
"#;
