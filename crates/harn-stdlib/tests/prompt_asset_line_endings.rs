use harn_stdlib::{StdlibPromptAsset, STDLIB_PROMPT_ASSETS};

fn assets_with_carriage_returns(assets: &[StdlibPromptAsset]) -> Vec<&str> {
    assets
        .iter()
        .filter_map(|asset| asset.source.contains('\r').then_some(asset.path))
        .collect()
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
