//! Private, bounded reads from known CLI credential stores. This is a secret
//! source, never an arbitrary command surface or a command-output artifact.

use super::{VmError, VmValue};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

const MAX_CREDENTIAL_BYTES: u64 = 8192;
const DEADLINE: Duration = Duration::from_secs(10);
const PRIVATE_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "GH_CONFIG_DIR",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "WINDIR",
];

struct Request {
    hostname: String,
    user: Option<String>,
}

fn invalid() -> VmError {
    VmError::TypeError(
        "secrets.read_cli expects provider github and optional hostname/user strings".into(),
    )
}

fn unavailable() -> VmError {
    VmError::Thrown(super::vm_string(
        "The requested CLI credential is unavailable.",
    ))
}

impl Request {
    fn parse(args: &[VmValue]) -> Result<Self, VmError> {
        if args.is_empty()
            || args.len() > 2
            || !matches!(&args[0], VmValue::String(provider) if provider.as_str() == "github")
        {
            return Err(invalid());
        }
        let options = match args.get(1) {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::Dict(options)) => Some(options),
            _ => return Err(invalid()),
        };
        if options.is_some_and(|options| {
            options
                .keys()
                .any(|key| key.as_str() != "hostname" && key.as_str() != "user")
        }) {
            return Err(invalid());
        }
        let hostname = options.and_then(|options| options.get("hostname"));
        let hostname = match hostname {
            None | Some(VmValue::Nil) => "github.com",
            Some(VmValue::String(host)) => host.as_str(),
            _ => return Err(invalid()),
        };
        if hostname.is_empty()
            || hostname.len() > 253
            || !hostname
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
            || !hostname.as_bytes()[0].is_ascii_alphanumeric()
        {
            return Err(invalid());
        }
        let user = options.and_then(|options| options.get("user"));
        let user = match user {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::String(user)) if user.is_empty() => None,
            Some(VmValue::String(user))
                if user.len() <= 100
                    && user.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && user.as_bytes()[0].is_ascii_alphanumeric() =>
            {
                Some(user.to_string())
            }
            _ => return Err(invalid()),
        };
        Ok(Self {
            hostname: hostname.into(),
            user,
        })
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = vec![
            "auth".into(),
            "token".into(),
            "--hostname".into(),
            self.hostname.clone(),
        ];
        if let Some(user) = &self.user {
            args.extend(["--user".into(), user.clone()]);
        }
        args
    }

    fn command(&self) -> Result<tokio::process::Command, VmError> {
        let program = trusted_github_cli().ok_or_else(unavailable)?;
        let mut command = crate::process_sandbox::tokio_command_for(
            program.to_str().ok_or_else(unavailable)?,
            &self.arguments(),
        )
        .map_err(|_| unavailable())?;
        let cwd = crate::stdlib::process::inherited_process_cwd().map_err(|_| unavailable())?;
        crate::process_sandbox::enforce_process_cwd(&cwd).map_err(|_| unavailable())?;
        command.current_dir(cwd);
        command.env_clear();
        for name in PRIVATE_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        crate::process_sandbox::validate_command_environment(command.as_std_mut(), true)
            .map_err(|_| unavailable())?;
        Ok(command)
    }
}

/// This secret source never searches workspace-controlled PATH entries.
/// Package-manager symlinks must resolve inside the corresponding installation.
fn trusted_github_cli() -> Option<PathBuf> {
    installed_github_cli_locations()
        .into_iter()
        .find_map(|(path, root)| trusted_installed_binary(&path, &root))
}

#[cfg(not(windows))]
fn installed_github_cli_locations() -> Vec<(PathBuf, PathBuf)> {
    vec![
        (PathBuf::from("/usr/bin/gh"), PathBuf::from("/usr/bin")),
        (
            PathBuf::from("/usr/local/bin/gh"),
            PathBuf::from("/usr/local"),
        ),
        (
            PathBuf::from("/opt/homebrew/bin/gh"),
            PathBuf::from("/opt/homebrew"),
        ),
    ]
}

#[cfg(windows)]
fn installed_github_cli_locations() -> Vec<(PathBuf, PathBuf)> {
    use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86};
    [FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86]
        .into_iter()
        .filter_map(|id| windows_known_install_root(&id))
        .map(|root| {
            let root = root.join("GitHub CLI");
            (root.join("gh.exe"), root)
        })
        .collect()
}

