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
            "\n[models.\"mock/source-falsifier-8062\"]\nname = \"Source falsifier 8062\"\nprovider = \"mock\"\ncontext_window = 8192\n"
        ),
    ).unwrap();
    fs::write(
        sources.join("capability_sources/capabilities.toml"),
        "[[provider.mock]]\nmodel_match = \"source-falsifier-8062\"\nstreaming = true\n",
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
            text.contains("source-falsifier-8062"),
            "{surface} ignored the source row: {text}"
        );
        assert!(!text.contains("source-falsifier-8063"));
    }
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
    }
}
