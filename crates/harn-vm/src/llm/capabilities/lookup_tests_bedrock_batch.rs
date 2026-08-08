use super::{clear_user_overrides, lookup};

#[test]
fn batch_support_is_exact_and_region_scoped() {
    clear_user_overrides();
    let supported = lookup("bedrock", "anthropic.claude-sonnet-4-5-20250929-v1:0");
    assert!(supported.batch_api);
    assert_eq!(supported.batch_wire_format.as_deref(), Some("bedrock"));
    assert_eq!(supported.batch_regions, ["us-east-1", "us-west-2"]);

    for unsupported in [
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
        "amazon.nova-pro-v1:0",
    ] {
        let caps = lookup("bedrock", unsupported);
        assert!(!caps.batch_api);
        assert!(caps.batch_regions.is_empty());
    }
}
