//! The process-only sandbox vocabulary: the named filesystem presets a
//! confined child may load from, the per-run grants layered onto the active
//! profile, and the disposition a backend reports for a grant it was asked to
//! render.
//!
//! This sits beside the rest of the policy types rather than inside them
//! because it is the one part of a capability policy that describes a
//! *different* enforcement surface. Everything else in `types` constrains what
//! Harn's own builtins may do; these describe what the operating system is
//! asked to permit for a child process, and the two attenuate independently.

use serde::{Deserialize, Serialize};

use super::{extend_unique, intersect_presets, intersect_roots, is_false};

/// Named host filesystem presets granted only to child-process OS
/// sandboxes. These do not widen Harn file builtins; they are used so
/// subprocesses can load runtimes, compilers, and cache files that live
/// outside the workspace while Harn's own read/write surface remains
/// scoped by `workspace_roots` and `read_only_roots`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSandboxPreset {
    /// Minimal host runtime roots needed to execute common system binaries.
    SystemRuntime,
    /// OS/vendor developer toolchains such as Xcode, Command Line Tools,
    /// Homebrew, plus common user-managed runtime roots such as
    /// `~/.local/share/uv`, `~/.rustup`, `~/.cargo`, `~/.pyenv`, and `~/.nvm`.
    DeveloperToolchains,
    /// Per-user package-manager config/cache roots used by npm, pip, cargo,
    /// git credential helpers, and enterprise CA configuration.
    PackageManagerConfig,
    /// Per-user scratch/cache locations used by developer tools. Write access
    /// is granted only when the active policy already allows workspace writes.
    UserTemp,
}

impl ProcessSandboxPreset {
    pub const fn default_presets() -> &'static [Self] {
        &[
            Self::SystemRuntime,
            Self::DeveloperToolchains,
            Self::PackageManagerConfig,
            Self::UserTemp,
        ]
    }
}

