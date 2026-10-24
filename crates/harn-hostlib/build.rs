use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=schemas");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let schemas = manifest.join("schemas");
    let proof: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(schemas.join("edit-proof.json")).expect("read edit proof schema"),
    )
    .expect("parse edit proof schema");
    let mut entries = discover(&schemas);
    entries.sort();
    let mut pairs = BTreeMap::<(String, String), u8>::new();
    for (module, method, kind, _) in &entries {
        let bit = if kind == "request" { 1 } else { 2 };
        *pairs.entry((module.clone(), method.clone())).or_default() |= bit;
    }
    for ((module, method), directions) in pairs {
        assert_eq!(
            directions, 3,
            "hostlib schema must ship a request/response pair: {module}.{method}"
        );
    }

    let mut generated = String::from(
        "/// Every schema discovered under `schemas/<module>/`.\n\
         pub const SCHEMAS: &[(&str, &str, SchemaKind, &str)] = &[\n",
    );
    for (module, method, kind, path) in entries {
        let kind_variant = match kind.as_str() {
            "request" => "Request",
            "response" => "Response",
            _ => unreachable!(),
        };
        let mut schema: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read schema"))
                .expect("parse schema");
        if schema
            .pointer("/properties/proof/$ref")
            .and_then(serde_json::Value::as_str)
            == Some("https://harnlang.com/schemas/hostlib/edit-proof.json")
        {
            schema["properties"]["proof"]["$ref"] = serde_json::json!("#/$defs/edit_proof");
            schema["$defs"]["edit_proof"] = proof.clone();
        }
        let body = serde_json::to_string(&schema).expect("serialize schema");
        generated.push_str(&format!(
            "    ({module:?}, {method:?}, SchemaKind::{kind_variant}, {body:?}),\n"
        ));
    }
    generated.push_str("];\n");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("out dir"));
    fs::write(out.join("hostlib_schemas.rs"), generated).expect("write schema catalog");
}

fn discover(root: &Path) -> Vec<(String, String, String, PathBuf)> {
    let mut entries = Vec::new();
    for module in fs::read_dir(root).expect("read schemas directory") {
        let module = module.expect("read schema module entry");
        if !module.file_type().expect("schema module type").is_dir() {
            continue;
        }
        let module_name = module.file_name().to_string_lossy().into_owned();
        for file in fs::read_dir(module.path()).expect("read schema module") {
            let file = file.expect("read schema entry");
            if !file.file_type().expect("schema entry type").is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            let Some((method, kind)) = stem.rsplit_once('.') else {
                panic!("schema filename must be <method>.<request|response>.json: {name}");
            };
            assert!(
                matches!(kind, "request" | "response"),
                "schema filename has unknown direction: {name}"
            );
            entries.push((
                module_name.clone(),
                method.to_string(),
                kind.to_string(),
                file.path(),
            ));
        }
    }
    entries
}
