//! Host-owned source-module verifier material, captured before task mutation.
//! Ordinary module loading is not reinterpreted as this isolated contract.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod python_modules;
pub use python_modules::IsolatedPythonModuleVerifier;

/// Host-owned loader configuration. Each backend captures native material;
/// serialized configuration and receipts never carry reusable authority.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "loader", rename_all = "snake_case")]
pub enum VerifierLoaderSpec {
    PythonSource(IsolatedPythonSourceVerifier),
    PythonModule(IsolatedPythonModuleVerifier),
}

impl From<IsolatedPythonSourceVerifier> for VerifierLoaderSpec {
    fn from(value: IsolatedPythonSourceVerifier) -> Self {
        Self::PythonSource(value)
    }
}

impl VerifierLoaderSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::PythonSource(value) => &value.id,
            Self::PythonModule(value) => &value.id,
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct CaptureLimits {
    interpreter_bytes: usize,
    source_bytes: usize,
    module_count: usize,
    bundle_bytes: usize,
}

#[cfg(target_os = "linux")]
fn limits() -> &'static CaptureLimits {
    static LIMITS: std::sync::LazyLock<CaptureLimits> = std::sync::LazyLock::new(|| {
        toml::from_str(include_str!("verifier_provenance/policy.toml"))
            .expect("embedded verifier capture policy")
    });
    &LIMITS
}

/// A filesystem observation is distinct from proof of what a child executed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum VerifierLoaderObservation {
    Unchanged,
    Changed { paths: Vec<PathBuf> },
    Absent { paths: Vec<PathBuf> },
    Unmeasured { reason: String },
    Refused { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct MaterialIdentity {
    sha256: String,
    bytes: u64,
}

impl MaterialIdentity {
    fn of(bytes: &[u8]) -> Self {
        Self {
            sha256: digest(bytes),
            bytes: bytes.len() as u64,
        }
    }
}

/// Minted only by the completion owner after this child's private witness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifierExecutionReceipt {
    pub verifier_id: String,
    pub baseline_sha256: String,
    pub executed_material: VerifierLoaderObservation,
}

/// One capture per exact host declaration, including failed measurements.
/// Goal refresh cannot replace an absent baseline with newly authored material.
#[derive(Default)]
pub(crate) struct VerifierBaselineRegistry(
    std::sync::Mutex<std::collections::BTreeMap<VerifierLoaderSpec, CapturedBaseline>>,
);

type CapturedBaseline = Result<std::sync::Arc<PreparedVerifier>, VerifierLoaderObservation>;

impl VerifierBaselineRegistry {
    pub(crate) fn capture(
        &self,
        spec: VerifierLoaderSpec,
    ) -> Result<std::sync::Arc<PreparedVerifier>, VerifierLoaderObservation> {
        self.0
            .lock()
            .map_err(|_| VerifierLoaderObservation::Refused {
                reason: "verifier baseline registry is poisoned".into(),
            })?
            .entry(spec.clone())
            .or_insert_with(|| PreparedVerifier::capture(spec).map(std::sync::Arc::new))
            .clone()
    }
}

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
    spec: VerifierLoaderSpec,
    #[cfg(target_os = "linux")]
    request: IsolatedPythonSourceVerifier,
    #[cfg(target_os = "linux")]
    modules: Option<python_modules::CapturedModules>,
    baseline: std::collections::BTreeMap<PathBuf, Option<MaterialIdentity>>,
    baseline_sha256: String,
    #[cfg(target_os = "linux")]
    interpreter: std::fs::File,
    #[cfg(target_os = "linux")]
    source: std::fs::File,
}

impl PreparedVerifier {
    pub fn spec(&self) -> &VerifierLoaderSpec {
        &self.spec
    }

    pub fn baseline_sha256(&self) -> &str {
        &self.baseline_sha256
    }

