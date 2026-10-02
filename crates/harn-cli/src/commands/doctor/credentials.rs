//! Provider selection and credential rows for `harn doctor`.

use std::path::PathBuf;

use harn_vm::llm::ProviderCredentialStatus as Cred;
use harn_vm::llm_config;

use super::{DoctorCheck, DoctorStatus};

pub(super) fn check_provider_credentials() -> Vec<DoctorCheck> {
    let mut providers = llm_config::provider_names();
    providers.sort();

    let mut checks = Vec::new();
    let mut any_credential_path = false;
    for name in &providers {
        let Some(def) = llm_config::provider_config(name) else {
            continue;
        };
        let auth = harn_vm::llm::provider_auth_status(name);
        let envs = llm_config::auth_env_names(&def.auth_env);
        let (status, detail, fix_command) = match auth.credential_status {
            Cred::Ok => {
                any_credential_path = true;
                (DoctorStatus::Ok, "credential present".to_string(), None)
            }
            Cred::Deferred => {
                any_credential_path = true;
                (
                    DoctorStatus::Ok,
                    "credential resolution deferred to platform provider".to_string(),
                    None,
                )
            }
            Cred::NotRequired => (DoctorStatus::Skip, "no key required".to_string(), None),
            Cred::RegionUnconfigured => (
                DoctorStatus::Warn,
                "platform-managed; no region configured".to_string(),
                None,
            ),
            Cred::CredentialsUnconfigured => (
                DoctorStatus::Warn,
                "platform-managed; no credential source found".to_string(),
                None,
            ),
            status @ (Cred::Missing | Cred::NeedsUserApproval) => {
                let detail = if status == Cred::NeedsUserApproval {
                    "stored; needs a Keychain approval this process cannot show".to_string()
                } else if envs.is_empty() {
                    "credential unavailable".to_string()
                } else {
                    format!("missing: {}", envs.join(", "))
                };
                let fix = envs.first().map(|env| format!("export {env}=…"));
                (DoctorStatus::Warn, detail, fix)
            }
        };
        checks.push(DoctorCheck {
            id: format!("creds:{name}"),
            status,
            label: format!("creds:{name}"),
            detail,
            fix_command,
            docs_url: Some("https://harnlang.com/llm/providers.html".to_string()),
            blocks: Vec::new(),
        });
    }

    // Add an aggregate row that fails only when no provider has creds AND
    // ollama appears unreachable. Reachability is best-effort: we only flag
    // FAIL when the synchronous `ollama --version` probe errors. Otherwise
    // demote to WARN so users without local models still get a softer signal.
    let ollama_present = which::which("ollama").is_ok();
    let aggregate_status = if any_credential_path {
        DoctorStatus::Ok
    } else if ollama_present {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Fail
    };
    let aggregate_detail = if any_credential_path {
        "at least one provider credential path is available".to_string()
    } else if ollama_present {
        "no cloud credentials; falling back to local Ollama".to_string()
    } else {
        "no provider credentials and no local Ollama".to_string()
    };
    let aggregate_blocks: Vec<&'static str> = if aggregate_status == DoctorStatus::Fail {
        vec!["scripting"]
    } else {
        Vec::new()
    };
    let aggregate_fix = if aggregate_status == DoctorStatus::Fail {
        Some("harn models recommend && harn quickstart --non-interactive".to_string())
    } else {
        None
    };
    checks.push(DoctorCheck {
        id: "creds:any".to_string(),
        status: aggregate_status,
        label: "credentials".to_string(),
        detail: aggregate_detail,
        fix_command: aggregate_fix,
        docs_url: Some("https://harnlang.com/llm/providers.html".to_string()),
        blocks: aggregate_blocks,
    });

    checks
}

pub(super) fn check_provider_selection() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
        let config_path = PathBuf::from(&path);
        let status = if config_path.is_file() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            id: String::new(),
            status,
            label: "providers config".to_string(),
            detail: format!("HARN_PROVIDERS_CONFIG={path}"),
            ..Default::default()
        });
    }

    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        let status = if llm_config::provider_config(&provider).is_some() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            id: String::new(),
            status,
            label: "selected provider".to_string(),
            detail: format!("HARN_LLM_PROVIDER={provider}"),
            ..Default::default()
        });
    }

    checks
}
