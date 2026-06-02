//! `harn scan` dispatches to the embedded `cli/scan.harn` handler. The shim
//! resolves the rule(s) and the fileset in Rust (see [`crate::commands::
//! rules_cli`]) — where the tree-sitter `Language` registry and the
//! gitignore-aware walker live — then hands the `.harn` handler a per-rule
//! *plan* (rule TOML + the matching files). The handler runs the engine
//! (`std/rules`) and formats the output.

use crate::cli::ScanArgs;

#[cfg(not(feature = "hostlib"))]
pub(crate) async fn run(_args: ScanArgs) {
    eprintln!(
        "`harn scan` requires the `hostlib` feature (default-on); it is unavailable in this build"
    );
    std::process::exit(2);
}

#[cfg(feature = "hostlib")]
pub(crate) async fn run(args: ScanArgs) {
    use crate::dispatch;
    use crate::env_guard::ScopedEnvVar;

    let plan = match build_plan(&args) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("scan: {message}");
            std::process::exit(2);
        }
    };

    let _plan = ScopedEnvVar::set("HARN_SCAN_PLAN_JSON", &plan);
    let _report = ScopedEnvVar::set(
        "HARN_SCAN_REPORT_ONLY",
        if args.report_only { "1" } else { "0" },
    );
    let exit = dispatch::dispatch_to_embedded_script("scan", Vec::new(), args.json).await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

#[cfg(feature = "hostlib")]
fn build_plan(args: &ScanArgs) -> Result<String, String> {
    use crate::commands::rules_cli;

    // With a saved rule/pack every positional is a path; otherwise the first
    // positional is the inline pattern and the rest are paths.
    let saved_rule = args.rule.is_some() || args.rule_pack.is_some();
    let (pattern, paths): (Option<&str>, &[String]) = if saved_rule {
        (None, &args.args)
    } else {
        (
            args.args.first().map(String::as_str),
            args.args.get(1..).unwrap_or(&[]),
        )
    };

    let specs = rules_cli::resolve_rules(
        pattern,
        args.lang.as_deref(),
        args.rule.as_deref(),
        args.rule_pack.as_deref(),
    )?;
    rules_cli::build_plan(specs, paths)
}
