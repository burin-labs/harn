use harn_vm::secrets::{
    configured_default_chain, configured_secret_namespace, EnvSecretProvider,
    KeyringSecretProvider, NativeKeyringAvailability, NativeKeyringError, SecretChainPlan,
    SecretId, SECRET_FILE_PATH_ENV,
};

use super::{DoctorCheck, DoctorStatus};

/// Reports the configured secret provider chain, each provider's health, and
/// every default provider the chain leaves out (a credential stored there
/// reads as missing). `check_keyring_write` adds a keyring write, read, and
/// delete to the default non-interactive keyring check.
pub(super) fn check_secret_providers(check_keyring_write: bool) -> Vec<DoctorCheck> {
    let namespace = configured_secret_namespace();
    let plan = SecretChainPlan::configured();
    let mut checks = Vec::new();

    match configured_default_chain(namespace.clone()) {
        Ok(chain) => checks.push(DoctorCheck {
            id: String::new(),
            status: if chain.providers().is_empty() {
                DoctorStatus::Fail
            } else {
                DoctorStatus::Ok
            },
            label: "secret providers".to_string(),
            detail: format!(
                "{} (namespace {})",
                if plan.providers.is_empty() {
                    "(none)".to_string()
                } else {
                    plan.providers.join(" -> ")
                },
                namespace
            ),
            ..Default::default()
        }),
        Err(error) => {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: "secret providers".to_string(),
                detail: error.to_string(),
                ..Default::default()
            });
            return checks;
        }
    }

    // A default provider left out of an explicit chain is invisible at read
    // time: a credential stored there reads as missing. Say so here.
    for excluded in &plan.excluded {
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Warn,
            label: format!("secret:{}", excluded.provider),
            detail: format!(
                "not consulted: {}; secrets stored there read as missing",
                excluded.reason
            ),
            ..Default::default()
        });
    }

    for provider in plan.providers.iter().map(String::as_str) {
        match provider {
            "file" => checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Ok,
                label: "secret:file".to_string(),
                detail: std::env::var(SECRET_FILE_PATH_ENV)
                    .map(|path| format!("reads {path}"))
                    .unwrap_or_else(|_| format!("reads the path in {SECRET_FILE_PATH_ENV}")),
                ..Default::default()
            }),
            "env" => {
                let env_provider = EnvSecretProvider::new(namespace.clone());
                let sample = env_provider.env_var_name(&SecretId::new("sample", "token"));
                checks.push(DoctorCheck {
                    id: String::new(),
                    status: DoctorStatus::Ok,
                    label: "secret:env".to_string(),
                    detail: format!("reads process env via {sample}"),
                    ..Default::default()
                });
            }
            "keyring" => checks.push(check_keyring(
                &KeyringSecretProvider::new(namespace.clone()),
                check_keyring_write,
            )),
            other => checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: format!("secret:{other}"),
                detail: format!("unsupported provider '{other}'"),
                ..Default::default()
            }),
        }
    }

    checks
}

/// The `secret:keyring` check: whether the store is available, locked, or
/// unavailable, asked without raising a prompt; with `check_write`, also a
/// write, read, and delete of a probe credential.
fn check_keyring(provider: &KeyringSecretProvider, check_write: bool) -> DoctorCheck {
    let service = provider.service();
    let (status, detail) = match provider.availability() {
        Ok(NativeKeyringAvailability::Available) if check_write => {
            match provider.verify_round_trip() {
                Ok(detail) => (DoctorStatus::Ok, detail),
                Err(error) => (DoctorStatus::Fail, unavailable_detail(&error)),
            }
        }
        Ok(NativeKeyringAvailability::Available) => (
            DoctorStatus::Ok,
            format!(
                "service '{service}' is available; pass --check-keyring-write to also \
                 write, read, and delete a probe credential"
            ),
        ),
        Ok(NativeKeyringAvailability::Locked) => (
            DoctorStatus::Warn,
            format!(
                "service '{service}' is locked; reading a credential needs an unlock \
                 prompt, which a process without a terminal does not raise. Unlock the \
                 keyring, or provide credentials through environment variables"
            ),
        ),
        Err(error) => (DoctorStatus::Fail, unavailable_detail(&error)),
    };
    DoctorCheck {
        id: String::new(),
        status,
        label: "secret:keyring".to_string(),
        detail,
        ..Default::default()
    }
}

fn unavailable_detail(error: &NativeKeyringError) -> String {
    match error.unavailable_reason() {
        Some(reason) => format!("unavailable ({reason}): {error}"),
        None => error.to_string(),
    }
}