/// Process-only policy layered onto the active sandbox profile.
///
/// `presets: None` means "use the runtime defaults"; `Some([])` is an
/// explicit request for no named presets. Extra roots are process-only:
/// they do not allow Harn file tools to read or write those paths. TCP
/// loopback is a separate capability from external network access so local
/// test servers do not require a remote-egress grant.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProcessSandboxPolicy {
    pub presets: Option<Vec<ProcessSandboxPreset>>,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    /// Subtrees a confined child may never read, whatever else grants them.
    ///
    /// This is the ONLY subtractive term in the policy. It composes
    /// most-restrictive over every additive source: a preset, a workspace root,
    /// a `read_only_root`, and an explicit `read_roots` entry all lose to it.
    /// That ordering is the point — `PackageManagerConfig` grants `~/.config`,
    /// `~/.cache`, and `~/.netrc` wholesale, so a denylist that merely competed
    /// with presets would leave credentials readable by default.
    ///
    /// macOS renders it as a trailing `deny file-read*` (last-match-wins);
    /// Linux, whose Landlock is allow-only, grants the siblings that do not
    /// lead to it. Which backends hold it is the `credential_reads` column of
    /// the enforcement table (`process_sandbox::enforcement`), not this
    /// comment.
    ///
    /// A backend that does not hold it refuses only under `os_hardened`.
    /// Refusing under every profile would be the fail-closed reflex, and it is
    /// wrong here: the default denylist is never empty, so it would refuse
    /// every spawn on that platform. The table's receipt says plainly that the
    /// term is unenforced there instead.
    pub read_deny_roots: Vec<String>,
    /// Permit a confined child to bind and connect TCP loopback sockets while
    /// retaining the deny on non-loopback destinations. Backends that cannot
    /// enforce this distinction reject the spawn rather than widening it.
    pub allow_tcp_loopback: bool,
    /// Directories under which a confined child may bind and connect
    /// Unix-domain sockets. Path-scoped: a socket outside every root is still
    /// refused, and nothing here grants IP networking. Build servers (sbt,
    /// Gradle's Kotlin daemon, MSBuild worker nodes) talk to themselves over a
    /// socket file under the project or temp dir; without this grant they die
    /// with a bare `Operation not permitted` that reads like a toolchain defect.
    ///
    /// Enforced on macOS, where the seatbelt filters sockets by path.
    ///
    /// Linux renders this grant as **serve-only** local IPC, which is a
    /// deliberately different and narrower shape than the macOS one. The
    /// kernel offers no access right governing connection to a filesystem
    /// socket at any Landlock ABI, so a child that may `connect` can reach
    /// every socket its uid can open — a container daemon's among them, which
    /// is an escape and not a grant. Linux therefore admits socket creation,
    /// `bind`, `listen` and `accept` for the Unix domain, scopes creation to
    /// these roots, contains abstract sockets inside the sandbox domain, and
    /// **refuses `connect` outright**. A build server that talks to itself is
    /// served; a child reaching for somebody else's socket is not. Build
    /// servers (sbt, Gradle's Kotlin daemon, MSBuild worker nodes) need only
    /// the serving half, so this costs them nothing, and a policy that also
    /// permits general networking keeps `connect` because it was already
    /// reachable.
    ///
    /// Windows and OpenBSD still reject a non-empty grant rather than widening
    /// it, the contract `allow_tcp_loopback` follows. Each backend's actual
    /// disposition is reported rather than assumed; see
    /// [`UnixSocketEnforcement`]. Omitted from the wire when empty so no
    /// workflow graph digest pinned before the field existed moves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unix_socket_roots: Vec<String>,
    /// Permit a confined child to enumerate its own entries under the process
    /// filesystem, not merely read the files it already knows the names of.
    ///
    /// Managed runtimes discover their own identity this way. The .NET build
    /// engine reads its process name from `/proc/self/task` inside a static
    /// initializer, before any project is loaded, so a child without this
    /// grant cannot start at all and reports `MSB1025` rather than anything
    /// resembling a permission problem.
    ///
    /// Enforced on Linux, which otherwise grants procfs file reads without
    /// directory reads. It is a no-op on macOS and Windows, which have no
    /// procfs and do not gate self-identification this way, so those backends
    /// neither widen nor refuse for it.
    ///
    /// The grant is read-only and carries no network authority whatsoever. It
    /// does let a child see the other processes of its own uid, because
    /// Landlock resolves a rule to an inode and a rule narrow enough to name
    /// only this process cannot cover the grandchildren a compiler driver
    /// spawns. That widening is stated rather than hidden.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_process_self_introspection: bool,
    /// Absolute path to the helper that builds a private network namespace for
    /// a confined child, supplied by the embedder rather than compiled in.
    ///
    /// Loopback-only child networking has no expression in either kernel
    /// interface this backend uses. A syscall filter decides address families,
    /// not addresses, and Landlock scopes network access by port and never by
    /// address, so "loopback and nothing else" can only be built as a private
    /// network namespace with one interface raised. The namespace must exist
    /// *before* confinement, and confinement must still be installed before
    /// the child's program is reached, which a pre-exec callback cannot do:
    /// the filter it installs is a default-deny allowlist carrying no
    /// namespace syscalls, so a helper run behind it dies at `unshare`.
    ///
    /// The helper therefore runs first and receives the policy as data. It is
    /// named here rather than derived because on distributions that restrict
    /// unprivileged namespaces the permission is granted per executable path
    /// by host policy, and that grant must name one stable, separately
    /// installed file. Deriving the path from this binary would move the grant
    /// onto whichever build happened to be running.
    ///
    /// Absent, a loopback request is **refused** and the refusal names the
    /// path that was looked for. It is never degraded to a weaker grant: the
    /// weaker grants leak datagram egress, and a reader of the receipt would
    /// have no way to tell which one was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netns_launcher_path: Option<String>,
}

/// What a backend actually did with [`ProcessSandboxPolicy::unix_socket_roots`].
///
/// The grant means different things on different kernels, and a reader of a
/// receipt must not have to infer which. Absence of a denial is not evidence
/// of a grant, so every backend states its disposition even when it refused.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UnixSocketEnforcement {
    /// No grant was requested, so nothing was decided.
    NotRequested,
    /// Creation, binding and connection are all scoped to the named roots.
    PathScoped,
    /// Creation is scoped to the named roots and connection is refused
    /// outright, because this kernel cannot scope a connection by path.
    ServeOnly,
    /// The policy already permits general networking, so the socket
    /// operations (creation, binding, connection, and abstract sockets) are
    /// authority the policy holds for another reason and the roots do not
    /// scope them. Socket files are still admitted under the named roots,
    /// including a root outside every writable root; only the syscall half is
    /// superseded, never the path half.
    SupersededByNetworkGrant,
    /// The backend cannot render the grant and refused the spawn rather than
    /// widening it.
    Refused,
}

