//! Harn Flow VCS backend abstraction.
//!
//! The Phase 0 backend is [`ShadowGitBackend`]: it stores every emitted atom as
//! a git commit on a sidecar ref (`refs/flow/atoms/<atom-id>`) without touching
//! the user worktree. Later phases can replace the storage substrate by
//! implementing [`VcsBackend`] directly.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::slice::SliceId;
use super::{Atom, AtomError, AtomId};

const ATOM_REF_PREFIX: &str = "refs/flow/atoms";
const SLICE_REF_PREFIX: &str = "refs/flow/slices";

/// Errors produced by Flow VCS backends.
#[derive(Debug)]
pub enum VcsBackendError {
    /// Backend configuration or caller input is invalid.
    Invalid(String),
    /// A requested atom, slice, commit, or ref is missing.
    NotFound(String),
    /// The backend intentionally does not implement this operation yet.
    Unsupported(String),
    /// Atom encoding, decoding, or validation failed.
    Atom(AtomError),
    /// JSON encoding or decoding failed.
    Json(String),
    /// A git command failed.
    Git {
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    /// The process could not be spawned or joined.
    Io(String),
}

impl fmt::Display for VcsBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VcsBackendError::Invalid(message) => write!(f, "vcs backend invalid: {message}"),
            VcsBackendError::NotFound(message) => write!(f, "vcs backend not found: {message}"),
            VcsBackendError::Unsupported(message) => {
                write!(f, "vcs backend unsupported: {message}")
            }
            VcsBackendError::Atom(error) => write!(f, "{error}"),
            VcsBackendError::Json(message) => write!(f, "vcs backend json error: {message}"),
            VcsBackendError::Git {
                args,
                status,
                stderr,
            } => write!(
                f,
                "git {:?} failed with status {:?}: {}",
                args,
                status,
                stderr.trim()
            ),
            VcsBackendError::Io(message) => write!(f, "vcs backend io error: {message}"),
        }
    }
}

impl std::error::Error for VcsBackendError {}

impl From<AtomError> for VcsBackendError {
    fn from(error: AtomError) -> Self {
        Self::Atom(error)
    }
}

impl From<serde_json::Error> for VcsBackendError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

impl From<std::io::Error> for VcsBackendError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// A candidate shippable unit represented as an ordered atom closure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowSlice {
    pub id: SliceId,
    pub atoms: Vec<AtomId>,
}

impl FlowSlice {
    /// Build a deterministic slice from an ordered atom set.
    pub fn new(atoms: Vec<AtomId>) -> Result<Self, VcsBackendError> {
        if atoms.is_empty() {
            return Err(VcsBackendError::Invalid(
                "slice must contain at least one atom".to_string(),
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"FSLI");
        for atom in &atoms {
            hasher.update(atom.0);
        }
        Ok(Self {
            id: SliceId(hasher.finalize().into()),
            atoms,
        })
    }
}

/// Location of an atom in a VCS backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomRef {
    pub atom_id: AtomId,
    pub commit: String,
    pub ref_name: String,
}

/// Receipt returned after a slice is made visible for shipping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipReceipt {
    pub slice_id: SliceId,
    pub commit: String,
    pub ref_name: String,
}

/// Receipt returned after exporting a Flow slice into a git ref.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitExportReceipt {
    pub slice_id: SliceId,
    pub commit: String,
    pub ref_name: String,
}

/// Storage and shipping abstraction for Harn Flow atoms and slices.
pub trait VcsBackend {
    /// Persist one atom and return its backend location.
    fn emit_atom(&self, atom: &Atom) -> Result<AtomRef, VcsBackendError>;
    /// Derive a shippable slice from an ordered atom set.
    fn derive_slice(&self, atoms: &[AtomId]) -> Result<FlowSlice, VcsBackendError>;
    /// Publish a slice in the backend's native shipping surface.
    fn ship_slice(&self, slice: &FlowSlice) -> Result<ShipReceipt, VcsBackendError>;
    /// List persisted atoms.
    fn list_atoms(&self) -> Result<Vec<AtomRef>, VcsBackendError>;
    /// Load atoms in the order recorded by a slice.
    fn replay_slice(&self, slice: &FlowSlice) -> Result<Vec<Atom>, VcsBackendError>;
    /// Export a slice into a git ref.
    fn export_git(
        &self,
        slice: &FlowSlice,
        ref_name: &str,
    ) -> Result<GitExportReceipt, VcsBackendError>;
    /// Import a git ref containing ShadowGit atom commits as a Flow slice.
    fn import_git(&self, ref_name: &str) -> Result<FlowSlice, VcsBackendError>;
}

/// Git-backed Phase 0 Flow backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowGitBackend {
    repo_root: PathBuf,
}

impl ShadowGitBackend {
    /// Create a backend rooted at an existing git worktree.
    pub fn new(repo_root: impl Into<PathBuf>) -> Result<Self, VcsBackendError> {
        let repo_root = repo_root.into();
        let output = git_output_at(&repo_root, &["rev-parse", "--show-toplevel"], None)?;
        let canonical = PathBuf::from(output.trim());
        Ok(Self {
            repo_root: canonical,
        })
    }

