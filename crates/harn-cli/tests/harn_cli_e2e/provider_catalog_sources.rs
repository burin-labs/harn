use std::{fs, path::Path};

use crate::test_util::process::harn_e2e_command;

fn render(root: &Path, surface: &str) -> std::process::Output {
    harn_e2e_command()
        .current_dir(root)
        .args(["provider", "catalog", surface, "--stdout"])
        .output()
        .expect("run catalog renderer")
}

#[test]
fn catalog_renderers_read_source_changes_without_rebuilding() {
    let root = tempfile::tempdir().unwrap();
    let sources = root.path().join("crates/harn-vm/src/llm");
    fs::create_dir_all(sources.join("capability_sources")).unwrap();
    fs::create_dir_all(sources.join("catalog_sources")).unwrap();
    fs::write(
        sources.join("catalog_sources/catalog.toml"),
        concat!(
            include_str!("../../../harn-vm/src/llm/providers.toml"),
            "\n[providers.mock]\ndisplay_name = \"Mock source fixture\"\nauth_style = \"none\"\nfeatures = [\"wire_model_capabilities\"]\n\n[models.\"mock/source-falsifier-8062\"]\nname = \"Source falsifier 8062\"\nprovider = \"mock\"\nwire_model = \"source-wire-route-8062\"\ncontext_window = 8192\n"
        ).replacen("unverified = [", "unverified = [\"mock\",", 1)
            .replacen("featured_providers = [", "featured_providers = [\"mock\",", 1)
            // Support generation needs a chosen recommendation for a
            // provider with chat routes; name the source row.
            .replacen("[qc_defaults]\n", "[qc_defaults]\nmock = \"mock/source-falsifier-8062\"\n", 1),
    ).unwrap();
    fs::write(
        sources.join("capability_sources/capabilities.toml"),
        "[[provider.mock]]\nmodel_match = \"source-wire-route-8062\"\nstreaming = true\nprompt_caching = true\n",
    )
    .unwrap();
    for surface in ["matrix", "support"] {
        let output = render(root.path(), surface);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains(if surface == "matrix" {
                "source-wire-route-8062"
            } else {
                "source-falsifier-8062"
            }),
            "{surface} ignored the source row: {text}"
        );
        assert!(!text.contains("source-falsifier-8063"));
        if surface == "matrix" {
            let artifact = root.path().join("matrix.md");
            let check = || {
                harn_e2e_command()
                    .current_dir(root.path())
                    .args(["provider", "catalog", "matrix", "--check", "--output"])
                    .arg(&artifact)
                    .output()
                    .unwrap()
            };
            fs::write(&artifact, "stale matrix\n").unwrap();
            assert!(
                !check().status.success(),
                "stale matrix must fail the canonical gate"
            );
            fs::write(&artifact, &text).unwrap();
            let current = check();
            assert!(
                current.status.success(),
                "{}",
                String::from_utf8_lossy(&current.stderr)
            );
        }
    }
    let support = harn_e2e_command()
        .current_dir(root.path())
        .args(["provider", "catalog", "support", "--json"])
        .output()
        .unwrap();
    assert!(support.status.success());
    let support: serde_json::Value = serde_json::from_slice(&support.stdout).unwrap();
    assert_eq!(support["credentials"][0]["id"], "mock");
    assert_eq!(support["credentials"][0]["featured"], true);
    let mock = support["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "mock")
        .expect("source provider reached support renderer");
    assert_eq!(mock["capabilities"]["prompt_or_context_cache"], true);
    assert_eq!(mock["recommended"]["model"], "mock/source-falsifier-8062");
    fs::write(
        sources.join("capability_sources/capabilities.toml"),
        "[[invalid",
    )
    .unwrap();
    for surface in ["matrix", "support"] {
        assert!(
            !render(root.path(), surface).status.success(),
            "malformed source must not fall back"
        );
    }
    fs::remove_dir_all(&sources).unwrap();
    for surface in ["matrix", "support"] {
        let output = render(root.path(), surface);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("openai"), "embedded catalog must be reached");
        assert!(!text.contains("source-falsifier-8062"));
        assert!(!text.contains("source-wire-route-8062"));
    }
}

/// The provider contract campaign reads `availability` from this dump to skip
/// models a provider serves only from a dedicated deployment. A dump without
/// the field would read every model as probe-able, so pin both values.
#[test]
fn provider_catalog_show_reports_model_availability() {
    let output = harn_e2e_command()
        .args(["provider", "catalog", "show"])
        .output()
        .expect("run provider catalog show");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("provider catalog show emits JSON");
    let availability = |id: &str| {
        value["models"]
            .as_array()
            .expect("models array")
            .iter()
            .find(|model| model["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from provider catalog show"))["availability"]
            .clone()
    };
    assert_eq!(
        availability("accounts/fireworks/models/kimi-k2p6"),
        "dedicated"
    );
    assert_eq!(
        availability("accounts/fireworks/models/glm-5p3-flash"),
        "serverless"
    );
}