/// What a backend actually did with [`ProcessSandboxPolicy::allow_tcp_loopback`].
///
/// Loopback is the grant with the widest gap between what was asked for and
/// what a given mechanism can deliver, so naming the mechanism is the point.
/// A reader who sees only that loopback was permitted cannot tell whether
/// datagrams can still leave the host, and that difference is the whole
/// security argument.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LoopbackEnforcement {
    /// No loopback grant was requested, so nothing was decided.
    NotRequested,
    /// A private network namespace with only the loopback interface raised.
    /// There is no route off the host to deny, so there is no residual: this
    /// is the only mechanism that closes datagram egress as well as stream
    /// egress.
    PrivateNetworkNamespace,
    /// The backend cannot build a namespace and refused the spawn rather than
    /// issuing one of the weaker grants. The refusal names what it looked for.
    Refused,
}

/// Runtime-owned forwarding endpoints for a managed child-process egress
/// proxy. Naming a loopback endpoint is authority because the OS sandbox grants
/// it; only the host may install this transport state. Destination decisions
/// remain owned by `crate::egress`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessNetworkProxy {
    pub http_port: u16,
    pub socks_port: u16,
}

impl ProcessSandboxPolicy {
    pub fn effective_presets(&self) -> Vec<ProcessSandboxPreset> {
        self.presets
            .clone()
            .unwrap_or_else(|| ProcessSandboxPreset::default_presets().to_vec())
    }

    pub fn extend(&mut self, other: &Self) {
        if let Some(presets) = other.presets.as_ref() {
            self.presets = Some(presets.clone());
        }
        extend_unique(&mut self.read_roots, &other.read_roots);
        extend_unique(&mut self.write_roots, &other.write_roots);
        extend_unique(&mut self.read_deny_roots, &other.read_deny_roots);
        self.allow_tcp_loopback |= other.allow_tcp_loopback;
        extend_unique(&mut self.unix_socket_roots, &other.unix_socket_roots);
        self.allow_process_self_introspection |= other.allow_process_self_introspection;
        if let Some(path) = other.netns_launcher_path.as_ref() {
            self.netns_launcher_path = Some(path.clone());
        }
    }

    pub(super) fn intersect(&self, requested: &Self) -> Self {
        let presets = match (&self.presets, &requested.presets) {
            (None, None) => None,
            _ => Some(intersect_presets(
                &self.effective_presets(),
                &requested.effective_presets(),
            )),
        };
        Self {
            presets,
            read_roots: intersect_roots(&self.read_roots, &requested.read_roots),
            write_roots: intersect_roots(&self.write_roots, &requested.write_roots),
            // UNION, not intersection, and deliberately so. Every other field
            // here narrows as it nests; this one is a denial, so narrowing it
            // would WIDEN the resulting authority. A nested request may add a
            // denial and may never drop one the outer policy made.
            read_deny_roots: {
                let mut denied = self.read_deny_roots.clone();
                extend_unique(&mut denied, &requested.read_deny_roots);
                denied
            },
            // Loopback is host-owned authority like `process_network_proxy`:
            // a nested request may neither invent it nor erase an outer grant.
            // Host configuration still composes additively through `extend`.
            allow_tcp_loopback: self.allow_tcp_loopback,
            unix_socket_roots: intersect_roots(
                &self.unix_socket_roots,
                &requested.unix_socket_roots,
            ),
            // Narrows like every other additive term: a nested request may
            // keep the grant its ceiling already made and may never invent it.
            allow_process_self_introspection: self.allow_process_self_introspection
                && requested.allow_process_self_introspection,
            // Host-owned like `allow_tcp_loopback` and for the same reason:
            // naming an executable that may build a namespace is authority,
            // and a nested request may neither invent it nor erase the one
            // its ceiling installed. Host configuration still composes
            // additively through `extend`.
            netns_launcher_path: self.netns_launcher_path.clone(),
        }
    }
}
