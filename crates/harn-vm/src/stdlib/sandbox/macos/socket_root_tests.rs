//! Unix-socket grants in the rendered macOS profile: a socket root admits
//! binds beneath it and nothing over IP, and never widens file writes.

use super::tests::macos_policy_with_workspace_ops;
use super::*;

#[test]
fn unix_socket_roots_admit_sockets_under_the_root_and_nothing_over_ip() {
    let workspace = tempfile::tempdir().expect("real workspace for UserTemp projection");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
    policy.workspace_roots = vec![workspace_path.display().to_string()];
    policy.process_sandbox.unix_socket_roots = vec!["/tmp/harn-workspace".to_string()];

    let profile = render_profile(&policy);

    for operation in ["network-bind", "network-inbound", "network-outbound"] {
        assert!(
            profile.contains(&format!(
                "(allow {operation} (subpath \"/tmp/harn-workspace\"))"
            )),
            "{operation} missing:\n{profile}"
        );
        // `/tmp` resolves through `/private/tmp` at bind time; both spellings
        // must be present or the socket is refused under the alias.
        assert!(
            profile.contains(&format!(
                "(allow {operation} (subpath \"/private/tmp/harn-workspace\"))"
            )),
            "{operation} alias missing:\n{profile}"
        );
    }
    assert!(!profile.contains("(allow network*)"), "{profile}");
    assert!(!profile.contains("localhost:*"), "{profile}");
    // Default presets include UserTemp, so a non-empty grant also admits
    // sockets under the session temp dir, and never the shared temp dirs.
    let session_temp = workspace_path.join(".harn-tmp");
    assert!(
        session_temp.is_dir(),
        "rendering must provision session temp"
    );
    assert!(
        profile.contains(&format!(
            "(allow network-bind (subpath \"{}\"))",
            session_temp.display()
        )),
        "UserTemp pairing missing:\n{profile}"
    );
    assert!(
        !profile.contains("(allow network-bind (subpath \"/tmp\"))"),
        "the shared temp dir must not take sockets:\n{profile}"
    );
}

/// Without a socket grant, only a child that may write the workspace binds in
/// its temp dir; a read-only run sharing that workspace gets no IPC path.
#[test]
fn the_session_temp_takes_sockets_only_with_an_explicit_grant() {
    let workspace = tempfile::tempdir().expect("real workspace for UserTemp projection");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let session_bind = format!(
        "(allow network-bind (subpath \"{}\"))",
        workspace_path.join(".harn-tmp").display()
    );
    let elsewhere = tempfile::tempdir().expect("an unrelated socket root");
    for granted in [false, true] {
        let mut policy = macos_policy_with_workspace_ops(&["read_text", "write_text"]);
        policy.workspace_roots = vec![workspace_path.display().to_string()];
        policy.side_effect_level = Some("workspace_write".to_string());
        if granted {
            policy.process_sandbox.unix_socket_roots = vec![elsewhere.path().display().to_string()];
        }
        // Write access alone must not open the shared temp dir to sockets.
        assert!(super::super::policy_allows_child_writes(&policy), "premise");
        let profile = render_profile(&policy);
        assert_eq!(
            profile.contains(&session_bind),
            granted,
            "granted={granted}:\n{profile}"
        );
    }
}

#[test]
fn a_socket_root_is_a_subpath_filter_never_a_broad_socket_grant() {
    let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
    policy.process_sandbox.unix_socket_roots = vec!["/tmp/harn-workspace".to_string()];

    let profile = render_profile(&policy);

    for line in profile.lines().filter(|line| line.contains("network-")) {
        assert!(
            line.contains("(subpath \"") || line.starts_with(";;"),
            "unscoped socket rule: {line}"
        );
    }
}

