//! `harn provider option-probe` dispatch shim.
//!
//! The Harn script owns planning, provider-error classification, and the typed
//! catalog diff. This shim owns the task-scoped authority a truthful negative
//! probe needs: suspending catalog shaping for exactly the option named on the
//! command line and limiting the observation to one physical provider request.

use crate::cli::ProviderOptionProbeArgs;

pub(crate) async fn run(mut args: ProviderOptionProbeArgs) -> i32 {
    args.model =
        match crate::commands::providers::resolve_probe_wire_model(&args.provider, &args.model) {
            Ok(model) => model,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
    let argv = option_probe_argv(&args);
    let dispatch =
        crate::dispatch::dispatch_to_embedded_script("providers/option_probe", argv, args.json);
    if args.gated {
        dispatch.await
    } else {
        harn_vm::llm::with_portable_option_probe(args.option.portable_option(), dispatch).await
    }
}

fn option_probe_argv(args: &ProviderOptionProbeArgs) -> Vec<String> {
    let mut argv = vec![
        "--provider".to_string(),
        args.provider.clone(),
        "--model".to_string(),
        args.model.clone(),
        "--option".to_string(),
        args.option.name().to_string(),
        "--max-tokens".to_string(),
        args.max_tokens.to_string(),
    ];
    if args.plan {
        argv.push("--plan".to_string());
    }
    if !args.gated {
        argv.push("--ungated".to_string());
    }
    if args.fail_on_drift {
        argv.push("--fail-on-drift".to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ProviderPortableOptionArg;

    #[test]
    fn typed_option_reaches_the_embedded_probe_verbatim() {
        let args = ProviderOptionProbeArgs {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5".to_string(),
            option: ProviderPortableOptionArg::TopP,
            max_tokens: 8,
            plan: true,
            fail_on_drift: true,
            gated: false,
            json: true,
        };
        assert_eq!(
            option_probe_argv(&args),
            [
                "--provider",
                "anthropic",
                "--model",
                "claude-sonnet-5",
                "--option",
                "top_p",
                "--max-tokens",
                "8",
                "--plan",
                "--ungated",
                "--fail-on-drift",
            ]
        );
    }

    #[tokio::test]
    async fn embedded_probe_plan_resolves_shared_contracts() {
        let outcome = crate::dispatch::run_embedded_script(
            "providers/option_probe",
            vec![
                "--provider".into(),
                "anthropic".into(),
                "--model".into(),
                "claude-opus-5".into(),
                "--option".into(),
                "temperature".into(),
                "--max-tokens".into(),
                "8".into(),
                "--plan".into(),
                "--ungated".into(),
            ],
            true,
        )
        .await;
        assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
        assert!(outcome.stderr.is_empty(), "stderr={}", outcome.stderr);
        let report: serde_json::Value =
            serde_json::from_str(&outcome.stdout).expect("option probe plan JSON");
        assert_eq!(
            report["schema_version"],
            "harn.provider_option_probe_plan.v1"
        );
        assert_eq!(report["catalog"]["field"], "temperature_supported");
        assert_eq!(report["request_count"], 1);
    }
}
