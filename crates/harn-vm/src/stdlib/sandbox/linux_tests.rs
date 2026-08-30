//! Tests for the linux sandbox backend, split out of `linux.rs` to keep
//! that file under the source-length ratchet. Same module path as before
//! (`super::*` still resolves to `linux.rs`), so nothing about scope moved.

use super::*;

const WRITE_BITS: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_REFER
    | LANDLOCK_ACCESS_FS_TRUNCATE;

fn linux_policy_with_workspace_ops(ops: &[&str]) -> CapabilityPolicy {
    CapabilityPolicy {
        tools: Vec::new(),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            ops.iter().map(|op| op.to_string()).collect(),
        )]),
        workspace_roots: vec!["/ws".to_string()],
        read_only_roots: Vec::new(),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: std::collections::BTreeMap::new(),
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: Default::default(),
        process_network_proxy: None,
    }
}

#[test]
fn managed_proxy_fails_closed_without_proxy_only_network_namespace() {
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("network".to_string());
    policy.process_network_proxy = Some(crate::orchestration::ProcessNetworkProxy {
        http_port: 3128,
        socks_port: 1080,
    });

    let error = match profile_setup("ignored", &policy, SandboxProfile::Worktree) {
        Ok(_) => panic!("managed proxy must not widen to unrestricted Linux sockets"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires a proxy-only Linux network namespace"),
        "{error}"
    );
}

#[test]
fn no_network_excludes_addressable_sockets_but_allows_local_socketpair() {
    // At a sub-network ceiling, the egress-capable socket syscalls are
    // not allowlisted, but `socketpair` (anonymous, unaddressable local IPC) stays
    // allowed so Cargo's socketpair-backed jobserver can spawn rustc.
    let policy = linux_policy_with_workspace_ops(&["read_text"]);
    assert_eq!(
        policy.side_effect_level.as_deref(),
        Some("read_only"),
        "fixture must be below the network ceiling",
    );
    let allowed = allowed_syscalls(&policy);

    assert!(
        !allowed.contains(&libc::SYS_socket),
        "addressable socket() must not be allowlisted without network",
    );
    assert!(
        !allowed.contains(&libc::SYS_connect),
        "connect() must not be allowlisted without network",
    );
    assert!(
        allowed.contains(&libc::SYS_socketpair),
        "socketpair() (local IPC) must be allowlisted — Cargo's jobserver needs it",
    );
    // The socketpair-backed jobserver also drives its pair with the
    // send/recv family. They open no egress while socket/connect/bind
    // stay absent from the allowlist.
    for call in [
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_sendmsg,
        libc::SYS_sendto,
    ] {
        assert!(
                allowed.contains(&call),
                "send/recv syscall {call} must be allowlisted — local socketpair IPC (Cargo jobserver) needs it",
            );
    }
    // The egress-capable openers stay absent: no addressable socket can be
    // created or routed, so the inherited-fd send/recv calls cannot reach the network.
    for call in [
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
    ] {
        assert!(
            !allowed.contains(&call),
            "egress opener {call} must stay absent without network",
        );
    }
}

#[test]
fn network_ceiling_allows_all_socket_syscalls() {
    // When network side effects are permitted, none of the socket family
    // is removed from the allowlist (socketpair included).
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("network".to_string());
    let allowed = allowed_syscalls(&policy);
    for call in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
    ] {
        assert!(
            allowed.contains(&call),
            "network ceiling must allowlist socket-family syscall {call}",
        );
    }
}