    /// Observation only. Execution uses the immutable baseline even when the
    /// workspace changes; a read-back is never a later spawn's attestation.
    pub fn observe_workspace(&self) -> VerifierLoaderObservation {
        let mut changed = Vec::new();
        let mut absent = Vec::new();
        for (path, expected) in &self.baseline {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
            }
            let current = match options.open(path) {
                Ok(file) => {
                    use std::io::Read;
                    match file.metadata() {
                        Ok(metadata) if metadata.is_file() => {}
                        Ok(_) => {
                            return VerifierLoaderObservation::Changed {
                                paths: vec![path.clone()],
                            }
                        }
                        Err(error) => {
                            return VerifierLoaderObservation::Unmeasured {
                                reason: error.to_string(),
                            }
                        }
                    }
                    let mut bytes = Vec::new();
                    let maximum = expected.as_ref().map_or(0, |identity| identity.bytes);
                    if let Err(error) = file.take(maximum + 1).read_to_end(&mut bytes) {
                        return VerifierLoaderObservation::Unmeasured {
                            reason: error.to_string(),
                        };
                    }
                    Some(MaterialIdentity::of(&bytes))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return VerifierLoaderObservation::Unmeasured {
                        reason: error.to_string(),
                    }
                }
            };
            if current != *expected {
                if current.is_none() {
                    absent.push(path.clone());
                } else {
                    changed.push(path.clone());
                }
            }
        }
        if !changed.is_empty() {
            changed.extend(absent);
            changed.sort();
            VerifierLoaderObservation::Changed { paths: changed }
        } else if !absent.is_empty() {
            VerifierLoaderObservation::Absent { paths: absent }
        } else {
            VerifierLoaderObservation::Unchanged
        }
    }

    pub fn matches(&self, program: &str, args: &[String], cwd: &Path) -> bool {
        match &self.spec {
            VerifierLoaderSpec::PythonSource(request) => request.matches(program, args, cwd),
            VerifierLoaderSpec::PythonModule(request) => request.matches(program, args, cwd),
        }
    }

    pub fn capture(spec: VerifierLoaderSpec) -> Result<Self, VerifierLoaderObservation> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Err(VerifierLoaderObservation::Unmeasured {
                reason: "sealed verifier execution is unsupported on this platform".into(),
            })
        }
        #[cfg(target_os = "linux")]
        {
            let modules = match &spec {
                VerifierLoaderSpec::PythonSource(_) => None,
                VerifierLoaderSpec::PythonModule(request) => Some(request.capture()?),
            };
            let paths = match &spec {
                VerifierLoaderSpec::PythonSource(request) => {
                    vec![request.interpreter.clone(), request.source.clone()]
                }
                VerifierLoaderSpec::PythonModule(request) => vec![request.interpreter.clone()],
            };
            for path in paths {
                match path.metadata() {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(VerifierLoaderObservation::Absent { paths: vec![path] })
                    }
                    Err(error) => {
                        return Err(VerifierLoaderObservation::Unmeasured {
                            reason: error.to_string(),
                        })
                    }
                    Ok(_) => {}
                }
            }
            Self::capture_material(spec, modules)
                .map_err(|reason| VerifierLoaderObservation::Unmeasured { reason })
        }
    }

    #[cfg(target_os = "linux")]
    fn capture_material(
        spec: VerifierLoaderSpec,
        modules: Option<python_modules::CapturedModules>,
    ) -> Result<Self, String> {
        let request = match &spec {
            VerifierLoaderSpec::PythonSource(request) => request.clone(),
            VerifierLoaderSpec::PythonModule(request) => modules
                .as_ref()
                .expect("module capture precedes native preparation")
                .source_request(request),
        };
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
            let interpreter = bounded_read(&request.interpreter, limits().interpreter_bytes)?;
            if !interpreter.starts_with(b"\x7fELF") {
                return Err(
                    "isolated source verifier interpreter is not a supported ELF executable"
                        .to_string(),
                );
            }
            let source = match &modules {
                Some(modules) => serde_json::to_vec(modules).map_err(|error| error.to_string())?,
                None => bounded_read(&request.source, limits().source_bytes)?,
            };
            if source.is_empty() {
                return Err("Python verifier entrypoint is empty".into());
            }
            if source.len() > limits().bundle_bytes {
                return Err("Python verifier bundle exceeds capture limits".into());
            }
            let mut baseline = std::collections::BTreeMap::from([(
                request.interpreter.clone(),
                Some(MaterialIdentity::of(&interpreter)),
            )]);
            if let Some(modules) = &modules {
                modules.extend_baseline(&mut baseline);
            } else {
                baseline.insert(request.source.clone(), Some(MaterialIdentity::of(&source)));
            }
            let baseline_sha256 = digest(
                &serde_json::to_vec(&(&spec, &baseline)).map_err(|error| error.to_string())?,
            );
            let seal = crate::process_sandbox::sealed_launch_bytes;
            Ok(Self {
                spec,
                request,
                modules,
                baseline,
                baseline_sha256,
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
        self.launch_avoiding(&[])
    }

    /// Allocate inherited capabilities outside the guardian's startup roles.
    #[cfg(target_os = "linux")]
    pub fn launch_avoiding(
        &self,
        reserved: &[std::os::fd::RawFd],
    ) -> Result<PinnedVerifierLaunch, String> {
        use std::os::fd::AsRawFd;

        let interpreter = clone_avoiding(&self.interpreter, reserved)?;
        let source = clone_avoiding(&self.source, reserved)?;
        let mut witness = VerifierExecutionWitness::new().map_err(|error| error.to_string())?;
        witness.receipt = Some(VerifierExecutionReceipt {
            verifier_id: self.request.id.clone(),
            baseline_sha256: self.baseline_sha256.clone(),
            executed_material: VerifierLoaderObservation::Unchanged,
        });
        let witness_descriptor = clone_avoiding(&witness.file, reserved)?;
        let metadata = serde_json::json!({
            "origin": self.request.source,
            "args": self.request.args,
            "witness_fd": witness_descriptor.as_raw_fd(),
            "witness": witness.expected,
            "module": self.modules.as_ref().map(|modules| &modules.entry),
        });
        Ok(PinnedVerifierLaunch {
            program: format!("/proc/self/fd/{}", interpreter.as_raw_fd()),
            args: vec![
                "-I".to_string(),
                "-S".to_string(),
                "-c".to_string(),
                include_str!("verifier_provenance/python_bootstrap.py").to_string(),
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

#[cfg(target_os = "linux")]
fn clone_avoiding(
    file: &std::fs::File,
    reserved: &[std::os::fd::RawFd],
) -> Result<std::fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let minimum = reserved
        .iter()
        .copied()
        .max()
        .unwrap_or(2)
        .checked_add(1)
        .ok_or_else(|| "invalid reserved startup descriptor".to_string())?;
    // SAFETY: fcntl duplicates a live borrowed descriptor into a fresh owned one.
    let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: a successful F_DUPFD_CLOEXEC returns a newly owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

/// Proof that the pinned bootstrap reached source execution in the admitted
/// child. Capture never executes the candidate. Completion owners check this
/// bounded private capability after their existing confined wait.
/// The interpreter is host-admitted material; this is execution evidence, not
/// an attestation against a malicious interpreter that fabricates the protocol.
pub struct VerifierExecutionWitness {
    receipt: Option<VerifierExecutionReceipt>,
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
            receipt: None,
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

    pub fn receipt(&self) -> std::io::Result<VerifierExecutionReceipt> {
        self.validate()?;
        self.receipt
            .clone()
            .ok_or_else(|| std::io::Error::other("verifier completion has no prepared baseline"))
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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

#[cfg(target_os = "linux")]
impl PinnedVerifierLaunch {
    pub fn bind_to_command(
        self,
        command: &mut std::process::Command,
        closed: bool,
    ) -> Result<VerifierExecutionWitness, String> {
        if command.get_program() != std::ffi::OsStr::new(&self.program) {
            return Err("prepared verifier executable wrapping is unmeasured".into());
        }
        validate_pinned_environment(command, closed)?;
        self.descriptors.attach(command);
        Ok(self.witness)
    }
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
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| error.to_string())?;
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
    if bytes.len() > maximum {
        return Err("isolated source verifier material exceeds the capture ceiling".to_string());
    }
    Ok(bytes)
}
