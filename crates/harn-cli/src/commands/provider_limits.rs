//! `harn provider limits [<provider>] [--json]` — deterministically report the
//! LLM rate/concurrency governor's configuration and live state. No network, no
//! LLM call.
//!
//! Two halves:
//!   - **resolved limits** — the per-provider governor config the catalog would
//!     apply (max/min concurrency, rpm/tpm, adaptive, backoff), defaulted for
//!     providers with no `[provider_limits.<provider>]` row. Always available.
//!   - **live governors** — when the `llm.rate_governor` flag is on and calls
//!     have flowed, each `(provider, org_key)`'s current AIMD concurrency limit,
//!     circuit state, in-flight count, throttle streak, and last signal.
//!
//! This is the sibling of `provider dispatch-explain` for rate governance
//! instead of routing.

use crate::cli::ProviderLimitsArgs;
use harn_vm::llm::rate_governor::{self, ResolvedLimits};

pub(crate) fn run(args: &ProviderLimitsArgs) {
    let live = rate_governor::all_snapshots();
    let flag_on = rate_governor::enabled();

    // Which providers to show resolved limits for: the explicit one, else the
    // union of catalog-configured providers and any live governor's provider.
    let providers = resolved_provider_set(args, &live);

    if args.json {
        emit_json(args, flag_on, &providers, &live);
        return;
    }
    emit_text(args, flag_on, &providers, &live);
}

/// The provider ids to report resolved limits for. A caller wanting a specific
/// unconfigured provider passes it explicitly and gets the built-in default row.
fn resolved_provider_set(
    args: &ProviderLimitsArgs,
    live: &[(String, rate_governor::GovernorSnapshot)],
) -> Vec<String> {
    if let Some(p) = args.provider.as_ref() {
        return vec![p.trim().to_ascii_lowercase()];
    }
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    set.extend(rate_governor::configured_limit_providers());
    for (route, _) in live {
        if let Some((provider, _)) = route.split_once("::") {
            set.insert(provider.to_string());
        }
    }
    set.into_iter().collect()
}

fn resolved_json(provider: &str, l: &ResolvedLimits) -> serde_json::Value {
    serde_json::json!({
        "provider": provider,
        "max_concurrency": l.max_concurrency,
        "min_concurrency": l.min_concurrency,
        "rpm": l.rpm,
        "tpm": l.tpm,
        "adaptive": l.adaptive,
        "backoff": {
            "base_ms": l.backoff_base_ms,
            "max_ms": l.backoff_max_ms,
            "multiplier": l.backoff_multiplier,
            "jitter": l.backoff_jitter,
        },
    })
}

fn emit_json(
    args: &ProviderLimitsArgs,
    flag_on: bool,
    providers: &[String],
    live: &[(String, rate_governor::GovernorSnapshot)],
) {
    let resolved: Vec<serde_json::Value> = providers
        .iter()
        .map(|p| resolved_json(p, &rate_governor::resolved_limits_for(p)))
        .collect();
    let filter = args.provider.as_ref().map(|p| p.trim().to_ascii_lowercase());
    let governors: Vec<serde_json::Value> = live
        .iter()
        .filter(|(route, _)| match filter.as_ref() {
            Some(p) => route.starts_with(&format!("{p}::")),
            None => true,
        })
        .map(|(route, snap)| {
            let mut obj = snap.to_json();
            if let Some(o) = obj.as_object_mut() {
                o.insert("route".to_string(), serde_json::json!(route));
            }
            obj
        })
        .collect();
    let report = serde_json::json!({
        "flag": "llm.rate_governor",
        "enabled": flag_on,
        "resolved_limits": resolved,
        "governors": governors,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
    );
}

fn emit_text(
    args: &ProviderLimitsArgs,
    flag_on: bool,
    providers: &[String],
    live: &[(String, rate_governor::GovernorSnapshot)],
) {
    println!(
        "rate governor (flag llm.rate_governor: {})",
        if flag_on { "ON" } else { "OFF" }
    );
    println!("\nresolved limits (from catalog):");
    for p in providers {
        let l = rate_governor::resolved_limits_for(p);
        println!(
            "  {p}: concurrency {}..{}{}{}, adaptive={}, backoff {}ms..{}ms x{}{}",
            l.min_concurrency,
            l.max_concurrency,
            l.rpm.map(|r| format!(", rpm={r}")).unwrap_or_default(),
            l.tpm.map(|t| format!(", tpm={t}")).unwrap_or_default(),
            l.adaptive,
            l.backoff_base_ms,
            l.backoff_max_ms,
            l.backoff_multiplier,
            if l.backoff_jitter { " jitter" } else { "" },
        );
    }

    let filter = args.provider.as_ref().map(|p| p.trim().to_ascii_lowercase());
    let shown: Vec<&(String, rate_governor::GovernorSnapshot)> = live
        .iter()
        .filter(|(route, _)| match filter.as_ref() {
            Some(p) => route.starts_with(&format!("{p}::")),
            None => true,
        })
        .collect();

    if shown.is_empty() {
        if flag_on {
            println!("\nlive governors: none yet (no calls have flowed)");
        } else {
            println!("\nlive governors: (flag off — no governor state is tracked)");
        }
        return;
    }
    println!("\nlive governors (provider::org_key):");
    for (route, snap) in shown {
        println!(
            "  {route}: circuit={}, concurrency={} ({}..{}), in_flight={}, throttles={}, open_cycles={}, last_signal={}",
            snap.circuit_state,
            snap.concurrency_limit,
            snap.min_concurrency,
            snap.max_concurrency,
            snap.in_flight,
            snap.consecutive_throttles,
            snap.open_cycles,
            snap.last_signal.unwrap_or("none"),
        );
    }
}