#[test]
fn network_ceiling_grants_exact_name_service_files_without_opening_run() {
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    assert!(network_name_service_read_roots(&policy).is_empty());

    policy.side_effect_level = Some("network".to_string());
    let roots = network_name_service_read_roots(&policy);
    assert_eq!(
        roots,
        [
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/gai.conf",
            "/etc/host.conf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>(),
    );
    assert!(
        roots.iter().all(|root| !root.starts_with("/run")),
        "the repair must grant canonical resolver files, never the mutable /run tree",
    );
}

#[test]
fn process_network_ceiling_controls_real_child_socket() {
    let workspace = tempfile::tempdir().expect("workspace");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let args = vec![
        "-c".to_string(),
        format!("exec 3<>/dev/tcp/127.0.0.1/{}", address.port()),
    ];

    let run_probe = |policy: &CapabilityPolicy| {
        let mut command = Command::new("/bin/bash");
        command.args(&args).current_dir(workspace.path());
        let preparation = Backend::prepare_std_command(
            "/bin/bash",
            &args,
            &mut command,
            policy,
            SandboxProfile::Worktree,
        )
        .expect("prepare sandboxed child");
        assert!(matches!(preparation, PrepareOutcome::Direct));
        command.output().expect("run sandboxed child")
    };

    let mut denied = linux_policy_with_workspace_ops(&["read_text"]);
    denied.workspace_roots = vec![workspace.path().display().to_string()];
    denied.side_effect_level = Some("process_exec".to_string());
    let denied_output = run_probe(&denied);
    assert!(
        !denied_output.status.success(),
        "the default process-exec ceiling must deny an addressable child socket",
    );

    let mut allowed = denied;
    allowed.side_effect_level = Some("network".to_string());
    let allowed_output = run_probe(&allowed);
    assert!(
        allowed_output.status.success(),
        "the network ceiling must permit the child loopback socket: {}",
        String::from_utf8_lossy(&allowed_output.stderr),
    );
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    listener
        .accept()
        .expect("the listener must observe the allowed child connection");
}

#[test]
fn seccomp_filter_is_default_deny_allowlist() {
    let filter = compile_seccomp_program(&[libc::SYS_read, libc::SYS_write])
        .expect("compile the probe filter");
    assert_eq!(
        filter.last().map(|entry| entry.k),
        Some(libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        "seccomp fallthrough must deny unknown syscalls",
    );
    assert!(
        filter
            .iter()
            .any(|entry| entry.k == libc::SECCOMP_RET_ALLOW),
        "allowlisted syscalls must jump to an allow action",
    );
}

/// The filter must reject foreign ABIs before it ever looks at a syscall
/// number. `msync` stands in for the general hazard: we allow number 26,
/// and i386 number 26 is `ptrace` — which
/// `allowlist_excludes_process_introspection_and_io_uring` asserts we
/// withhold. Without the arch gate that exclusion is reachable anyway,
/// through `int $0x80`.
#[test]
fn seccomp_filter_validates_architecture_before_syscall_number() {
    let filter = compile_seccomp_program(&[libc::SYS_msync]).expect("compile the probe filter");

    let arch_load = filter.first().expect("filter must not be empty");
    assert_eq!(
        arch_load.code,
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        "the first instruction must be an absolute word load",
    );
    assert_eq!(
        arch_load.k, 4,
        "the first load must read seccomp_data.arch (offset 4), not .nr (offset 0)",
    );

    assert_eq!(
        filter.get(2).map(|entry| entry.k),
        Some(libc::SECCOMP_RET_KILL_PROCESS),
        "an architecture mismatch must kill the process, never return EPERM: \
             EPERM would let a caller probe the whole syscall space for free",
    );

    // Only after the arch gate may the program consult the syscall number.
    assert_eq!(
        filter.get(3).map(|entry| (entry.code, entry.k)),
        Some(((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0)),
        "the syscall number load must follow the architecture check",
    );
}

#[test]
fn allowlist_excludes_process_introspection_and_io_uring() {
    let policy = linux_policy_with_workspace_ops(&["read_text", "write_text"]);
    let allowed = allowed_syscalls(&policy);
    for call in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ] {
        assert!(
            !allowed.contains(&call),
            "dangerous syscall {call} must stay outside the seccomp allowlist",
        );
    }
}

#[test]
fn read_only_access_grants_read_and_execute_but_never_write() {
    let access = read_only_access();
    assert_ne!(access & LANDLOCK_ACCESS_FS_READ_FILE, 0, "read file");
    assert_ne!(access & LANDLOCK_ACCESS_FS_READ_DIR, 0, "read dir");
    assert_ne!(access & LANDLOCK_ACCESS_FS_EXECUTE, 0, "execute");
    assert_eq!(
        access & WRITE_BITS,
        0,
        "read-only access must not carry any write/create/remove right",
    );
}

#[test]
fn read_only_access_is_independent_of_workspace_write_capability() {
    // Even when the policy otherwise allows workspace writes, the
    // read-only access bits are unchanged: a read-only root gets
    // read+execute only.
    let writable = linux_policy_with_workspace_ops(&["read_text", "write_text", "delete"]);
    assert_ne!(
        workspace_access(&writable) & LANDLOCK_ACCESS_FS_WRITE_FILE,
        0,
        "writable workspace root should carry write",
    );
    assert_eq!(
        read_only_access() & WRITE_BITS,
        0,
        "read-only roots stay unwritable regardless of workspace write capability",
    );
}

#[test]
fn package_manager_config_roots_are_read_only() {
    let temp_home = tempfile::tempdir().expect("temp home");
    std::fs::write(
        temp_home.path().join(".npmrc"),
        "registry=https://registry.example\n",
    )
    .expect("write npmrc");
    let roots = super::super::package_manager_config_read_roots_for_home(temp_home.path());

    assert!(
        roots.iter().any(|path| path.ends_with(".npmrc")),
        "npmrc should be part of the package-manager preset"
    );
    assert!(
        roots
            .iter()
            .any(|path| path.ends_with(".cargo/config.toml")),
        "cargo config should be part of the package-manager preset"
    );
    assert!(
        roots.iter().all(|path| path.starts_with(temp_home.path())),
        "package-manager roots must stay under HOME"
    );
    assert_eq!(
        read_only_access() & WRITE_BITS,
        0,
        "package-manager Landlock rules use read-only access bits"
    );
}

#[test]
fn developer_toolchain_roots_are_read_only() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let roots = super::super::developer_toolchain_read_roots_for_home(temp_home.path());

    assert!(
        roots.iter().any(|path| path.ends_with(".local/share/uv")),
        "uv runtimes should be part of the developer-toolchain preset"
    );
    assert!(
        roots.iter().any(|path| path.ends_with(".rustup")),
        "rustup should be part of the developer-toolchain preset"
    );
    assert!(
        roots.iter().all(|path| path.starts_with(temp_home.path())),
        "developer-toolchain roots must stay under HOME"
    );
    assert_eq!(
        read_only_access() & WRITE_BITS,
        0,
        "developer-toolchain Landlock rules use read-only access bits"
    );
}

#[test]
fn developer_toolchains_admit_linux_vendor_installations() {
    let enabled = CapabilityPolicy {
        process_sandbox: crate::orchestration::ProcessSandboxPolicy {
            presets: Some(vec![ProcessSandboxPreset::DeveloperToolchains]),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        developer_toolchain_system_read_roots(&enabled),
        vec![PathBuf::from("/opt")]
    );

    let disabled = CapabilityPolicy {
        process_sandbox: crate::orchestration::ProcessSandboxPolicy {
            presets: Some(Vec::new()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(developer_toolchain_system_read_roots(&disabled).is_empty());
}

#[test]
fn standard_device_rules_allow_common_device_files_only() {
    let rules = standard_device_rules();
    assert_eq!(rules.len(), 4);
    assert!(rules.iter().any(
        |(path, access)| path.as_path() == std::path::Path::new("/dev/null")
            && access & LANDLOCK_ACCESS_FS_READ_FILE != 0
            && access & LANDLOCK_ACCESS_FS_WRITE_FILE != 0
            && access & LANDLOCK_ACCESS_FS_IOCTL_DEV == 0
    ));
    for device in ["/dev/zero", "/dev/random", "/dev/urandom"] {
        let Some((_, access)) = rules
            .iter()
            .find(|(path, _)| path.as_path() == std::path::Path::new(device))
        else {
            panic!("missing standard device rule for {device}");
        };
        assert_ne!(
            *access & LANDLOCK_ACCESS_FS_READ_FILE,
            0,
            "{device} should be readable"
        );
        assert_eq!(
            *access & LANDLOCK_ACCESS_FS_WRITE_FILE,
            0,
            "{device} must not be writable"
        );
        assert_eq!(
            *access & LANDLOCK_ACCESS_FS_IOCTL_DEV,
            0,
            "{device} must not receive device ioctl access"
        );
    }
}

#[test]
fn directory_only_access_excludes_file_applicable_rights() {
    // The file-applicable rights must never be classified as
    // directory-only, otherwise `push_rule` would strip a read/exec
    // grant from a regular-file rule and silently under-scope it.
    for right in [
        LANDLOCK_ACCESS_FS_READ_FILE,
        LANDLOCK_ACCESS_FS_WRITE_FILE,
        LANDLOCK_ACCESS_FS_EXECUTE,
        LANDLOCK_ACCESS_FS_TRUNCATE,
        LANDLOCK_ACCESS_FS_IOCTL_DEV,
    ] {
        assert_eq!(
            DIRECTORY_ONLY_ACCESS_FS & right,
            0,
            "file-applicable right {right:#x} must not be directory-only",
        );
    }
    // READ_DIR is the right that triggers the EINVAL on regular files.
    assert_ne!(
        DIRECTORY_ONLY_ACCESS_FS & LANDLOCK_ACCESS_FS_READ_DIR,
        0,
        "READ_DIR must be classified as directory-only",
    );
}

#[test]
fn read_only_access_on_a_regular_file_drops_directory_only_bits() {
    // A read-only preset root that resolves to a *file* (e.g.
    // `~/.gitconfig`) must end up with only file-applicable rights;
    // the `READ_DIR` bit in `read_only_access()` would otherwise make
    // `landlock_add_rule` return EINVAL.
    let masked = read_only_access() & !DIRECTORY_ONLY_ACCESS_FS;
    assert_eq!(
        masked & LANDLOCK_ACCESS_FS_READ_DIR,
        0,
        "READ_DIR must be stripped for non-directory rules",
    );
    assert_ne!(
        masked & LANDLOCK_ACCESS_FS_READ_FILE,
        0,
        "READ_FILE must survive for non-directory rules",
    );
    assert_ne!(
        masked & LANDLOCK_ACCESS_FS_EXECUTE,
        0,
        "EXECUTE must survive for non-directory rules",
    );
}

#[test]
fn landlock_handled_access_tracks_device_ioctl_abi() {
    assert_eq!(
        landlock_handled_access(4) & LANDLOCK_ACCESS_FS_IOCTL_DEV,
        0,
        "ABI 4 kernels do not support device ioctl mediation",
    );
    assert_ne!(
        landlock_handled_access(5) & LANDLOCK_ACCESS_FS_IOCTL_DEV,
        0,
        "ABI 5+ kernels should explicitly mediate device ioctls",
    );
}

#[test]
fn proc_runtime_reads_require_restricted_yama_scope() {
    for safe in ["1", "2\n", "3"] {
        assert!(yama_scope_contains_process_reads(safe), "scope {safe}");
    }
    for unsafe_or_unknown in ["0", "", "disabled", "256"] {
        assert!(
            !yama_scope_contains_process_reads(unsafe_or_unknown),
            "scope {unsafe_or_unknown} must not grant procfs reads",
        );
    }
}

// ---- complement enumeration: how a denial is expressed without a deny rule

/// Landlock is allow-only. A denial is therefore enforced by NOT granting,
/// which means a grant containing a denied subtree must be replaced by the
/// siblings that do not lead to it. These assert the substitution, because
/// on this backend there is no deny rule to look for in a rendered profile:
/// the ABSENCE of a grant IS the enforcement, and absence is exactly what a
/// careless test reads as success.
fn tree(root: &std::path::Path, names: &[&str]) {
    for name in names {
        std::fs::create_dir_all(root.join(name)).expect("tree");
    }
}

/// Measurement, not assertion: prints what the product-default denylist
/// actually costs on THIS host, so the cap is chosen from numbers instead
/// of intuition. Run with `--nocapture`.
///
/// It asserts only that the expansion succeeded and stayed under the cap,
/// because the point is the reported figure, and a machine-specific count
/// is not something to freeze into an assertion.
#[test]
fn report_default_denylist_expansion_cost() {
    let Some(home) = crate::user_dirs::home_dir() else {
        eprintln!("[landlock-cost] no home dir on this host; nothing to measure");
        return;
    };
    let denied: Vec<PathBuf> = crate::orchestration::default_read_deny_home_paths()
        .iter()
        .map(|relative| home.join(relative))
        .collect();
    let home = home.canonicalize().unwrap_or(home);

    // No wall-clock read here on purpose: the cap is a function of the RULE
    // COUNT, not of how long the walk took, and the test harness already
    // reports per-test duration. Reading `Instant` would put a host clock
    // read in a production file for a test-only number.
    let granted = expand_around_denied(&home, &denied).expect("expand around home");

    let entries = std::fs::read_dir(&home).map(|dir| dir.count()).unwrap_or(0);
    eprintln!(
        "[landlock-cost] home={} home_entries={} denied={} expanded_rules={} cap={}",
        home.display(),
        entries,
        denied.len(),
        granted.len(),
        MAX_DENY_EXPANSION_RULES,
    );
    assert!(
        granted.len() <= MAX_DENY_EXPANSION_RULES,
        "the product default must not trip the cap on a real home: {} rules",
        granted.len()
    );
}

#[test]
fn a_root_with_no_denial_inside_it_is_granted_whole() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["a", "b"]);
    // Genuinely outside `root`. A path like `root.join("elsewhere")` would
    // be INSIDE it and would exercise the subtraction instead, which is the
    // opposite of what this test claims to check.
    let unrelated = tempfile::TempDir::new().expect("unrelated");
    let unrelated = unrelated.path().canonicalize().expect("canonical");

    let granted = expand_around_denied(&root, &[unrelated]).expect("expand");

    assert_eq!(
        granted,
        vec![root],
        "a denial that is not inside the root must cost nothing and leave it intact"
    );
}

#[test]
fn a_denied_child_is_replaced_by_its_siblings_and_never_granted() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["projects", "documents", ".ssh"]);
    let denied = root.join(".ssh");

    let granted = expand_around_denied(&root, std::slice::from_ref(&denied)).expect("expand");

    assert!(
        !granted.contains(&root),
        "the root itself must NOT be granted; granting it would include the denied \
             subtree, which is the whole failure this function exists to prevent: {granted:?}"
    );
    assert!(
        !granted.iter().any(|path| path.starts_with(&denied)),
        "no grant may lead into the denied subtree: {granted:?}"
    );
    assert!(
        granted.contains(&root.join("projects")) && granted.contains(&root.join("documents")),
        "the siblings must still be reachable, or the subtraction has silently removed \
             access the policy granted: {granted:?}"
    );
}

#[test]
fn a_nested_denial_keeps_siblings_at_every_level() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["keep-me", ".config/gh", ".config/keep-this"]);
    let denied = root.join(".config/gh");

    let granted = expand_around_denied(&root, std::slice::from_ref(&denied)).expect("expand");

    assert!(
        granted.contains(&root.join("keep-me")),
        "a sibling at the top level must survive: {granted:?}"
    );
    assert!(
        granted.contains(&root.join(".config/keep-this")),
        "a sibling INSIDE the denied path's parent must survive, which is what makes this \
             a subtraction rather than denying the whole parent: {granted:?}"
    );
    assert!(
        !granted.contains(&denied) && !granted.contains(&root.join(".config")),
        "neither the denial nor any ancestor that contains it may be granted: {granted:?}"
    );
}

