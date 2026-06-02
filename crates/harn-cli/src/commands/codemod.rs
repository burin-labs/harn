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

    if args.rule.is_none() && args.rule_pack.is_none() {
        eprintln!("codemod: provide `--rule <file>` or `--rule-pack <dir>`");
        std::process::exit(2);
    }

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
    rules_cli::build_plan(specs, &args.paths)
}
