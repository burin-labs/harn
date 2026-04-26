//! `harn dump-trigger-quickref` — regenerate the LLM trigger quickref.
//!
//! The quickref is intentionally generated from the runtime provider catalog
//! so connector docs do not drift from `std/triggers::list_providers()`.

use std::fs;
use std::path::Path;
use std::process;

use harn_vm::{
    registered_provider_metadata, ProviderMetadata, ProviderRuntimeMetadata,
    SignatureVerificationMetadata,
};

struct FirstPartyConnectorPackage {
    provider: &'static str,
    package_url: &'static str,
    install: &'static str,
    contract_check: &'static str,
}

const FIRST_PARTY_CONNECTOR_PACKAGES: &[FirstPartyConnectorPackage] = &[
    FirstPartyConnectorPackage {
        provider: "GitHub",
        package_url: "https://github.com/burin-labs/harn-github-connector",
        install: "harn add github.com/burin-labs/harn-github-connector@v0.1.0",
        contract_check: "harn connector check . --provider github",
    },
    FirstPartyConnectorPackage {
        provider: "Slack",
        package_url: "https://github.com/burin-labs/harn-slack-connector",
        install: "harn add github.com/burin-labs/harn-slack-connector@v0.1.0",
        contract_check: "harn connector check . --provider slack",
    },
    FirstPartyConnectorPackage {
        provider: "Linear",
        package_url: "https://github.com/burin-labs/harn-linear-connector",
        install: "harn add github.com/burin-labs/harn-linear-connector@v0.1.0",
        contract_check: "harn connector check . --provider linear",
    },
    FirstPartyConnectorPackage {
        provider: "Notion",
        package_url: "https://github.com/burin-labs/harn-notion-connector",
        install: "harn add github.com/burin-labs/harn-notion-connector@v0.1.0",
        contract_check: "harn connector check . --provider notion --run-poll-tick",
    },
    FirstPartyConnectorPackage {
        provider: "GitLab",
        package_url: "https://github.com/burin-labs/harn-gitlab-connector",
        install: "harn add github.com/burin-labs/harn-gitlab-connector@v0.1.0",
        contract_check: "harn connector check . --provider gitlab",
    },
    FirstPartyConnectorPackage {
        provider: "Forgejo",
        package_url: "https://github.com/burin-labs/harn-forgejo-connector",
        install: "harn add github.com/burin-labs/harn-forgejo-connector@v0.1.0",
        contract_check: "harn connector check . --provider forgejo",
    },
    FirstPartyConnectorPackage {
        provider: "Gitea",
        package_url: "https://github.com/burin-labs/harn-gitea-connector",
        install: "harn add github.com/burin-labs/harn-gitea-connector@v0.1.0",
        contract_check: "harn connector check . --provider gitea",
    },
    FirstPartyConnectorPackage {
        provider: "Bitbucket",
        package_url: "https://github.com/burin-labs/harn-bitbucket-connector",
        install: "harn add github.com/burin-labs/harn-bitbucket-connector@v0.1.0",
        contract_check: "harn connector check . --provider bitbucket",
    },
    FirstPartyConnectorPackage {
        provider: "SourceHut",
        package_url: "https://github.com/burin-labs/harn-sourcehut-connector",
        install: "harn add github.com/burin-labs/harn-sourcehut-connector@v0.1.0",
        contract_check: "harn connector check . --provider sourcehut",
    },
    FirstPartyConnectorPackage {
        provider: "Subversion",
        package_url: "https://github.com/burin-labs/harn-svn-connector",
        install: "harn add github.com/burin-labs/harn-svn-connector@v0.1.0",
        contract_check: "harn connector check . --provider svn --run-poll-tick",
    },
];