#[test]
fn a_root_that_is_itself_denied_grants_nothing() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["inside"]);

    let granted = expand_around_denied(&root, std::slice::from_ref(&root)).expect("expand");

    assert!(
        granted.is_empty(),
        "a root that IS the denial must grant nothing at all: {granted:?}"
    );
}

/// An unreadable ancestor must not be GRANTED, which is the only unsafe
/// outcome here. Ending the walk grants nothing beneath it, which is
/// strictly narrower than continuing.
///
/// This used to refuse the spawn. That was over-strict in the one direction
/// that matters operationally: under the hardened conformance profile
/// `$HOME` is `/root`, unreadable to the runtime, so every spawn failed
/// while no authority was gained by failing.
#[test]
fn an_unreadable_ancestor_grants_nothing_beneath_it_and_never_itself() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["locked/.ssh", "visible/keep"]);
    let locked = root.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock dir");

    let result = expand_around_denied(&root, &[locked.join(".ssh")]);

    // Restore before asserting so a failure cannot leave an unremovable dir.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
    let granted = result.expect("an unreadable ancestor must not refuse the spawn");

    assert!(
        !granted
            .iter()
            .any(|path| path == &locked || locked.starts_with(path)),
        "granting the unreadable ancestor (or anything above it) would expose the \
             subtree we could not enumerate around: {granted:?}"
    );
    // Control: expansion still happened. Without this the assertion above
    // would also pass on an empty result produced by giving up entirely,
    // which would be a silent loss of every legitimate grant.
    assert!(
        granted.contains(&root.join("visible")),
        "a sibling outside the unreadable subtree must still be granted: {granted:?}"
    );
}

