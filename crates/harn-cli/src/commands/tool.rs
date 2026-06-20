use std::fs;
use std::path::PathBuf;

use crate::cli::ToolNewArgs;
use crate::commands::scaffold_common::harn_identifier_with_prefix;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::{
    current_harn_range_example, generate_package_docs_impl, validate_package_alias, PackageError,
};

/// `harn tool new` dispatch shim. Validates the requested package alias
/// in Rust, resolves the destination directory, then delegates the
/// template render + file-write loop to `cli/scaffold/tool_new.harn`
/// through the standard CLI dispatch path.
pub(crate) async fn run_new(args: &ToolNewArgs) -> Result<(), PackageError> {
    validate_package_alias(&args.name)?;
    let dest = args
        .dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&args.name));
    if dest.exists() {
        if !args.force {
            return Err(format!(
                "{} already exists. Pass --force to overwrite.",
                dest.display()
            )
            .into());
        }
        if !dest.is_dir() {
            return Err(format!("{} exists and is not a directory.", dest.display()).into());
        }
    } else {
        fs::create_dir_all(&dest)
            .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;
    }

    let description = args
        .description
        .clone()
        .unwrap_or_else(|| format!("Custom Harn tool package for {}.", args.name));
    if description.contains('\n') || description.contains('\r') {
        return Err("tool description must be a single line".to_string().into());
    }
    let ident = harn_identifier(&args.name)?;
    let handler = format!("handle_{ident}");

    dispatch_to_script(&args.name, &dest, &ident, &handler, &description).await?;

    generate_package_docs_impl(Some(&dest), None, false)?;

    println!(
        "Scaffolded tool package '{}' at {}",
        args.name,
        dest.display()
    );
    println!("  harn test tests/");
    println!("  harn package check");
    println!("  harn package docs --check");
    println!("  harn package pack --dry-run");
    Ok(())
}

async fn dispatch_to_script(
    name: &str,
    dest: &std::path::Path,
    ident: &str,
    handler: &str,
    description: &str,
) -> Result<(), PackageError> {
    let dest_str = dest.display().to_string();
    let harn_range = current_harn_range_example();
    let _name_env = ScopedEnvVar::set("HARN_TOOL_NAME", name);
    let _dest_env = ScopedEnvVar::set("HARN_TOOL_DEST", &dest_str);
    let _ident_env = ScopedEnvVar::set("HARN_TOOL_IDENT", ident);
    let _handler_env = ScopedEnvVar::set("HARN_TOOL_HANDLER", handler);
    let _desc_env = ScopedEnvVar::set("HARN_TOOL_DESCRIPTION", description);
    let _range_env = ScopedEnvVar::set("HARN_TOOL_HARN_RANGE", &harn_range);
    let exit = dispatch::dispatch_to_embedded_script(
        "scaffold/tool_new",
        Vec::new(),
        /* json_mode */ false,
    )
    .await;
    if exit != 0 {
        return Err(format!("tool new scaffolder exited with code {exit}").into());
    }
    Ok(())
}

fn harn_identifier(name: &str) -> Result<String, PackageError> {
    harn_identifier_with_prefix(name, "tool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harn_identifier_normalizes_package_names() {
        assert_eq!(harn_identifier("acme-tool").unwrap(), "acme_tool");
        assert_eq!(harn_identifier("123-tool").unwrap(), "tool_123_tool");
    }

    // The force-overwrite behavior moved to a subprocess parity test in
    // `tests/scaffold_dispatch.rs`: the dispatched `.harn` impl runs the
    // full VM and would overflow the default `#[tokio::test]` thread
    // stack here. The subprocess inherits the workspace-level 16 MiB
    // runtime stack via `harn_cli::install_signal_shutdown_handler`.
}