#[test]
fn a_workspace_socket_bind_succeeds_under_the_grant_and_fails_outside_it() {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let elsewhere_path = elsewhere
        .path()
        .canonicalize()
        .expect("canonical elsewhere");

    let mut policy = macos_policy_with_workspace_ops(&["read_text", "write_text"]);
    policy.workspace_roots = vec![workspace_path.display().to_string()];
    policy.process_sandbox.write_roots = vec![elsewhere_path.display().to_string()];
    policy.process_sandbox.unix_socket_roots = vec![workspace_path.display().to_string()];
    policy.process_sandbox.presets = Some(vec![
        crate::orchestration::ProcessSandboxPreset::SystemRuntime,
        crate::orchestration::ProcessSandboxPreset::DeveloperToolchains,
    ]);
    let profile = render_profile(&policy);
    let profile_file = workspace_path.join("profile.sb");
    std::fs::write(&profile_file, &profile).expect("write profile");

    // perl is a real binary on macOS; /usr/bin/python3 is an xcrun shim that
    // needs a cache under /var/folders the profile does not grant.
    let bind = |socket: &Path| -> std::process::Output {
        std::process::Command::new(SANDBOX_EXEC_PATH)
            .arg("-f")
            .arg(&profile_file)
            .arg("/usr/bin/perl")
            .arg("-MSocket")
            .arg("-e")
            .arg(
                "socket(S, PF_UNIX, SOCK_STREAM, 0) or die \"socket: $!\"; \
                 bind(S, sockaddr_un($ARGV[0])) or die \"bind: $!\"; print \"bound\\n\"",
            )
            .arg(socket)
            .current_dir(&workspace_path)
            .output()
            .expect("spawn sandbox-exec")
    };

    let inside = bind(&workspace_path.join("build.sock"));
    assert!(
        inside.status.success(),
        "socket under the granted root must bind:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&inside.stdout),
        String::from_utf8_lossy(&inside.stderr)
    );

    // Writable, but not a socket root: the write grant alone must not admit
    // the bind, or "a socket file is a file" would have widened silently.
    let outside = bind(&elsewhere_path.join("build.sock"));
    assert!(
        !outside.status.success(),
        "socket outside every socket root must be refused:\nstdout={}",
        String::from_utf8_lossy(&outside.stdout)
    );
    let stderr = String::from_utf8_lossy(&outside.stderr);
    assert!(stderr.contains("bind: Operation not permitted"), "{stderr}");
}

/// A socket root outside every writable root takes a socket file and nothing
/// else.
///
/// Binding creates the socket file, a file write the bind rule does not
/// carry, so a root that was only a socket root refused every bind with a bare
/// EPERM. The regular-file leg keeps the fix from turning the root into a
/// writable directory.
#[test]
fn a_socket_root_outside_every_writable_root_takes_a_socket_and_no_regular_file() {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return;
    }
    // Under the home directory: the default presets grant the shared temp
    // tree, and a root there would bind through that grant instead.
    let home = std::env::var_os("HOME").expect("HOME");
    let root = tempfile::Builder::new()
        .prefix(".harn-socket-root-")
        .tempdir_in(home)
        .expect("socket root");
    let sockets = root.path().canonicalize().expect("canonical socket root");
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");

    let mut policy = macos_policy_with_workspace_ops(&["read_text", "write_text"]);
    policy.workspace_roots = vec![workspace_path.display().to_string()];
    policy.process_sandbox.unix_socket_roots = vec![sockets.display().to_string()];
    let profile_file = workspace_path.join("profile.sb");
    std::fs::write(&profile_file, render_profile(&policy)).expect("write profile");
    let run = |program: &str, args: &[&str]| -> std::process::Output {
        std::process::Command::new(SANDBOX_EXEC_PATH)
            .arg("-f")
            .arg(&profile_file)
            .arg(program)
            .args(args)
            .current_dir(&workspace_path)
            .output()
            .expect("spawn sandbox-exec")
    };

    let socket = sockets.join("build.sock");
    let bound = run(
        "/usr/bin/perl",
        &[
            "-MSocket",
            "-e",
            "socket(S, PF_UNIX, SOCK_STREAM, 0) or die \"socket: $!\"; \
             bind(S, sockaddr_un($ARGV[0])) or die \"bind: $!\";",
            &socket.display().to_string(),
        ],
    );
    assert!(
        bound.status.success() && socket.exists(),
        "a socket under a socket root outside every writable root must bind: stderr={}",
        String::from_utf8_lossy(&bound.stderr)
    );

    let regular = sockets.join("regular.txt");
    let touched = run("/usr/bin/touch", &[&regular.display().to_string()]);
    assert!(
        !touched.status.success() && !regular.exists(),
        "a socket root must not admit a regular file"
    );
}