/// The live Landlock falsifier.
///
/// Every other test here checks the SUBSTITUTION — which paths we decided to
/// grant. On this backend that is not the same claim as "the denied file is
/// refused", because the enforcement is the absence of a grant, and absence
/// is exactly what a careless test reads as success. So this spawns a real
/// confined child and reads two files that differ only in whether a denial
/// covers them.
///
/// Structure mirrors the macOS live test:
/// * control: the sibling inside the same granted root is readable, so a
///   refusal cannot be explained by an absent grant;
/// * claim: the denied file is refused;
/// * revert: with `read_deny_roots` cleared the same file is readable,
///   which is what proves the refusal was the denylist.
#[test]
fn a_live_landlock_child_is_refused_a_denied_file_and_allowed_its_sibling() {
    if landlock_abi_version() == 0 {
        eprintln!("[landlock-live] SKIPPED: no Landlock on this kernel");
        return;
    }
    let home = tempfile::TempDir::new().expect("temp home");
    let home_path = home.path().canonicalize().expect("canonical home");

    let secrets = home_path.join("secrets");
    std::fs::create_dir_all(&secrets).expect("secrets dir");
    let denied_file = secrets.join("id_ed25519");
    std::fs::write(&denied_file, "NOT-A-REAL-KEY\n").expect("dummy key");
    let allowed_file = home_path.join("readable.txt");
    std::fs::write(&allowed_file, "READABLE\n").expect("control file");

    let read_under = |deny: &[String], target: &std::path::Path| -> std::io::Result<bool> {
        let policy = CapabilityPolicy {
            workspace_roots: vec![home_path.display().to_string()],
            sandbox_profile: SandboxProfile::Worktree,
            process_sandbox: crate::orchestration::ProcessSandboxPolicy {
                read_deny_roots: deny.to_vec(),
                ..Default::default()
            },
            ..CapabilityPolicy::default()
        };
        crate::orchestration::push_execution_policy(policy);
        let output = crate::stdlib::sandbox::command_output(
            "/bin/cat",
            &[target.display().to_string()],
            &crate::stdlib::sandbox::ProcessCommandConfig::default(),
        );
        // Pop before returning. Each arm must run under its OWN policy, and
        // a leaked push would leave the last arm's denial on the stack for
        // whatever runs next on this thread.
        crate::orchestration::pop_execution_policy();
        Ok(matches!(output, Ok(out) if out.status.success()))
    };

    let deny = vec![secrets.display().to_string()];

    // Control first: the grant works, so a later refusal is attributable.
    let control = read_under(&deny, &allowed_file).expect("control read");
    assert!(
        control,
        "the control file inside the same workspace root must be readable, or the denial \
             below proves nothing"
    );

    let denied = read_under(&deny, &denied_file).expect("denied read");
    assert!(
        !denied,
        "a denied file must be refused even though its parent root is granted; it was read"
    );

    // Revert the fix: same policy, same files, denial removed.
    let ungated = read_under(&[], &denied_file).expect("ungated read");
    assert!(
        ungated,
        "with the denial removed the same file must become readable, which is what proves \
             the refusal was the denylist and not an unrelated accident"
    );
    eprintln!("[landlock-live] denied refused, sibling readable, revert readable");
}

