//! Private, bounded reads from known CLI credential stores. This is a secret
//! source, never an arbitrary command surface or a command-output artifact.

use super::{VmError, VmValue};
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

    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("gh");
        command.args(["auth", "token", "--hostname", &self.hostname]);
        if let Some(user) = &self.user {
            command.args(["--user", user]);
        }
        command.env_clear();
        for name in PRIVATE_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }
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
    capture(request.command(), DEADLINE).await
}

async fn capture(
    mut command: tokio::process::Command,
    deadline: Duration,
) -> Result<String, VmError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| unavailable())?;
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
        let status = child.wait().await.map_err(|_| unavailable())?;
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
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(unavailable())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let command = request.command();
        let command = command.as_std();
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "auth",
                "token",
                "--hostname",
                "git.example.com",
                "--user",
                "octo-bot"
            ]
        );
        assert!(command
            .get_envs()
            .all(|(key, _)| PRIVATE_ENVIRONMENT.iter().any(|name| key == *name)));
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
