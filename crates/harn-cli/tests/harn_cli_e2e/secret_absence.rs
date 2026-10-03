//! A missing secret must name the provider chain that was consulted.
//!
//! `HARN_SECRET_PROVIDERS=env` is a common agent-session setting. Under it a
//! credential stored in the OS keyring is unreachable, and the read has to say
//! so rather than reading as "never stored".
use std::fs;
use std::path::Path;

use crate::test_util::process::harn_e2e_command;

const PROGRAM: &str = r#"import { client } from "std/oauth/client"
import { secrets } from "std/oauth/storage"

fn main(harness: Harness) {
  try {
    const _ = harness.secrets.read("acme_probe/oauth-token")
    harness.stdio.println("read: present")
  } catch (err) {
    harness.stdio.println("read: " + to_string(err?.category) + " | " + to_string(err?.message))
  }
  const store = secrets(harness.auth, harness.secrets, {provider: "acme_probe"})
  harness.stdio.println("get: " + to_string(store.get("acme_probe")))
  const cli = client(
    harness,
    {
      id: "acme_probe",
      auth_url: "https://auth.invalid/authorize",
      token_url: "https://auth.invalid/token",
    },
    {client_id: "synthetic-client", storage: store},
  )
  try {
    const _ = cli.token()
    harness.stdio.println("token: present")
  } catch (err) {
    harness.stdio.println("token: " + to_string(err?.message ?? err))
  }
}
"#;

fn run(directory: &Path, chain: &str) -> String {
    let script = directory.join("probe.harn");
    fs::write(&script, PROGRAM).unwrap();
    let output = harn_e2e_command()
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory)
        .env("HARN_SECRET_PROVIDERS", chain)
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .current_dir(directory)
        .args(["run", "--environment-policy", "isolated"])
        .arg(&script)
        .output()
        .expect("run native Harn");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn an_empty_chain_is_a_configuration_error_not_an_absent_secret() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("empty.harn");
    fs::write(
        &script,
        r#"fn main(harness: Harness) {
  try {
    const _ = harness.secrets.read("acme_probe/oauth-token")
  } catch (err) {
    harness.stdio.println("read: " + to_string(err?.category) + " | " + to_string(err?.message))
  }
}
"#,
    )
    .unwrap();
    let output = harn_e2e_command()
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.path())
        .env("HARN_SECRET_PROVIDERS", "")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .current_dir(directory.path())
        .args(["run", "--environment-policy", "isolated"])
        .arg(&script)
        .output()
        .expect("run native Harn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("read: tool_error | no secret providers configured"),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn env_only_chain_names_the_consulted_provider_and_the_disabled_keyring() {
    let directory = tempfile::tempdir().unwrap();
    let stdout = run(directory.path(), "env");

    let absence = "secret 'acme_probe/oauth-token' not found in providers: \
                   env (HARN_SECRET_ACME_PROBE_OAUTH_TOKEN); \
                   keyring disabled by HARN_SECRET_PROVIDERS=env";
    // Absence across the whole chain is `not_found`, not a tool failure.
    assert!(
        stdout.contains(&format!("read: not_found | {absence}")),
        "{stdout}"
    );
    // std/oauth storage treats that absence as "nothing stored" ...
    assert!(stdout.contains("get: nil"), "{stdout}");
    // ... and the client's diagnostic carries the chain detail.
    assert!(
        stdout.contains(&format!(
            "token: HARN-OAU-002: std/oauth/client: no token in storage; \
             run authorization first ({absence})"
        )),
        "{stdout}"
    );
}