/// The defect the cost measurement caught on a real host: a denied path whose
/// ancestor does not exist must cost nothing, not refuse the spawn.
///
/// `~/.kube/config` is on the default denylist and `~/.kube` is absent on
/// most machines. Treating a missing directory as "cannot enumerate" made
/// `expand_around_denied` fail closed for every spawn on any such host,
/// which would have taken the eval fleet down while looking like a working
/// security feature.
#[test]
fn a_denial_under_a_missing_directory_costs_nothing() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    tree(&root, &["present"]);

    let granted = expand_around_denied(&root, &[root.join("absent/config")])
        .expect("a denial under a missing directory must not refuse the spawn");

    assert!(
        granted.contains(&root.join("present")),
        "the siblings must still be granted: {granted:?}"
    );
}

/// The relaxation above is bounded: only NotFound and PermissionDenied end
/// the walk. Any OTHER enumeration error still refuses the spawn, because
/// "any error means nothing to exclude" is exactly the widening this term
/// must never acquire.
///
/// A file where a directory is expected reproduces that third shape
/// (`NotADirectory`) without needing an exotic filesystem.
#[test]
fn an_unexpected_enumeration_error_still_refuses_the_spawn() {
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    std::fs::write(root.join("notadir"), b"x").expect("write file");

    let result = expand_around_denied(&root, &[root.join("notadir/inner/secret")]);

    assert!(
        result.is_err(),
        "an enumeration error that is neither missing nor forbidden must fail closed, \
             got {result:?}"
    );
}