    /// The canonical git worktree root used for all commands.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    fn atom_ref_name(atom_id: AtomId) -> String {
        format!("{ATOM_REF_PREFIX}/{atom_id}")
    }

    fn slice_ref_name(slice_id: SliceId) -> String {
        format!("{SLICE_REF_PREFIX}/{slice_id}")
    }

    fn atom_commit(&self, atom_id: AtomId) -> Result<String, VcsBackendError> {
        let ref_name = Self::atom_ref_name(atom_id);
        git_output_at(
            &self.repo_root,
            &["rev-parse", &format!("{ref_name}^{{commit}}")],
            None,
        )
        .map(|commit| commit.trim().to_string())
        .map_err(|error| match error {
            VcsBackendError::Git { .. } => {
                VcsBackendError::NotFound(format!("atom {atom_id} has no ShadowGit ref"))
            }
            other => other,
        })
    }

    fn atom_from_commit(&self, commit: &str) -> Result<Atom, VcsBackendError> {
        let payload = git_output_at(
            &self.repo_root,
            &["show", &format!("{commit}:atom.json")],
            None,
        )
        .map_err(|error| match error {
            VcsBackendError::Git { .. } => VcsBackendError::NotFound(format!(
                "commit {commit} does not contain a ShadowGit atom payload"
            )),
            other => other,
        })?;
        let atom = Atom::from_json_slice(payload.as_bytes())?;
        Ok(atom)
    }

    fn commit_for_slice(&self, slice: &FlowSlice) -> Result<String, VcsBackendError> {
        let tail = slice
            .atoms
            .last()
            .copied()
            .ok_or_else(|| VcsBackendError::Invalid("slice must contain atoms".to_string()))?;
        self.atom_commit(tail)
    }

    fn append_atom_closure(
        &self,
        atom_id: AtomId,
        seen: &mut HashSet<AtomId>,
        out: &mut Vec<AtomId>,
    ) -> Result<(), VcsBackendError> {
        if !seen.insert(atom_id) {
            return Ok(());
        }

        let commit = self.atom_commit(atom_id)?;
        let atom = self.atom_from_commit(&commit)?;
        if atom.id != atom_id {
            return Err(VcsBackendError::Invalid(format!(
                "commit {commit} payload id {} did not match requested {atom_id}",
                atom.id
            )));
        }
        for parent in &atom.parents {
            self.append_atom_closure(*parent, seen, out)?;
        }
        out.push(atom_id);
        Ok(())
    }

    fn update_ref(&self, ref_name: &str, commit: &str) -> Result<(), VcsBackendError> {
        validate_ref_name(&self.repo_root, ref_name)?;
        git_output_at(&self.repo_root, &["update-ref", ref_name, commit], None)?;
        Ok(())
    }
}

impl VcsBackend for ShadowGitBackend {
    fn emit_atom(&self, atom: &Atom) -> Result<AtomRef, VcsBackendError> {
        atom.verify()?;

        let ref_name = Self::atom_ref_name(atom.id);
        if let Ok(commit) = self.atom_commit(atom.id) {
            return Ok(AtomRef {
                atom_id: atom.id,
                commit,
                ref_name,
            });
        }

        let payload = atom.to_json()?;
        let blob = git_output_at(
            &self.repo_root,
            &["hash-object", "-w", "--stdin"],
            Some(payload.as_bytes()),
        )?;
        let tree_input = format!("100644 blob {}\tatom.json\n", blob.trim());
        let tree = git_output_at(&self.repo_root, &["mktree"], Some(tree_input.as_bytes()))?;

        let mut commit_args = vec!["commit-tree".to_string(), tree.trim().to_string()];
        for parent in &atom.parents {
            let parent_commit = self.atom_commit(*parent)?;
            commit_args.push("-p".to_string());
            commit_args.push(parent_commit);
        }
        commit_args.push("-m".to_string());
        commit_args.push(format!("flow atom {}", atom.id));

        let commit = git_output_at_owned(&self.repo_root, &commit_args, None)?;
        let commit = commit.trim().to_string();
        self.update_ref(&ref_name, &commit)?;
        Ok(AtomRef {
            atom_id: atom.id,
            commit,
            ref_name,
        })
    }

    fn derive_slice(&self, atoms: &[AtomId]) -> Result<FlowSlice, VcsBackendError> {
        let mut seen = HashSet::new();
        let mut closure = Vec::new();
        for atom in atoms {
            self.append_atom_closure(*atom, &mut seen, &mut closure)?;
        }
        FlowSlice::new(closure)
    }

    fn ship_slice(&self, slice: &FlowSlice) -> Result<ShipReceipt, VcsBackendError> {
        let commit = self.commit_for_slice(slice)?;
        let ref_name = Self::slice_ref_name(slice.id);
        self.update_ref(&ref_name, &commit)?;
        Ok(ShipReceipt {
            slice_id: slice.id,
            commit,
            ref_name,
        })
    }

