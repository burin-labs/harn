use harn_stdlib::{StdlibPromptAsset, STDLIB_CLI_SCRIPTS, STDLIB_PROMPT_ASSETS, STDLIB_SOURCES};

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
    assert!(!STDLIB_SOURCES.is_empty() && !STDLIB_CLI_SCRIPTS.is_empty());
    let offenders = sources_with_carriage_returns(
        STDLIB_SOURCES
            .iter()
            .map(|entry| (entry.module, entry.source))
            .chain(
                STDLIB_CLI_SCRIPTS
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
        !STDLIB_PROMPT_ASSETS.is_empty(),
        "the embedded prompt catalog must be non-empty before its line endings can pass"
    );

    let offenders = assets_with_carriage_returns(STDLIB_PROMPT_ASSETS);
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