pub(crate) fn run(output_path: &str, check_only: bool) {
    let generated = generate_file();
    let path = Path::new(output_path);

    if check_only {
        let existing = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                eprintln!("hint: run `make gen-trigger-quickref` to regenerate.");
                process::exit(1);
            }
        };
        if existing != generated {
            eprintln!(
                "error: {} is stale relative to the trigger provider catalog.",
                path.display()
            );
            eprintln!("hint: run `make gen-trigger-quickref` to regenerate.");
            process::exit(1);
        }
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {e}", parent.display());
            process::exit(1);
        }
    }
    if let Err(e) = fs::write(path, &generated) {
        eprintln!("error: cannot write {}: {e}", path.display());
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

fn generate_file() -> String {
    let mut providers = registered_provider_metadata();
    providers.sort_by(|a, b| a.provider.cmp(&b.provider));

    let mut out = String::new();
    out.push_str("# Harn Trigger Quick Reference (LLM-friendly)\n\n");
    out.push_str("<!-- GENERATED by `harn dump-trigger-quickref` -- do not edit by hand. -->\n");
    out.push_str("<!-- Sources of truth: crates/harn-vm/src/triggers/event.rs ProviderCatalog metadata and connector contract v1 docs. -->\n\n");
    out.push_str("<!-- markdownlint-disable MD013 -->\n\n");
    out.push_str(
        "**Canonical URL:** <https://harnlang.com/docs/llm/harn-triggers-quickref.html>\n\n",
    );
    out.push_str("Use this with `docs/llm/harn-quickref.md` when writing trigger, connector, or orchestrator code. It covers manifest shape, provider catalog metadata, the pure-Harn connector contract, and example-library commands.\n\n");
    out.push_str("## Trigger Manifest\n\n");
    out.push_str("```toml\n");
    out.push_str("[package]\nname = \"review-bot\"\n\n");
    out.push_str("[exports]\nhandlers = \"lib.harn\"\n\n");
    out.push_str("[[triggers]]\n");
    out.push_str("id = \"github-prs\"\nkind = \"webhook\"\nprovider = \"github\"\nmatch = { path = \"/hooks/github\", events = [\"pull_request.opened\"] }\nhandler = \"handlers::on_pull_request\"\nwhen = \"handlers::should_handle\"\ndedupe_key = \"event.dedupe_key\"\nretry = { max = 7, backoff = \"svix\" }\nbudget = { daily_cost_usd = 5.00, max_concurrent = 4 }\nsecrets = { signing_secret = \"github/webhook-secret\" }\n\n");
    out.push_str("[[triggers]]\nid = \"weekday-digest\"\nkind = \"cron\"\nprovider = \"cron\"\nschedule = \"0 9 * * 1-5\"\ntimezone = \"America/Los_Angeles\"\nhandler = \"handlers::send_digest\"\n```\n\n");
    out.push_str("Key fields: `id`, `kind`, `provider`, `handler`, `match.events`, `match.path`, `when`, `dedupe_key`, `retry`, `budget`, `secrets`, `schedule`, `timezone`, and provider-specific config tables such as `poll`.\n\n");

    out.push_str("## Provider Catalog\n\n");
    out.push_str("This table is generated from `std/triggers::list_providers()` / `ProviderCatalog` metadata.\n\n");
    out.push_str(
        "| Provider | Kinds | Schema | Runtime | Signature | Secrets | Outbound methods |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|\n");
    for provider in &providers {
        out.push_str(&provider_row(provider));
    }

    out.push_str("\n## First-party Connector Packages\n\n");
    out.push_str("Prefer pure-Harn packages for provider business logic. The Rust providers remain compatibility defaults while the pure-Harn packages soak.\n\n");
    out.push_str("| Provider | Package | Install | Contract check |\n");
    out.push_str("|---|---|---|---|\n");
    for package in FIRST_PARTY_CONNECTOR_PACKAGES {
        out.push_str(&format!(
            "| {} | <{}> | `{}` | `{}` |\n",
            package.provider, package.package_url, package.install, package.contract_check
        ));
    }
    out.push('\n');
    out.push_str("Community connectors are Harn packages that declare `connector_contract = \"v1\"` and export the connector functions below. Direct GitHub refs are enough for private or pre-registry packages; registry names such as `@burin/notion-connector` are for discoverable package-index entries.\n\n");

    out.push_str("## Connector Contract V1\n\n");
    out.push_str("Required exports for a pure-Harn connector package:\n\n");
    out.push_str("| Export | Required | Purpose |\n");
    out.push_str("|---|---:|---|\n");
    out.push_str(
        "| `provider_id() -> string` | Yes | Provider id, matching `[[providers]].id`. |\n",
    );
    out.push_str("| `kinds() -> list<string>` | Yes | Trigger kinds such as `webhook`, `poll`, `cron`, `a2a-push`, or `stream`. |\n");
    out.push_str("| `payload_schema() -> dict` | Yes | `{ harn_schema_name, json_schema? }`; the contract check rejects `{ name = ... }` drift. |\n");
    out.push_str("| `normalize_inbound(raw) -> dict` | Inbound | Returns `NormalizeResult` v1 for webhook-style input. |\n");
    out.push_str("| `poll_tick(ctx) -> dict` | Poll | Required when `kinds()` includes `poll`; returns events plus optional `cursor`/`state`. |\n");
    out.push_str("| `call(method, args) -> dict` | Outbound | Provider API escape hatch. Unknown probes may throw `method_not_found:<method>`. |\n");
    out.push_str("| `init(ctx)` | No | Receives event log, secrets, metrics, inbox, and rate-limit handles. |\n");
    out.push_str("| `activate(bindings)` | No | Runs on manifest activation/reload. |\n");
    out.push_str("| `shutdown()` | No | Cleanup on reload or process shutdown. |\n\n");
    out.push_str("`normalize_inbound(raw)` must return one of these tagged shapes: `{ type: \"event\", event }`, `{ type: \"batch\", events }`, `{ type: \"immediate_response\", immediate_response, event?, events? }`, or `{ type: \"reject\", status, body? }`. Direct legacy event dicts are transitional only; new packages should use the tagged shape.\n\n");
    out.push_str("Connector-only builtins available during connector export execution: `secret_get`, `event_log_emit`, and `metrics_inc`. The hot-path `normalize_inbound` effect policy rejects network calls, LLM calls, process execution, host calls, MCP calls, and ambient filesystem/project access.\n\n");

    out.push_str("## Package Fixtures\n\n");
    out.push_str("Connector packages should declare deterministic fixtures in `harn.toml` and run them in CI:\n\n");
    out.push_str("```toml\n[connector_contract]\nversion = 1\n\n[[connector_contract.fixtures]]\nprovider = \"slack\"\nname = \"url verification\"\nkind = \"webhook\"\nheaders = { \"content-type\" = \"application/json\" }\nbody_json = { type = \"url_verification\", challenge = \"challenge-token\" }\nexpect_type = \"immediate_response\"\nexpect_event_count = 0\n```\n\n");
    out.push_str("Run `harn connector check .` locally. Use `--provider <id>` for a multi-provider package, `--run-poll-tick` to execute the first poll tick, and `--json` for CI output.\n\n");

    out.push_str("## Example Library\n\n");
    out.push_str("Ready-to-customize pipelines live under `examples/triggers/`. Each example includes `harn.toml`, `lib.harn`, `README.md`, and `SKILL.md` so it can be copied into a project or installed as a local skill bundle. Validate examples with `make check-trigger-examples`.\n");
    out
}

fn provider_row(provider: &ProviderMetadata) -> String {
    format!(
        "| `{}` | {} | `{}` | {} | {} | {} | {} |\n",
        provider.provider,
        comma_code(&provider.kinds),
        provider.schema_name,
        runtime_summary(&provider.runtime),
        signature_summary(&provider.signature_verification),
        secret_summary(provider),
        method_summary(provider),
    )
}

fn comma_code(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn runtime_summary(runtime: &ProviderRuntimeMetadata) -> String {
    match runtime {
        ProviderRuntimeMetadata::Builtin {
            connector,
            default_signature_variant,
        } => match default_signature_variant {
            Some(variant) => format!("builtin `{connector}` / `{variant}` signatures"),
            None => format!("builtin `{connector}`"),
        },
        ProviderRuntimeMetadata::Placeholder => "placeholder".to_string(),
    }
}

fn signature_summary(signature: &SignatureVerificationMetadata) -> String {
    match signature {
        SignatureVerificationMetadata::None => "none".to_string(),
        SignatureVerificationMetadata::Hmac {
            variant,
            signature_header,
            timestamp_header,
            id_header,
            default_tolerance_secs,
            digest,
            encoding,
            ..
        } => {
            let mut parts = vec![
                format!("HMAC `{variant}`"),
                format!("header `{signature_header}`"),
                format!("{digest}/{encoding}"),
            ];
            if let Some(header) = timestamp_header {
                parts.push(format!("ts `{header}`"));
            }
            if let Some(header) = id_header {
                parts.push(format!("id `{header}`"));
            }
            if let Some(tolerance) = default_tolerance_secs {
                parts.push(format!("{tolerance}s tolerance"));
            }
            parts.join(", ")
        }
    }
}

fn secret_summary(provider: &ProviderMetadata) -> String {
    if provider.secret_requirements.is_empty() {
        return "-".to_string();
    }
    provider
        .secret_requirements
        .iter()
        .map(|secret| {
            let required = if secret.required {
                "required"
            } else {
                "optional"
            };
            format!("`{}/{}` ({required})", secret.namespace, secret.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn method_summary(provider: &ProviderMetadata) -> String {
    if provider.outbound_methods.is_empty() {
        "-".to_string()
    } else {
        provider
            .outbound_methods
            .iter()
            .map(|method| format!("`{}`", method.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_quickref_contains_catalog_and_contract() {
        let out = generate_file();
        assert!(out.contains("| `github` | `webhook` | `GitHubEventPayload` |"));
        assert!(out.contains("Connector Contract V1"));
        assert!(out.contains("harn connector check ."));
        assert!(out.contains("harn-forgejo-connector"));
        assert!(out.contains("harn-svn-connector"));
    }

    #[test]
    fn committed_trigger_quickref_matches_generator() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir)
            .join("..")
            .join("..")
            .join("docs")
            .join("llm")
            .join("harn-triggers-quickref.md");
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "failed to read {}: {e}\n\
                 hint: run `make gen-trigger-quickref` to regenerate.",
                path.display()
            )
        });
        let generated = generate_file();
        assert_eq!(
            on_disk, generated,
            "docs/llm/harn-triggers-quickref.md is stale relative to the trigger provider catalog.\n\
             Run `make gen-trigger-quickref` to regenerate."
        );
    }
}
