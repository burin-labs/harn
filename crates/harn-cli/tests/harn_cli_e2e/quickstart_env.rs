use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::test_util::process::harn_e2e_command;

fn git(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command.env_clear();
    restore_os_environment(&mut command);
    command
        .current_dir(root)
        .args(["-c", "core.excludesFile=", "-c", "init.templateDir="])
        .args(args)
        .output()
        .unwrap()
}

fn restore_os_environment(command: &mut Command) {
    for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn quickstart(root: &Path) -> Output {
    let mut command = harn_e2e_command();
    command.env_clear();
    restore_os_environment(&mut command);
    let home = root.join("home");
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("APPDATA", home.join("config"))
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env("HTTP_PROXY", "http://127.0.0.1:0")
        .env("http_proxy", "http://127.0.0.1:0")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .args([
            "quickstart",
            "--non-interactive",
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-4",
        ])
        .output()
        .unwrap()
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(unix)]
#[test]
fn quickstart_printed_shell_step_activates_credentials_for_doctor() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(".env"),
        "ANTHROPIC_API_KEY=synthetic-activation-fixture\n",
    )
    .unwrap();
    let setup = quickstart(root.path());
    assert!(setup.status.success(), "{}", output_text(&setup));
    let stdout = String::from_utf8(setup.stdout).unwrap();
    let next_steps: Vec<_> = stdout
        .lines()
        .skip_while(|line| *line != "Next:")
        .skip(1)
        .collect();
    assert_eq!(next_steps.len(), 2, "{stdout}");
    let script = root.path().join("activate.sh");
    fs::write(
        &script,
        format!(
            "{}\nexec \"$HARN_ACTIVATION_TEST_BIN\" doctor --no-network --json\n",
            next_steps[0].trim()
        ),
    )
    .unwrap();
    let mut shell = Command::new("/bin/sh");
    shell.env_clear();
    restore_os_environment(&mut shell);
    let home = root.path().join("home");
    let binary = harn_e2e_command().get_program().to_os_string();
    let doctor = shell
        .current_dir(root.path())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env("HARN_ACTIVATION_TEST_BIN", binary)
        .arg(script)
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", output_text(&doctor)));
    let checks = report["data"]["checks"].as_array().unwrap();
    let credentials: Vec<_> = checks
        .iter()
        .filter(|check| check["id"] == "creds:anthropic")
        .collect();
    assert_eq!(credentials.len(), 1, "{report}");
    assert_eq!(credentials[0]["status"], "ok", "{report}");
    assert!(!output_text(&doctor).contains("synthetic-activation-fixture"));
}

#[test]
fn repeated_quickstart_preserves_an_inactive_credential_placeholder() {
    let root = tempfile::tempdir().unwrap();
    let output = quickstart(root.path());
    assert!(output.status.success(), "{}", output_text(&output));
    let environment = fs::read_to_string(root.path().join(".env")).unwrap();
    assert_eq!(
        environment
            .lines()
            .filter(|line| *line == "# ANTHROPIC_API_KEY=replace-me")
            .count(),
        1
    );
    assert!(!environment
        .lines()
        .any(|line| line.starts_with("ANTHROPIC_API_KEY=")));
    let snapshots: Vec<_> = [
        ".env",
        ".gitignore",
        "harn.toml",
        "home/config/harn/providers.toml",
    ]
    .into_iter()
    .map(|path| (path, fs::read(root.path().join(path)).unwrap()))
    .collect();
    for _ in 0..2 {
        let output = quickstart(root.path());
        assert!(output.status.success(), "{}", output_text(&output));
        for (path, expected) in &snapshots {
            assert_eq!(
                fs::read(root.path().join(path)).unwrap().as_slice(),
                expected.as_slice(),
                "repeated quickstart changed {path}"
            );
        }
    }
}

#[test]
fn quickstart_protects_existing_env_and_preserves_authored_ignore_rules() {
    let root = tempfile::tempdir().unwrap();
    assert!(git(root.path(), &["init", "--quiet"]).status.success());
    let authored = b"# project rules\n/build/\n!.env";
    fs::write(root.path().join(".gitignore"), authored).unwrap();
    fs::write(root.path().join(".env.example"), "EXAMPLE=public\n").unwrap();
    let env_file = root.path().join(".env");
    fs::write(&env_file, "ANTHROPIC_API_KEY=synthetic-fixture-value\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&env_file, fs::Permissions::from_mode(0o644)).unwrap();
    }
    // Known nonempty index data proves the tracked-path query works before its zero result.
    assert!(git(root.path(), &["add", ".env.example"]).status.success());
    assert!(!git(root.path(), &["ls-files", "-z"]).stdout.is_empty());
    let output = quickstart(root.path());
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(!output_text(&output).contains("synthetic-fixture-value"));
    let env = fs::read(&env_file).unwrap();
    assert!(env.starts_with(b"ANTHROPIC_API_KEY=synthetic-fixture-value\n"));
    let ignore = fs::read(root.path().join(".gitignore")).unwrap();
    assert_eq!(ignore, [authored.as_slice(), b"\n/.env\n"].concat());
    assert!(git(root.path(), &["check-ignore", "--", ".env"])
        .status
        .success());
    assert_eq!(
        git(
            root.path(),
            &["check-ignore", "--no-index", "--", ".env.example"]
        )
        .status
        .code(),
        Some(1)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let output = quickstart(root.path());
    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(fs::read(&env_file).unwrap(), env);
    assert_eq!(fs::read(root.path().join(".gitignore")).unwrap(), ignore);
}

#[test]
fn quickstart_refuses_tracked_env_and_unreadable_repository_before_writing() {
    for tracked_name in [".env", ".ENV"] {
        let root = tempfile::tempdir().unwrap();
        assert!(git(root.path(), &["init", "--quiet"]).status.success());
        let authored = b"PROJECT_VALUE=preserve\n";
        fs::write(root.path().join(tracked_name), authored).unwrap();
        let env_existed = root.path().join(".env").exists();
        assert!(git(root.path(), &["add", tracked_name]).status.success());
        let output = quickstart(root.path());
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(output_text(&output).contains("Git already tracks"));
        assert_eq!(fs::read(root.path().join(tracked_name)).unwrap(), authored);
        assert_eq!(root.path().join(".env").exists(), env_existed);
        assert!(!root.path().join(".gitignore").exists());
        assert!(!root.path().join("harn.toml").exists());
        assert!(!root.path().join("home/config/harn/providers.toml").exists());
    }

    let broken = tempfile::tempdir().unwrap();
    fs::write(broken.path().join(".git"), "gitdir: missing-worktree\n").unwrap();
    let output = quickstart(broken.path());
    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).contains("cannot query Git's index"));
    assert!(!broken.path().join(".env").exists());
    assert!(!broken.path().join(".gitignore").exists());
    assert!(!broken.path().join("harn.toml").exists());
}

#[cfg(unix)]
#[test]
fn quickstart_refuses_symlink_destinations_without_replacing_authored_files() {
    for name in [".env", ".gitignore"] {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("authored");
        fs::write(&target, "keep this file\n").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join(name)).unwrap();
        let output = quickstart(root.path());
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(output_text(&output).contains("must be a regular file"));
        assert_eq!(fs::read_to_string(target).unwrap(), "keep this file\n");
        assert!(fs::symlink_metadata(root.path().join(name))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!root.path().join("harn.toml").exists());
    }
}
