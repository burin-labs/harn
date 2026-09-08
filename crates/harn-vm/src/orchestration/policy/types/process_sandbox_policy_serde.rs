use serde::{Deserialize, Serialize};

use super::{ProcessSandboxPolicy, ProcessSandboxPreset};

// Adding a public field to `ProcessSandboxPolicy` breaks every downstream host
// that constructs the policy with a complete struct literal. Keep the source
// shape stable and encode the new authority behind the owning type instead.
// NUL cannot occur in an OS path, so callers cannot confuse a real read root
// with this private representation. Custom serde projects it as the explicit
// `unix_socket_roots` wire field.
const UNIX_SOCKET_ROOT_PREFIX: &str = "\0harn:unix-socket-root:";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct ProcessSandboxPolicyWire {
    presets: Option<Vec<ProcessSandboxPreset>>,
    read_roots: Vec<String>,
    write_roots: Vec<String>,
    read_deny_roots: Vec<String>,
    allow_tcp_loopback: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unix_socket_roots: Vec<String>,
}

impl From<&ProcessSandboxPolicy> for ProcessSandboxPolicyWire {
    fn from(policy: &ProcessSandboxPolicy) -> Self {
        Self {
            presets: policy.presets.clone(),
            read_roots: policy.explicit_read_roots(),
            write_roots: policy.write_roots.clone(),
            read_deny_roots: policy.read_deny_roots.clone(),
            allow_tcp_loopback: policy.allow_tcp_loopback,
            unix_socket_roots: policy.unix_socket_roots(),
        }
    }
}

impl From<ProcessSandboxPolicyWire> for ProcessSandboxPolicy {
    fn from(wire: ProcessSandboxPolicyWire) -> Self {
        let mut policy = Self {
            presets: wire.presets,
            // Reserved extension entries are an in-memory representation, not
            // another wire-level authority path. Accept socket roots only from
            // their explicit field so an untrusted `read_roots` value cannot
            // smuggle a socket grant past the owning policy dimension.
            read_roots: wire
                .read_roots
                .into_iter()
                .filter(|root| !root.starts_with(UNIX_SOCKET_ROOT_PREFIX))
                .collect(),
            write_roots: wire.write_roots,
            read_deny_roots: wire.read_deny_roots,
            allow_tcp_loopback: wire.allow_tcp_loopback,
        };
        policy.set_unix_socket_roots(wire.unix_socket_roots);
        policy
    }
}

impl Serialize for ProcessSandboxPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ProcessSandboxPolicyWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProcessSandboxPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ProcessSandboxPolicyWire::deserialize(deserializer).map(Self::from)
    }
}

impl ProcessSandboxPolicy {
    /// Explicit process read roots, excluding type-owned extension state.
    pub fn explicit_read_roots(&self) -> Vec<String> {
        self.read_roots
            .iter()
            .filter(|root| !root.starts_with(UNIX_SOCKET_ROOT_PREFIX))
            .cloned()
            .collect()
    }

    /// Directories under which a confined child may use Unix-domain sockets.
    pub fn unix_socket_roots(&self) -> Vec<String> {
        self.read_roots
            .iter()
            .filter_map(|root| {
                root.strip_prefix(UNIX_SOCKET_ROOT_PREFIX)
                    .map(str::to_string)
            })
            .collect()
    }

    pub fn set_unix_socket_roots(&mut self, roots: Vec<String>) {
        self.read_roots
            .retain(|root| !root.starts_with(UNIX_SOCKET_ROOT_PREFIX));
        for root in roots {
            let encoded = format!("{UNIX_SOCKET_ROOT_PREFIX}{root}");
            if !self.read_roots.contains(&encoded) {
                self.read_roots.push(encoded);
            }
        }
    }

    pub fn with_unix_socket_roots(mut self, roots: Vec<String>) -> Self {
        self.set_unix_socket_roots(roots);
        self
    }
}
