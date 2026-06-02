//! `harn codemod` dispatches to the embedded `cli/codemod.harn` handler. Like
//! `harn scan`, the shim resolves the rule(s) and the fileset in Rust (see
//! [`crate::commands::rules_cli`]) and hands the handler a per-rule plan; the
//! handler applies each rule's `fix` via `std/rules` (dry-run by default).

use crate::cli::CodemodArgs;

#[cfg(not(feature = "hostlib"))]
pub(crate) async fn run(_args: CodemodArgs) {
    eprintln!(
        "`harn codemod` requires the `hostlib` feature (default-on); it is unavailable in this build"
    );
    std::process::exit(2);
}

#[cfg(feature = "hostlib")]
pub(crate) async fn run(args: CodemodArgs) {
    use crate::dispatch;
    use crate::env_guard::ScopedEnvVar;

    // No early `--rule` check: `resolve` falls back to the project's
    // `[rules] ruleDirs` (#2843) and errors itself if nothing is found.
    let plan = match resolve(&args) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("codemod: {message}");
            std::process::exit(2);
        }
    };

    let _plan = ScopedEnvVar::set("HARN_CODEMOD_PLAN_JSON", &plan);
    let _apply = ScopedEnvVar::set("HARN_CODEMOD_APPLY", if args.apply { "1" } else { "0" });
    let _unsafe = ScopedEnvVar::set(
        "HARN_CODEMOD_ALLOW_UNSAFE",
        if args.allow_unsafe { "1" } else { "0" },
    );
    let exit = dispatch::dispatch_to_embedded_script("codemod", Vec::new(), args.json).await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

#[cfg(feature = "hostlib")]
fn resolve(args: &CodemodArgs) -> Result<String, String> {
    use crate::commands::rules_cli;

    let specs =
        rules_cli::resolve_rules(None, None, args.rule.as_deref(), args.rule_pack.as_deref())?;

    // A codemod only applies rules that have a `fix`. Discovered packs mix in
    // lint/search rules, which are silently skipped here; an explicitly given
    // fix-less rule is a user error and reported as such.
    let explicit = args.rule.is_some() || args.rule_pack.is_some();
    let codemod_specs: Vec<_> = specs
        .into_iter()
        .filter(|s| rules_cli::rule_has_fix(&s.toml))
        .collect();
    if codemod_specs.is_empty() {
        return Err(if explicit {
            "the given rule has no `fix` template (it is a lint, not a codemod)".into()
        } else {
            "no codemod rules (rules with a `fix`) found in the project `[rules] ruleDirs`".into()
        });
    }
    rules_cli::build_plan(codemod_specs, &args.paths)
}
