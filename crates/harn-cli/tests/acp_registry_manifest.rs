use serde_json::{json, Value as JsonValue};

const AGENT_MANIFEST: &str = include_str!("../../../spec/acp-registry/harn/agent.json");

#[test]
fn acp_registry_manifest_tracks_current_binary_distribution() {
    let manifest: JsonValue =
        serde_json::from_str(AGENT_MANIFEST).expect("ACP registry manifest parses");
    let version = env!("CARGO_PKG_VERSION");

    assert_eq!(manifest["id"], "harn");
    assert_eq!(manifest["version"], version);

    let binary = manifest["distribution"]["binary"]
        .as_object()
        .expect("binary distribution map");
    let expected_targets = [
        (
            "darwin-aarch64",
            "harn-aarch64-apple-darwin.tar.gz",
            "./harn",
        ),
        ("darwin-x86_64", "harn-x86_64-apple-darwin.tar.gz", "./harn"),
        (
            "linux-aarch64",
            "harn-aarch64-unknown-linux-gnu.tar.gz",
            "./harn",
        ),
        (
            "linux-x86_64",
            "harn-x86_64-unknown-linux-gnu.tar.gz",
            "./harn",
        ),
        (
            "windows-x86_64",
            "harn-x86_64-pc-windows-msvc.zip",
            "harn.exe",
        ),
    ];

    assert_eq!(binary.len(), expected_targets.len());
    for (target, archive_name, command) in expected_targets {
        let entry = binary
            .get(target)
            .unwrap_or_else(|| panic!("missing binary target {target}"));
        assert_eq!(entry["cmd"], command);
        assert_eq!(entry["args"], json!(["serve", "acp"]));
        assert_eq!(
            entry["archive"],
            format!(
                "https://github.com/burin-labs/harn/releases/download/v{version}/{archive_name}"
            )
        );
    }
}
