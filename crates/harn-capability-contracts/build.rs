use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use syn::parse::Parser as _;
use syn::{Ident, Item, Token};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let mut definitions = Vec::new();

    println!("cargo:rerun-if-changed=src");
    for source in rust_sources(&manifest_dir.join("src")) {
        println!(
            "cargo:rerun-if-changed={}",
            source
                .strip_prefix(&manifest_dir)
                .expect("capability source is under manifest directory")
                .display()
        );
        collect_definitions(&source, &mut definitions);
    }

    definitions.sort();
    definitions.dedup();

    let mut generated = String::from("&[\n");
    for definition in definitions {
        generated.push_str("    &");
        generated.push_str(&definition);
        generated.push_str("_DEF,\n");
    }
    generated.push_str("]\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    fs::write(out_dir.join("capability_method_defs.rs"), generated)
        .expect("write generated capability method manifest");
}

fn rust_sources(source_dir: &Path) -> Vec<PathBuf> {
    let mut sources = fs::read_dir(source_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", source_dir.display()))
        .map(|entry| entry.expect("read capability source entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("rs"))
        .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some("lib.rs"))
        .collect::<Vec<_>>();
    sources.sort();
    sources
}

fn collect_definitions(path: &Path, definitions: &mut Vec<String>) {
    let source =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let file = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {} as Rust: {error}", path.display()));

    for item in file.items {
        let Item::Macro(item_macro) = item else {
            continue;
        };
        if !item_macro.mac.path.is_ident("capability_method") {
            continue;
        }
        let first_argument = |input: syn::parse::ParseStream<'_>| {
            let name: Ident = input.parse()?;
            input.parse::<Token![,]>()?;
            let _: proc_macro2::TokenStream = input.parse()?;
            Ok(name)
        };
        let name = first_argument
            .parse2(item_macro.mac.tokens)
            .unwrap_or_else(|error| {
                panic!(
                    "parse capability declaration in {}: {error}",
                    path.display()
                )
            });
        definitions.push(name.to_string().to_uppercase());
    }
}