#[cfg(windows)]
fn windows_known_install_root(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT_PATH};
    let mut pointer = std::ptr::null_mut();
    // Read the OS's default installer directory, never ProgramFiles from the
    // caller's environment or a user-redirected known-folder path.
    let status = unsafe {
        SHGetKnownFolderPath(
            id,
            KF_FLAG_DEFAULT_PATH as u32,
            std::ptr::null_mut(),
            &mut pointer,
        )
    };
    let result = if status >= 0 && !pointer.is_null() {
        let mut length = 0;
        while length < 32768 && unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        (length > 0 && length < 32768).then(|| {
            PathBuf::from(OsString::from_wide(unsafe {
                std::slice::from_raw_parts(pointer, length)
            }))
        })
    } else {
        None
    };
    unsafe { CoTaskMemFree(pointer.cast()) };
    result
}

fn trusted_installed_binary(path: &Path, root: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let resolved = path.canonicalize().ok()?;
    if !resolved.starts_with(root) {
        return None;
    }
    let metadata = resolved.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return None;
        }
    }
    Some(resolved)
}

pub(super) async fn read(args: &[VmValue]) -> Result<String, VmError> {
    if crate::harness_tenant::current_tenant_id().is_some()
        || crate::harness_auth::current_auth_principal().is_some()
        || crate::connectors::harn_module::active_harn_connector_ctx().is_some()
    {
        return Err(VmError::CategorizedError {
            message: "CLI credentials are available only to an unscoped local host.".into(),
            category: super::ErrorCategory::Auth,
        });
    }
    let request = Request::parse(args)?;
    let name = format!(
        "github/cli/{}/{}",
        request.hostname,
        request.user.as_deref().unwrap_or("active")
    );
    let id = super::secret_id_for_scope(&name, &super::secret_scope_arg(None)?)?;
    crate::secrets::ensure_scoped_secret_access_allowed("read", &id)
        .map_err(super::secret_error_to_vm)?;
    let value = capture(request.command()?, DEADLINE).await?;
    crate::secrets::emit_secret_access_event("github_cli", &id);
    Ok(value)
}

