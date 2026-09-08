//! Which boundary of the process sandbox refused a child, inferred from the
//! child's own output.
//!
//! Split out of `refusal.rs` to keep that file under the source-length cap.
//! The marker vocabulary is DATA (`refusal_markers.toml`), not Rust consts:
//! adding the phrase a new build tool prints is a reviewable one-line diff in
//! a file whose only job is to say what the phrases are.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::{
    normalize_for_policy, normalized_workspace_roots, path_is_within, sandbox_user_home_dir,
};
use super::{path_is_denied, process_sandbox_read_deny_roots};
use crate::orchestration::CapabilityPolicy;

static REFUSAL_MARKERS_TOML: &str = include_str!("refusal_markers.toml");

/// The output phrases that identify each boundary. Every list is lowercase and
/// matched by substring against the lowercased child output.
#[derive(Debug, Deserialize)]
struct RefusalMarkers {
    /// Present in every refusal the OS sandbox produces; gates the write and
    /// home-read classes so a tool's ordinary "could not create" is not read
    /// as a sandbox denial.
    permission: Vec<String>,
    egress: Vec<String>,
    local_socket: Vec<String>,
    write: Vec<String>,
}

/// Parsed once. A parse failure is a hard error, not an empty vocabulary: an
/// empty vocabulary classifies every refusal as `Unknown`, which is exactly
/// the misdiagnosis this module exists to end, while every other signature of
/// a working classifier stays intact.
fn markers() -> &'static RefusalMarkers {
    static PARSED: std::sync::OnceLock<RefusalMarkers> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        let parsed: RefusalMarkers = toml::from_str(REFUSAL_MARKERS_TOML)
            .expect("refusal_markers.toml must parse; the refusal classifier is not optional");
        for (name, list) in [
            ("permission", &parsed.permission),
            ("egress", &parsed.egress),
            ("local_socket", &parsed.local_socket),
            ("write", &parsed.write),
        ] {
            assert!(!list.is_empty(), "refusal_markers.toml `{name}` is empty");
            assert!(
                list.iter().all(|marker| *marker == marker.to_ascii_lowercase()),
                "refusal_markers.toml `{name}` entries must be lowercase; output is lowercased before matching"
            );
        }
        parsed
    })
}

/// WHICH boundary of the process sandbox refused the child, as far as the
/// child's own output lets us tell.
///
/// Three grants are mutually invisible from the bare `Operation not permitted`
/// they all produce: remote egress, local sockets, and reads under the user's
/// home directory. A consumer that only knows "the sandbox refused something"
/// reaches for the egress story every time, because that is the denial people
/// expect — and then a build server's socket or a package manager's home
/// config costs a day of misdiagnosis. This names the boundary so the
/// receipt and the agent both read the right one.
///
/// Inferred from the child's output, like everything else on the refusal
/// record; `Unknown` is the honest answer when the output names none of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessSandboxMechanism {
    /// The child tried to reach a host off this machine and the sandbox
    /// denies remote egress.
    Egress,
    /// The child tried to bind or connect a local socket — a Unix-domain
    /// socket file, or a loopback endpoint — and the sandbox admits neither
    /// without a grant.
    LocalSocket,
    /// The child read a file under the user's home directory that no preset
    /// or root grants, or that the credential denylist refuses.
    HomeRead,
    /// The child wrote outside every writable root.
    Write,
    #[default]
    Unknown,
}

impl ProcessSandboxMechanism {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Egress => "egress",
            Self::LocalSocket => "local_socket",
            Self::HomeRead => "home_read",
            Self::Write => "write",
            Self::Unknown => "unknown",
        }
    }

    /// What the mechanism means for the person or agent reading the refusal,
    /// with the grant that would have admitted the operation.
    pub fn explanation(self, grants: &ProcessSandboxGrants) -> String {
        match self {
            Self::Egress => "the sandbox denies network egress off this machine; \
                             the child tried to reach a remote host (a package registry, \
                             a repository, an update check). Warm the dependency it \
                             wanted before the confined run, or grant egress explicitly"
                .to_string(),
            Self::LocalSocket => format!(
                "the sandbox refused a local socket, not network egress. TCP loopback is {}; \
                 Unix-domain sockets are {}. Build servers and compiler daemons (sbt, \
                 Gradle, MSBuild) need one or both",
                if grants.tcp_loopback {
                    "granted"
                } else {
                    "not granted (allow_tcp_loopback / --allow-process-loopback)"
                },
                if grants.unix_socket_roots.is_empty() {
                    "not granted anywhere (process_sandbox.unix_socket_roots / \
                     --sandbox-unix-socket-root)"
                        .to_string()
                } else {
                    format!("granted only under {}", grants.unix_socket_roots.join(", "))
                },
            ),
            Self::HomeRead => format!(
                "the sandbox refused a read under the user's home directory{}; tool config \
                 outside the granted roots is not readable by a confined child. Point the \
                 tool at a workspace-local config (its *_HOME or *_CONFIG variable) or grant \
                 that one path as a process read root",
                match &grants.denied_home_path {
                    Some(path) => format!(" ({path} is on the credential denylist)"),
                    None => String::new(),
                }
            ),
            Self::Write => "the sandbox refused a write outside every writable root; grant the \
                            directory as a process write root or point the tool's cache at \
                            the workspace"
                .to_string(),
            Self::Unknown => "the sandbox refused an operation the child's output does not \
                              identify; see stderr_excerpt"
                .to_string(),
        }
    }
}