/// An optional preset root that EXISTS but cannot be opened must be skipped,
/// not fatal.
///
/// This took down every confined command when the runtime's `$HOME` was not
/// its own: `HOME=/root` under a non-root uid makes `~/.asdf` (and friends)
/// exist, unreadable, and previously fatal. Reproduced on Linux before the fix.
#[test]
fn an_unreadable_optional_root_is_skipped_and_a_required_one_still_fails() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::TempDir::new().expect("temp");
    let root = temp.path().canonicalize().expect("canonical");
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).expect("mkdir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");

    let mut profile = LandlockProfile {
        ruleset_fd: -1,
        rules: Vec::new(),
        handled_access_fs: 0,
        read_deny_roots: Vec::new(),
    };
    let optional = push_rule_exact(
        &mut profile,
        locked.clone(),
        LANDLOCK_ACCESS_FS_READ_FILE,
        true,
    );
    let required = push_rule_exact(
        &mut profile,
        locked.clone(),
        LANDLOCK_ACCESS_FS_READ_FILE,
        false,
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");

    assert!(
        optional.is_ok(),
        "an unreadable OPTIONAL root must be skipped, not refuse the spawn: {optional:?}"
    );
    assert!(
        required.is_err(),
        "an unreadable REQUIRED root must still fail closed; something asked for it by name"
    );
}