    fn list_atoms(&self) -> Result<Vec<AtomRef>, VcsBackendError> {
        let output = git_output_at(
            &self.repo_root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                ATOM_REF_PREFIX,
            ],
            None,
        )?;
        let mut atoms = Vec::new();
        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let (ref_name, commit) = line
                .split_once(' ')
                .ok_or_else(|| VcsBackendError::Invalid(format!("malformed ref line: {line}")))?;
            let raw_id = ref_name
                .strip_prefix(&format!("{ATOM_REF_PREFIX}/"))
                .ok_or_else(|| {
                    VcsBackendError::Invalid(format!("unexpected atom ref {ref_name}"))
                })?;
            atoms.push(AtomRef {
                atom_id: AtomId::from_hex(raw_id)?,
                commit: commit.to_string(),
                ref_name: ref_name.to_string(),
            });
        }
        atoms.sort_by_key(|atom| atom.atom_id.0);
        Ok(atoms)
    }

    fn replay_slice(&self, slice: &FlowSlice) -> Result<Vec<Atom>, VcsBackendError> {
        slice
            .atoms
            .iter()
            .map(|atom_id| {
                let commit = self.atom_commit(*atom_id)?;
                let atom = self.atom_from_commit(&commit)?;
                if atom.id != *atom_id {
                    return Err(VcsBackendError::Invalid(format!(
                        "commit {commit} payload id {} did not match requested {atom_id}",
                        atom.id
                    )));
                }
                Ok(atom)
            })
            .collect()
    }

    fn export_git(
        &self,
        slice: &FlowSlice,
        ref_name: &str,
    ) -> Result<GitExportReceipt, VcsBackendError> {
        let commit = self.commit_for_slice(slice)?;
        self.update_ref(ref_name, &commit)?;
        Ok(GitExportReceipt {
            slice_id: slice.id,
            commit,
            ref_name: ref_name.to_string(),
        })
    }

    fn import_git(&self, ref_name: &str) -> Result<FlowSlice, VcsBackendError> {
        validate_ref_name(&self.repo_root, ref_name)?;
        let output = git_output_at(&self.repo_root, &["rev-list", "--reverse", ref_name], None)?;
        let mut atoms = Vec::new();
        for commit in output.lines().filter(|line| !line.trim().is_empty()) {
            let atom = self.atom_from_commit(commit)?;
            atoms.push(atom.id);
        }
        FlowSlice::new(atoms)
    }
}

/// Placeholder for the Phase 2 native Flow substrate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowNativeBackend;

impl FlowNativeBackend {
    pub fn new() -> Self {
        Self
    }

    fn unsupported<T>(&self) -> Result<T, VcsBackendError> {
        Err(VcsBackendError::Unsupported(
            "FlowNativeBackend is deferred to Flow Phase 2".to_string(),
        ))
    }
}

impl VcsBackend for FlowNativeBackend {
    fn emit_atom(&self, _atom: &Atom) -> Result<AtomRef, VcsBackendError> {
        self.unsupported()
    }

    fn derive_slice(&self, _atoms: &[AtomId]) -> Result<FlowSlice, VcsBackendError> {
        self.unsupported()
    }

    fn ship_slice(&self, _slice: &FlowSlice) -> Result<ShipReceipt, VcsBackendError> {
        self.unsupported()
    }

    fn list_atoms(&self) -> Result<Vec<AtomRef>, VcsBackendError> {
        self.unsupported()
    }

    fn replay_slice(&self, _slice: &FlowSlice) -> Result<Vec<Atom>, VcsBackendError> {
        self.unsupported()
    }

    fn export_git(
        &self,
        _slice: &FlowSlice,
        _ref_name: &str,
    ) -> Result<GitExportReceipt, VcsBackendError> {
        self.unsupported()
    }

    fn import_git(&self, _ref_name: &str) -> Result<FlowSlice, VcsBackendError> {
        self.unsupported()
    }
}

fn validate_ref_name(repo_root: &Path, ref_name: &str) -> Result<(), VcsBackendError> {
    if ref_name.trim().is_empty() {
        return Err(VcsBackendError::Invalid(
            "git ref name must not be empty".to_string(),
        ));
    }
    git_output_at(repo_root, &["check-ref-format", ref_name], None)?;
    Ok(())
}

fn git_output_at(
    repo_root: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<String, VcsBackendError> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    git_output_at_owned(repo_root, &owned, stdin)
}

fn git_output_at_owned(
    repo_root: &Path,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<String, VcsBackendError> {
    let mut command = Command::new("git");
    command.args(args).current_dir(repo_root);
    clear_git_env(&mut command);
    command
        .env("GIT_AUTHOR_NAME", "Harn Flow")
        .env("GIT_AUTHOR_EMAIL", "flow@harn.local")
        .env("GIT_COMMITTER_NAME", "Harn Flow")
        .env("GIT_COMMITTER_EMAIL", "flow@harn.local");
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(input) = stdin {
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| VcsBackendError::Io("failed to open git stdin".to_string()))?;
        use std::io::Write;
        child_stdin.write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(VcsBackendError::Git {
            args: args.to_vec(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn clear_git_env(command: &mut Command) {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
}