/// The socket and home-read grants in force when a refusal is classified, so
/// the explanation can say what WAS granted instead of guessing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessSandboxGrants {
    pub tcp_loopback: bool,
    pub unix_socket_roots: Vec<String>,
    /// The home-relative credential path the output named, when the refused
    /// read was one the denylist refuses by design.
    pub denied_home_path: Option<String>,
}

impl ProcessSandboxGrants {
    pub fn from_policy(policy: &CapabilityPolicy) -> Self {
        Self {
            tcp_loopback: policy.process_sandbox.allow_tcp_loopback,
            unix_socket_roots: policy.process_sandbox.unix_socket_roots(),
            denied_home_path: None,
        }
    }
}

fn permission_marker_present(haystack: &str) -> bool {
    markers()
        .permission
        .iter()
        .any(|marker| haystack.contains(marker))
}

/// Every absolute path spelled in the excerpt, so a home-directory read can be
/// recognised without anyone parsing the sentence around it.
fn absolute_paths_in(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    // Do not split on `:`. A Windows path is `C:\Users\...`; splitting there
    // leaves `C` and `\Users\...`, and neither looks absolute.
    for token in text
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '(' | ')' | ',' | ';'))
    {
        let trimmed = token
            .trim_end_matches(['.', '!', '?', ':'])
            .trim_end_matches(['.', '!', '?']);
        if is_absolute_path_token(trimmed) {
            paths.push(trimmed.to_string());
        }
    }
    paths
}

fn is_absolute_path_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    (token.starts_with('/') && token.len() > 1)
        || token.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

/// Classify the boundary a refused child hit from its output and the policy in
/// force. Writes are checked first (a refused cache write under `~` names the
/// home path too), then home reads, then egress, then local sockets: a tool
/// that fails to read its config often goes on to print a resolver or socket
/// error next, and the first denial is the one to fix.
pub fn infer_process_sandbox_mechanism(
    evidence: &str,
    policy: Option<&CapabilityPolicy>,
) -> (ProcessSandboxMechanism, ProcessSandboxGrants) {
    let mut grants = policy
        .map(ProcessSandboxGrants::from_policy)
        .unwrap_or_default();
    let lower = evidence.to_ascii_lowercase();
    let permission = permission_marker_present(&lower);
    if permission && markers().write.iter().any(|marker| lower.contains(marker)) {
        return (ProcessSandboxMechanism::Write, grants);
    }
    if permission {
        if let Some(home) = sandbox_user_home_dir() {
            let home = normalize_for_policy(&home);
            let workspace_roots = policy.map(normalized_workspace_roots).unwrap_or_default();
            let home_reads: Vec<PathBuf> = absolute_paths_in(evidence)
                .iter()
                .map(|path| normalize_for_policy(Path::new(path)))
                .filter(|path| path_is_within(path, &home))
                .filter(|path| {
                    !workspace_roots
                        .iter()
                        .any(|root| path_is_within(path, root))
                })
                .collect();
            if !home_reads.is_empty() {
                let denied = policy
                    .map(process_sandbox_read_deny_roots)
                    .unwrap_or_default();
                grants.denied_home_path = home_reads
                    .iter()
                    .find(|path| path_is_denied(path, &denied))
                    .map(|path| path.display().to_string());
                return (ProcessSandboxMechanism::HomeRead, grants);
            }
        }
    }
    if markers().egress.iter().any(|marker| lower.contains(marker)) {
        return (ProcessSandboxMechanism::Egress, grants);
    }
    if markers()
        .local_socket
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return (ProcessSandboxMechanism::LocalSocket, grants);
    }
    (ProcessSandboxMechanism::Unknown, grants)
}

#[cfg(test)]
mod path_tokens {
    #[test]
    fn a_windows_drive_path_is_one_token_not_a_split_on_the_colon() {
        let paths = super::absolute_paths_in(
            r"file C:\home\builder\.composer\config.json is not readable.",
        );
        assert_eq!(
            paths,
            vec![r"C:\home\builder\.composer\config.json".to_string()]
        );
    }

    #[test]
    fn a_unix_path_is_still_collected() {
        let paths =
            super::absolute_paths_in("file /home/me/.composer/config.json is not readable.");
        assert_eq!(paths, vec!["/home/me/.composer/config.json".to_string()]);
    }
}
