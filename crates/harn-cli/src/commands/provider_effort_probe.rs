//! `harn provider effort-probe` — thin argv shim.
//!
//! The probe itself is `stdlib/cli/providers/effort_probe.harn`. The only
//! thing this shim decides is whether Harn's declared-ladder check stays on.
//! It is suspended by default: the check refuses an out-of-ladder effort
//! before the request leaves the process, so with it on the probe could only
//! ever re-measure the catalog against itself and no drift would be findable.
//! `--gated` restores it for callers who want a confirm-only run.

use crate::cli::ProviderEffortProbeArgs;

pub(crate) async fn run(args: ProviderEffortProbeArgs) -> i32 {
    let argv = effort_probe_argv(&args);
    let _ungated = (!args.gated)
        .then(|| crate::env_guard::ScopedEnvVar::set(harn_vm::llm::EFFORT_LADDER_UNGATED_ENV, "1"));
    crate::dispatch::dispatch_to_embedded_script("providers/effort_probe", argv, args.json).await
}

/// Fold the parsed flags into the script's argv. Split out from the dispatch
/// call so the mapping is testable without running a probe.
fn effort_probe_argv(args: &ProviderEffortProbeArgs) -> Vec<String> {
    let mut argv = Vec::new();
    for model in &args.models {
        argv.push("--model".to_string());
        argv.push(model.clone());
    }
    if args.all_declared {
        argv.push("--all-declared".to_string());
    }
    if !args.efforts.is_empty() {
        argv.push("--effort".to_string());
        argv.push(args.efforts.join(","));
    }
    argv.push("--max-tokens".to_string());
    argv.push(args.max_tokens.to_string());
    argv.push("--prompt".to_string());
    argv.push(args.prompt.clone());
    if args.one_per_claim {
        argv.push("--one-per-claim".to_string());
    }
    if args.plan {
        argv.push("--plan".to_string());
    }
    if args.suggest_fragment {
        argv.push("--suggest-fragment".to_string());
    }
    if args.fail_on_drift {
        argv.push("--fail-on-drift".to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> ProviderEffortProbeArgs {
        ProviderEffortProbeArgs {
            models: vec!["glm-5.3".to_string()],
            all_declared: false,
            efforts: vec!["low".to_string(), "max".to_string()],
            max_tokens: 16,
            prompt: "ping".to_string(),
            one_per_claim: false,
            plan: false,
            suggest_fragment: false,
            fail_on_drift: false,
            gated: false,
            json: false,
        }
    }

    #[test]
    fn every_route_reaches_the_script_as_its_own_model_flag() {
        let mut probe = args();
        probe.models = vec!["glm-5.3".to_string(), "openrouter:z-ai/glm-5.3".to_string()];
        let argv = effort_probe_argv(&probe);
        assert_eq!(
            argv.iter().filter(|token| *token == "--model").count(),
            2,
            "each selector needs its own --model so a model id containing a comma              cannot merge two routes into one: {argv:?}"
        );
        assert!(argv.contains(&"openrouter:z-ai/glm-5.3".to_string()));
    }

    #[test]
    fn requested_rungs_reach_the_script_verbatim() {
        let argv = effort_probe_argv(&args());
        let index = argv.iter().position(|token| token == "--effort").unwrap();
        assert_eq!(argv[index + 1], "low,max");
    }

    #[test]
    fn optional_switches_stay_absent_until_asked_for() {
        let argv = effort_probe_argv(&args());
        for absent in [
            "--all-declared",
            "--one-per-claim",
            "--plan",
            "--suggest-fragment",
            "--fail-on-drift",
        ] {
            assert!(
                !argv.contains(&absent.to_string()),
                "{absent} leaked into {argv:?}"
            );
        }
    }
}
