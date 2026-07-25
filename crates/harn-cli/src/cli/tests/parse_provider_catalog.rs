//! `harn provider catalog <sub>` argument parsing.
//!
//! Split from `parse_providers` so the catalog surface — refresh, validate,
//! generate, export, overlay-audit, matrix, support, recommend, show — has one
//! file, and so `parse_providers` stays about the wider provider surface.

use super::*;

fn parse_provider_catalog(args: &[&str]) -> ProviderCatalogCommand {
    let mut argv = vec!["harn", "provider", "catalog"];
    argv.extend_from_slice(args);
    let cli = Cli::parse_from(argv);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Catalog(catalog) = provider.command else {
        panic!("expected provider catalog command");
    };
    catalog.command
}

#[test]
fn test_parses_providers_refresh_args() {
    let ProviderCatalogCommand::Refresh(args) = parse_provider_catalog(&[
        "refresh",
        "--live",
        "--check",
        "--script",
        "scripts/update_provider_catalog.harn",
    ]) else {
        panic!("expected provider catalog refresh command");
    };
    assert!(args.live);
    assert!(args.check);
    assert!(!args.update);
    assert_eq!(
        args.script,
        std::path::PathBuf::from("scripts/update_provider_catalog.harn")
    );
}

#[test]
fn test_parses_providers_validate_args() {
    let ProviderCatalogCommand::Validate(args) =
        parse_provider_catalog(&["validate", "--overlay", "providers.local.toml", "--json"])
    else {
        panic!("expected provider catalog validate command");
    };
    assert_eq!(
        args.overlay.as_deref(),
        Some(std::path::Path::new("providers.local.toml"))
    );
    assert!(args.json);
}

#[test]
fn test_parses_providers_export_args() {
    let ProviderCatalogCommand::Export(args) =
        parse_provider_catalog(&["export", "--output-dir", "tmp/catalog", "--check"])
    else {
        panic!("expected provider catalog export command");
    };
    assert_eq!(args.output_dir, std::path::PathBuf::from("tmp/catalog"));
    assert!(args.check);
}

#[test]
fn test_parses_providers_overlay_audit_args() {
    let ProviderCatalogCommand::OverlayAudit(args) = parse_provider_catalog(&[
        "overlay-audit",
        "--overlay",
        "tmp/providers.toml",
        "--json",
        "--check",
    ]) else {
        panic!("expected provider catalog overlay-audit command");
    };
    assert_eq!(args.overlay, std::path::PathBuf::from("tmp/providers.toml"));
    assert!(args.json);
    assert!(args.check);
}

#[test]
fn test_parses_providers_generate_args() {
    let ProviderCatalogCommand::Generate(args) = parse_provider_catalog(&[
        "generate",
        "--source-dir",
        "tmp/catalog_sources",
        "--capability-source-dir",
        "tmp/capability_sources",
        "--providers-output",
        "tmp/providers.toml",
        "--capabilities-output",
        "tmp/capabilities.toml",
        "--artifact-dir",
        "tmp/catalog",
        "--check",
    ]) else {
        panic!("expected provider catalog generate command");
    };
    assert_eq!(
        args.source_dir,
        std::path::PathBuf::from("tmp/catalog_sources")
    );
    assert_eq!(
        args.capability_source_dir,
        std::path::PathBuf::from("tmp/capability_sources")
    );
    assert_eq!(
        args.providers_output,
        std::path::PathBuf::from("tmp/providers.toml")
    );
    assert_eq!(
        args.capabilities_output,
        std::path::PathBuf::from("tmp/capabilities.toml")
    );
    assert_eq!(args.artifact_dir, std::path::PathBuf::from("tmp/catalog"));
    assert!(args.check);
}

#[test]
fn test_parses_providers_matrix_args() {
    let ProviderCatalogCommand::Matrix(args) = parse_provider_catalog(&[
        "matrix",
        "--output",
        "tmp/provider-matrix.md",
        "--check",
        "--empirical",
        "tmp/parity.toml",
        "--filter",
        "native_tools",
    ]) else {
        panic!("expected provider catalog matrix command");
    };
    assert_eq!(
        args.output,
        std::path::PathBuf::from("tmp/provider-matrix.md")
    );
    assert!(args.check);
    assert_eq!(args.filter.as_deref(), Some("native_tools"));
}

#[test]
fn test_parses_providers_support_args() {
    let ProviderCatalogCommand::Support(args) = parse_provider_catalog(&[
        "support",
        "--output",
        "tmp/provider-support.md",
        "--json-output",
        "tmp/provider-support.json",
        "--notes",
        "provider_support_notes.toml",
        "--empirical",
        "summary.json",
        "--check",
    ]) else {
        panic!("expected provider catalog support command");
    };
    assert_eq!(
        args.output,
        std::path::PathBuf::from("tmp/provider-support.md")
    );
    assert_eq!(
        args.json_output,
        std::path::PathBuf::from("tmp/provider-support.json")
    );
    assert_eq!(
        args.notes,
        std::path::PathBuf::from("provider_support_notes.toml")
    );
    assert_eq!(
        args.empirical,
        vec![std::path::PathBuf::from("summary.json")]
    );
    assert!(args.check);
}

#[test]
fn test_parses_provider_catalog_args() {
    let ProviderCatalogCommand::Show(args) =
        parse_provider_catalog(&["show", "--available-only", "--refresh"])
    else {
        panic!("expected provider catalog show command");
    };
    assert!(args.available_only);
    assert!(args.refresh);
}

#[test]
fn test_parses_providers_recommend_args() {
    let ProviderCatalogCommand::Recommend(recommend) = parse_provider_catalog(&[
        "recommend",
        "--input",
        "local_readiness.json",
        "--provider",
        "ollama",
        "--json",
    ]) else {
        panic!("expected provider catalog recommend command");
    };
    assert_eq!(recommend.input, Some(PathBuf::from("local_readiness.json")));
    assert_eq!(recommend.provider.as_deref(), Some("ollama"));
    assert!(recommend.json);
}