async fn capture(
    mut command: tokio::process::Command,
    deadline: Duration,
) -> Result<String, VmError> {
    crate::op_interrupt::configure_tokio_kill_group(&mut command);
    #[cfg(windows)]
    let job = {
        crate::op_interrupt::windows::configure_suspended(command.as_std_mut());
        crate::op_interrupt::windows::KillOnCloseJob::new().map_err(|_| unavailable())?
    };
    let cleanup_token = crate::op_interrupt::new_process_cleanup_token();
    command.env(
        crate::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    crate::op_interrupt::preserve_process_owner_token(command.as_std_mut());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| unavailable())?;
    #[cfg(windows)]
    {
        let pid = child.id().ok_or_else(unavailable)?;
        job.assign_process(pid).map_err(|_| unavailable())?;
        crate::op_interrupt::windows::resume_process(pid).map_err(|_| unavailable())?;
    }
    let registration =
        crate::op_interrupt::register_active_process_cleanup(child.id(), &cleanup_token, None)
            .terminate_on_drop();
    if crate::op_interrupt::record_tokio_process_owner_group(&mut child, &cleanup_token)
        .await
        .is_err()
    {
        registration.update_root_pid(child.id());
        return Err(unavailable());
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(unavailable)?
        .take(MAX_CREDENTIAL_BYTES + 1);
    let result = tokio::time::timeout(deadline, async {
        let mut bytes = Zeroizing::new(Vec::new());
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| unavailable())?;
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(unavailable());
        }
        // EOF can precede the CLI's successful exit. Remove descendants while
        // the root still reserves its PID, then let it report its own status.
        #[cfg(unix)]
        let _ = registration.terminate_descendants();
        let status = child.wait().await;
        registration.update_root_pid(child.id());
        let status = status.map_err(|_| unavailable())?;
        if !status.success() {
            return Err(unavailable());
        }
        let value = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(unavailable());
        }
        Ok(value.to_string())
    })
    .await;
    match result {
        Ok(Ok(value)) => Ok(value),
        _ => {
            let _ = registration.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            registration.update_root_pid(child.id());
            Err(unavailable())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_identity_cannot_escape_into_a_workspace_or_writable_file() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("installed");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join("gh");
        std::fs::write(&binary, "synthetic executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            trusted_installed_binary(&binary, &install),
            Some(binary.canonicalize().unwrap())
        );
        assert!(trusted_installed_binary(&binary, &temp.path().join("workspace")).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(trusted_installed_binary(&binary, &install).is_none());
            let outside = temp.path().join("shadow-gh");
            std::fs::write(&outside, "synthetic executable").unwrap();
            std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::remove_file(&binary).unwrap();
            symlink(&outside, &binary).unwrap();
            assert!(trusted_installed_binary(&binary, &install).is_none());
        }
    }

    #[tokio::test]
    async fn private_capture_reaps_descendants_on_completion_and_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let survivor = temp.path().join("survivor");
        let ready = temp.path().join("ready");
        let descendant = format!(
            "import pathlib,time;time.sleep(2);pathlib.Path({:?}).write_text('alive')",
            survivor.to_str().unwrap()
        );
        let parent = |finish: &str| {
            format!(
            "import pathlib,subprocess,sys,time;child=subprocess.Popen([sys.executable,'-c',{:?}],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);pathlib.Path({:?}).write_text(str(child.pid));print('synthetic-cli-token',flush=True);{}",
            descendant, ready.to_str().unwrap(), finish
        )
        };
        let mut control = tokio::process::Command::new("python3");
        control.args(["-c", &parent("pass")]);
        assert!(control.status().await.unwrap().success());
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            survivor.exists(),
            "the known unmanaged child must reach the same sentinel path"
        );
        for (finish, refuses) in [
            ("pass", false),
            (
                "sys.stdout.close();__import__('os').close(1);time.sleep(0.4)",
                false,
            ),
            ("time.sleep(20)", true),
        ] {
            std::fs::remove_file(&survivor).ok();
            std::fs::remove_file(&ready).ok();
            let mut command = tokio::process::Command::new("python3");
            command.args(["-c", &parent(finish)]);
            let result = capture(command, Duration::from_secs(1)).await;
            assert_eq!(result.is_err(), refuses);
            assert!(
                ready.exists(),
                "the negative must have spawned its descendant"
            );
            tokio::time::sleep(Duration::from_millis(2500)).await;
            assert!(
                !survivor.exists(),
                "no credential helper survives the private capture"
            );
        }
    }

    #[tokio::test]
    async fn private_capture_accepts_only_one_complete_credential() {
        let mut success = tokio::process::Command::new("python3");
        success.args(["-c", "print('synthetic-cli-token')"]);
        assert_eq!(
            capture(success, DEADLINE).await.unwrap(),
            "synthetic-cli-token"
        );
        for program in [
            "print('')",
            "print('synthetic-cli-token');raise SystemExit(1)",
            "print('x'*8193)",
            "print('two tokens')",
        ] {
            let mut command = tokio::process::Command::new("python3");
            command.args(["-c", program]);
            assert!(capture(command, DEADLINE).await.is_err());
        }
        let mut slow = tokio::process::Command::new("python3");
        slow.args(["-c", "import time;time.sleep(20)"]);
        assert!(capture(slow, Duration::from_millis(50)).await.is_err());
    }

    #[test]
    fn provider_is_closed_and_account_selection_has_no_ambient_tokens() {
        let request = Request {
            hostname: "git.example.com".into(),
            user: Some("octo-bot".into()),
        };
        assert_eq!(
            request.arguments(),
            [
                "auth",
                "token",
                "--hostname",
                "git.example.com",
                "--user",
                "octo-bot"
            ]
        );
        assert!(!PRIVATE_ENVIRONMENT
            .iter()
            .any(|name| *name == "GH_TOKEN" || *name == "GITHUB_TOKEN"));
        assert!(Request::parse(&[super::super::vm_string("shell")]).is_err());
    }

    #[tokio::test]
    async fn authenticated_dispatch_cannot_read_the_hosts_cli_credential() {
        let _scope =
            crate::harness_auth::enter_auth_principal(crate::harness_auth::AuthPrincipal {
                subject: "tenant-caller".into(),
                scheme: "bearer".into(),
                scopes: Default::default(),
                kind: Some("tenant".into()),
            });
        let error = read(&[super::super::vm_string("github")])
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            VmError::CategorizedError {
                category: super::super::ErrorCategory::Auth,
                ..
            }
        ));
    }
}
#[cfg(windows)]
#[test]
fn windows_installer_roots_ignore_caller_environment() {
    const PROBE: &str = "CLI_TEST_INSTALL_ROOT";
    if let Some(hostile) = std::env::var_os(PROBE) {
        let hostile = PathBuf::from(hostile);
        let locations = installed_github_cli_locations();
        assert!(
            !locations.is_empty(),
            "calibrate a real non-null OS installer root"
        );
        assert!(locations
            .iter()
            .all(|(path, _)| !path.starts_with(&hostile)));
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("GitHub CLI");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("gh.exe"), "synthetic hostile executable").unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "harness_methods::cli_credential::tests::windows_installer_roots_ignore_caller_environment", "--nocapture"])
            .env("ProgramFiles", temp.path())
            .env("ProgramFiles(x86)", temp.path())
            .env(PROBE, temp.path())
            .status().unwrap();
    assert!(status.success());
}
