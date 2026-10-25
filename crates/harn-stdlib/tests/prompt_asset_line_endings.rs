use harn_stdlib::{StdlibPromptAsset, CATALOG};

fn assets_with_carriage_returns(assets: &[StdlibPromptAsset]) -> Vec<&str> {
    sources_with_carriage_returns(assets.iter().map(|asset| (asset.path, asset.source)))
}

fn sources_with_carriage_returns<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<&'a str> {
    sources
        .into_iter()
        .filter_map(|(name, source)| source.contains('\r').then_some(name))
        .collect()
}

#[test]
fn embedded_harn_sources_use_lf_line_endings() {
    assert!(!CATALOG.sources.is_empty() && !CATALOG.cli_scripts.is_empty());
    let offenders = sources_with_carriage_returns(
        CATALOG
            .sources
            .iter()
            .map(|entry| (entry.module, entry.source))
            .chain(
                CATALOG
                    .cli_scripts
                    .iter()
                    .map(|entry| (entry.name, entry.source)),
            ),
    );
    assert!(
        offenders.is_empty(),
        "embedded source must use LF: {offenders:?}"
    );
}

#[test]
fn embedded_prompt_assets_use_lf_line_endings() {
    assert!(
        !CATALOG.prompt_assets.is_empty(),
        "the embedded prompt catalog must be non-empty before its line endings can pass"
    );

    let offenders = assets_with_carriage_returns(CATALOG.prompt_assets);
    assert!(
        offenders.is_empty(),
        "embedded prompt assets must use LF line endings: {}",
        offenders.join(", ")
    );
}

#[test]
fn embedded_prompt_line_ending_check_rejects_crlf() {
    let fixture = [
        StdlibPromptAsset {
            path: "lf.harn.prompt",
            source: "first\nsecond\n",
        },
        StdlibPromptAsset {
            path: "crlf.harn.prompt",
            source: "first\r\nsecond\r\n",
        },
    ];

    assert_eq!(
        assets_with_carriage_returns(&fixture),
        vec!["crlf.harn.prompt"]
    );
}
