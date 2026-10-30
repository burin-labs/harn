//! Project complete translated templates into Rust-checked formatting branches.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

#[path = "localization_contract.rs"]
mod contract;

pub fn generate(manifest: &Path) {
    let formats = manifest.join("src/localization_formats.json");
    println!("cargo:rerun-if-changed={}", formats.display());
    let source: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&formats).expect("read CLI templates"))
            .expect("CLI templates must be strings");
    assert!(!source.is_empty(), "CLI templates must be populated");
    let english_path = manifest.join("src/localization/en.json");
    println!("cargo:rerun-if-changed={}", english_path.display());
    let english: BTreeMap<String, String> = serde_json::from_str(
        &std::fs::read_to_string(english_path).expect("read canonical CLI catalog"),
    )
    .expect("canonical CLI messages must map strings");
    let mut catalogs = Vec::new();
    for (name, variant) in [
        ("es", "Spanish"),
        ("fr", "French"),
        ("de", "German"),
        ("pt-BR", "BrazilianPortuguese"),
        ("it", "Italian"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("zh-Hans", "SimplifiedChinese"),
    ] {
        let path = manifest.join(format!("src/localization/{name}.json"));
        println!("cargo:rerun-if-changed={}", path.display());
        let catalog: BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read CLI catalog"))
                .expect("CLI catalog must map strings");
        contract::validate_fields(&english, &catalog)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(
            source.keys().all(|key| catalog
                .get(key)
                .is_some_and(|value| !value.trim().is_empty())),
            "incomplete {name} command-result catalog"
        );
        catalogs.push((variant, catalog));
    }
    let mut generated = String::from("macro_rules! formatted {\n");
    for key in source.keys() {
        // Keep formatter temporaries outside the calling async state machine.
        // Nine inline formatting arms otherwise inflate its stack allocation.
        writeln!(
            generated,
            "({key:?} $(, $($args:tt)*)?) => {{{{ let __harn_cli_render = || match $crate::localization::selected_locale() {{"
        )
        .unwrap();
        writeln!(
            generated,
            "$crate::localization::Locale::English => ::std::format!({key:?} $(, $($args)*)?),"
        )
        .unwrap();
        for (variant, catalog) in &catalogs {
            writeln!(
                generated,
                "$crate::localization::Locale::{variant} => ::std::format!({:?} $(, $($args)*)?),",
                catalog[key]
            )
            .unwrap();
        }
        generated.push_str("}; __harn_cli_render() }};\n");
    }
    generated.push_str("}\npub(crate) use formatted;\n");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output"));
    std::fs::write(output.join("localization_formats.rs"), generated)
        .expect("write CLI formatting projection");
}
