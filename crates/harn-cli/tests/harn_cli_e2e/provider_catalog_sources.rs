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
    // Edit the catalog by table path: the generated file's key order is not
    // stable, so a text anchor can land in the wrong table.
    let mut catalog: toml::Value =
        toml::from_str(harn_provider_catalog::EMBEDDED_PROVIDERS_TOML).unwrap();
    let fixture: toml::Value = toml::from_str(
        "[providers.mock]\ndisplay_name = \"Mock source fixture\"\nauth_style = \"none\"\nfeatures = [\"wire_model_capabilities\"]\n\n[models.\"mock/source-falsifier-8062\"]\nname = \"Source falsifier 8062\"\nprovider = \"mock\"\nwire_model = \"source-wire-route-8062\"\ncontext_window = 8192\n",
    )
    .unwrap();
    for table in ["providers", "models"] {
        let rows = fixture[table].as_table().unwrap().clone();
        catalog[table].as_table_mut().unwrap().extend(rows);
    }
    for (table, list) in [
        ("usage_accounting_audit", "unverified"),
        ("presentation", "featured_providers"),
    ] {
        catalog[table][list]
            .as_array_mut()
            .unwrap()
            .insert(0, "mock".into());
    }
    // Support generation needs a chosen recommendation for a provider with
    // chat routes; name the source row.
    catalog["qc_defaults"]
        .as_table_mut()
        .unwrap()
        .insert("mock".into(), "mock/source-falsifier-8062".into());
    fs::write(
        sources.join("catalog_sources/catalog.toml"),
        toml::to_string(&catalog).unwrap(),
    )
    .unwrap();
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

#[test]
fn catalog_export_resolves_wire_capabilities_from_its_explicit_provider_overlay() {
    let root = tempfile::tempdir().unwrap();
    let overlay_path = root.path().join("providers.toml");
    let capabilities_path = root.path().join("capabilities.toml");
    let overlay = r#"
[providers.fixture_proxy]
base_url = "https://fixture.invalid/v1"
chat_endpoint = "/chat/completions"
auth_style = "none"
features = ["wire_model_capabilities", "responses_api"]
[models.private-wire-alias]
name = "Explicit wire identity fixture"
provider = "fixture_proxy"
context_window = 8192
wire_model = "served-shape"
"#;
    fs::write(
        &capabilities_path,
        r#"
[[provider.fixture_proxy]]
model_match = "served-shape"
native_tools = true
vision = true
responses_api = true
"#,
    )
    .unwrap();
    let export = |name: &str| {
        let destination = root.path().join(name);
        let output = harn_e2e_command()
            .current_dir(root.path())
            .args(["provider", "catalog", "export", "--output-dir"])
            .arg(&destination)
            .arg("--overlay")
            .arg(&overlay_path)
            .arg("--capabilities-overlay")
            .arg(&capabilities_path)
            .output()
            .expect("export catalog");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let catalog: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("provider-catalog.json")).unwrap())
                .unwrap();
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "private-wire-alias")
            .expect("explicit model was exported")
            .clone()
    };
    fs::write(&overlay_path, overlay).unwrap();
    let declared = export("declared");
    assert_eq!(declared["tool_support"]["native"], true);
    assert!(declared["capability_tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tag| tag == "vision"));

    fs::write(
        &overlay_path,
        overlay.replace("\"wire_model_capabilities\", ", ""),
    )
    .unwrap();
    let undeclared = export("undeclared");
    assert_eq!(undeclared["tool_support"]["native"], false);
    assert!(!undeclared["capability_tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tag| tag == "vision"));
}
