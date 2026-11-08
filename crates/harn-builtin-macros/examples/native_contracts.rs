//! Project native builtin declarations without their implementations.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use proc_macro2::{TokenStream, TokenTree};
use quote::{format_ident, quote};
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, Item, Lit, Meta, Token};

#[derive(Clone)]
enum Condition {
    Known(bool),
    Atom(Meta),
    All(Vec<Self>),
    Any(Vec<Self>),
    Not(Box<Self>),
}

impl Condition {
    fn parse(meta: Meta) -> Self {
        if meta.path().is_ident("test") {
            return Self::Known(false);
        }
        if let Meta::List(list) = &meta {
            let args = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .expect("parse cfg predicate");
            let args = args.into_iter().map(Self::parse).collect::<Vec<_>>();
            if list.path.is_ident("all") {
                return Self::All(args).simplify();
            }
            if list.path.is_ident("any") {
                return Self::Any(args).simplify();
            }
            if list.path.is_ident("not") {
                assert_eq!(args.len(), 1, "not needs one predicate");
                return Self::Not(Box::new(args.into_iter().next().unwrap())).simplify();
            }
        }
        Self::Atom(meta)
    }

    fn simplify(self) -> Self {
        match self {
            Self::All(args) => {
                if args.iter().any(|arg| matches!(arg, Self::Known(false))) {
                    return Self::Known(false);
                }
                let mut args = args
                    .into_iter()
                    .filter(|arg| !matches!(arg, Self::Known(true)))
                    .collect::<Vec<_>>();
                match args.len() {
                    0 => Self::Known(true),
                    1 => args.pop().unwrap(),
                    _ => Self::All(args),
                }
            }
            Self::Any(args) => {
                if args.iter().any(|arg| matches!(arg, Self::Known(true))) {
                    return Self::Known(true);
                }
                let mut args = args
                    .into_iter()
                    .filter(|arg| !matches!(arg, Self::Known(false)))
                    .collect::<Vec<_>>();
                match args.len() {
                    0 => Self::Known(false),
                    1 => args.pop().unwrap(),
                    _ => Self::Any(args),
                }
            }
            Self::Not(arg) => match *arg {
                Self::Known(value) => Self::Known(!value),
                other => Self::Not(Box::new(other)),
            },
            other => other,
        }
    }

    fn tokens(&self) -> TokenStream {
        match self {
            Self::Known(true) => quote!(all()),
            Self::Known(false) => quote!(any()),
            Self::Atom(meta) => quote!(#meta),
            Self::All(args) => {
                let args = args.iter().map(Self::tokens);
                quote!(all(#(#args),*))
            }
            Self::Any(args) => {
                let args = args.iter().map(Self::tokens);
                quote!(any(#(#args),*))
            }
            Self::Not(arg) => {
                let arg = arg.tokens();
                quote!(not(#arg))
            }
        }
    }
}

fn condition(attrs: &[Attribute], inherited: Condition) -> Condition {
    let mut conditions = vec![inherited];
    for attr in attrs {
        if attr.path().is_ident("cfg") {
            conditions.push(Condition::parse(attr.parse_args().expect("cfg predicate")));
        } else if attr.path().is_ident("cfg_attr") {
            let args = attr
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .expect("cfg_attr arguments");
            let mut args = args.into_iter();
            let predicate = Condition::parse(args.next().expect("cfg_attr predicate"));
            if matches!(predicate, Condition::Known(false)) {
                continue;
            }
            for nested in args {
                assert!(
                    !nested.path().is_ident("path"),
                    "conditional module paths need an explicit source projection"
                );
                if let Meta::List(cfg) = nested {
                    if cfg.path.is_ident("cfg") {
                        let inner = syn::parse2(cfg.tokens).expect("nested cfg predicate");
                        conditions.push(
                            Condition::Any(vec![
                                Condition::Not(Box::new(predicate.clone())).simplify(),
                                Condition::parse(inner),
                            ])
                            .simplify(),
                        );
                    }
                }
            }
        }
    }
    Condition::All(conditions).simplify()
}

struct Declaration {
    source: PathBuf,
    condition: Condition,
    function: syn::Ident,
    attrs: TokenStream,
    docs: Vec<Attribute>,
}

fn string_value(meta: &Meta) -> String {
    let Meta::NameValue(value) = meta else {
        panic!("expected a string-valued attribute")
    };
    let Expr::Lit(value) = &value.value else {
        panic!("expected a string literal")
    };
    let Lit::Str(value) = &value.lit else {
        panic!("expected a string literal")
    };
    value.value()
}

fn module_directory(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap();
    match file.file_name().unwrap().to_str().unwrap() {
        "lib.rs" | "main.rs" | "mod.rs" => parent.to_path_buf(),
        _ => parent.join(file.file_stem().unwrap()),
    }
}

fn walk_file(file: &Path, inherited: Condition, declarations: &mut Vec<Declaration>) {
    let source = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let parsed = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
    let inherited = condition(&parsed.attrs, inherited);
    walk_items(
        &parsed.items,
        file,
        &module_directory(file),
        false,
        inherited,
        declarations,
    );
}

fn walk_items(
    items: &[Item],
    file: &Path,
    directory: &Path,
    inline: bool,
    inherited: Condition,
    declarations: &mut Vec<Declaration>,
) {
    if matches!(inherited, Condition::Known(false)) {
        return;
    }
    for item in items {
        match item {
            Item::Fn(function) => {
                let cfg = condition(&function.attrs, inherited.clone());
                if matches!(cfg, Condition::Known(false)) {
                    continue;
                }
                for attr in &function.attrs {
                    if attr.path().segments.last().unwrap().ident != "harn_builtin" {
                        continue;
                    }
                    let Meta::List(args) = &attr.meta else {
                        panic!("builtin attribute needs arguments")
                    };
                    declarations.push(Declaration {
                        source: file.to_path_buf(),
                        condition: cfg.clone(),
                        function: function.sig.ident.clone(),
                        attrs: args.tokens.clone(),
                        docs: function
                            .attrs
                            .iter()
                            .filter(|attr| attr.path().is_ident("doc"))
                            .cloned()
                            .collect(),
                    });
                }
            }
            Item::Mod(module) => {
                let cfg = condition(&module.attrs, inherited.clone());
                if matches!(cfg, Condition::Known(false)) {
                    continue;
                }
                let path = module
                    .attrs
                    .iter()
                    .find(|attr| attr.path().is_ident("path"))
                    .map(|attr| string_value(&attr.meta));
                if let Some((_, items)) = &module.content {
                    let child = path.map_or_else(
                        || directory.join(module.ident.to_string()),
                        |path| {
                            (if inline {
                                directory
                            } else {
                                file.parent().unwrap()
                            })
                            .join(path)
                        },
                    );
                    walk_items(items, file, &child, true, cfg, declarations);
                } else {
                    let child = if let Some(path) = path {
                        (if inline {
                            directory
                        } else {
                            file.parent().unwrap()
                        })
                        .join(path)
                    } else {
                        let plain = directory.join(format!("{}.rs", module.ident));
                        let nested = directory.join(module.ident.to_string()).join("mod.rs");
                        assert_ne!(
                            plain.exists(),
                            nested.exists(),
                            "exactly one module source must exist for {} in {}",
                            module.ident,
                            file.display()
                        );
                        if plain.exists() {
                            plain
                        } else {
                            nested
                        }
                    };
                    walk_file(&child, cfg, declarations);
                }
            }
            Item::Macro(item) if item.mac.path.is_ident("include") => {
                let cfg = condition(&item.attrs, inherited.clone());
                if matches!(cfg, Condition::Known(false)) {
                    continue;
                }
                let path: syn::LitStr = syn::parse2(item.mac.tokens.clone())
                    .expect("production include must name a source file");
                let child = file.parent().unwrap().join(path.value());
                let parsed = syn::parse_file(&std::fs::read_to_string(&child).unwrap()).unwrap();
                walk_items(&parsed.items, &child, directory, inline, cfg, declarations);
            }
            Item::Macro(item) => {
                assert!(
                    !contains_builtin(item.mac.tokens.clone()),
                    "macro-generated declarations in {} need an explicit projection",
                    file.display()
                );
                assert!(
                    item.mac.path.segments.last().unwrap().ident != "harn_capability_method",
                    "method declaration in {} needs a compiler projection",
                    file.display()
                );
            }
            _ => {}
        }
    }
}

fn contains_builtin(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => ident == "harn_builtin",
        TokenTree::Group(group) => contains_builtin(group.stream()),
        _ => false,
    })
}

fn formatted(source: TokenStream) -> String {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2021", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("rustfmt is installed by setup");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "format generated declarations");
    String::from_utf8(output.stdout).unwrap()
}

fn render(declarations: &[Declaration], root: &Path) -> String {
    assert!(
        declarations.len() > 500,
        "native declarations were not reached"
    );
    let mut modules = Vec::new();
    let mut references = Vec::new();
    for (index, declaration) in declarations.iter().enumerate() {
        let module = format_ident!("declaration_{index:04}");
        let name = &declaration.function;
        let definition = format_ident!("{}_DEF", name.to_string().to_uppercase());
        let attrs = &declaration.attrs;
        let docs = &declaration.docs;
        let cfg = declaration.condition.tokens();
        let source = declaration
            .source
            .strip_prefix(root)
            .unwrap()
            .display()
            .to_string();
        modules.push(quote! {
            #[cfg(#cfg)]
            #[doc = #source]
            mod #module {
                #[allow(unused_imports)]
                use harn_builtin_meta::*;
                #[harn_builtin_macros::harn_builtin_metadata(#attrs)]
                #(#docs)*
                fn #name() {}
            }
        });
        references.push(quote! { #[cfg(#cfg)] definitions.push(&#module::#definition); });
    }
    let body = formatted(quote! {
        #(#modules)*
        pub(super) fn definitions() -> Vec<&'static harn_vm_core::builtin_support::VmBuiltinDef> {
            let mut definitions = Vec::new();
            #(#references)*
            definitions
        }
        pub(super) fn force_link() { std::hint::black_box(definitions()); }
    });
    format!("// Generated by make gen-vm-exposures from native builtin attributes.\n// Do not edit this projection.\n\n{body}")
}

fn manifest(root: &Path) -> String {
    let cargo: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let version = cargo["workspace"]["package"]["version"].as_str().unwrap();
    let vm: toml::Table = std::fs::read_to_string(root.join("crates/harn-vm/Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let runtime: toml::Table = std::fs::read_to_string(root.join("crates/harn-runtime/Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut output = String::from("# Generated by make gen-vm-exposures.\n[package]\nname = \"harn-stdlib-compiler\"\nversion.workspace = true\nedition.workspace = true\nrust-version.workspace = true\nlicense.workspace = true\nrepository.workspace = true\nhomepage.workspace = true\ndescription = \"Compile Harn standard library bytecode without native runtime implementations\"\n\n[features]\ndefault = [\"full\"]\n");
    for (feature, aliases) in vm["features"].as_table().unwrap() {
        if feature == "default" {
            continue;
        }
        let mut forwards = vec![format!("harn-vm/{feature}")];
        if runtime["features"].get(feature).is_some() {
            forwards.push(format!("harn-runtime/{feature}"));
        }
        if feature == "full" {
            forwards.extend(
                aliases
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned()),
            );
        }
        writeln!(output, "{feature} = {forwards:?}").unwrap();
    }
    output.push_str("\n[dependencies]\n");
    for dependency in [
        "harn-builtin-meta",
        "harn-builtin-macros",
        "harn-stdlib",
        "harn-vm-core",
    ] {
        writeln!(
            output,
            "{dependency} = {{ path = \"../{dependency}\", version = \"={version}\" }}"
        )
        .unwrap();
    }
    output.push_str("\n[dev-dependencies]\n");
    for dependency in ["harn-runtime", "harn-vm"] {
        writeln!(output, "{dependency} = {{ path = \"../{dependency}\", version = \"={version}\", default-features = false }}").unwrap();
    }
    output.push_str("\n[lints]\nworkspace = true\n");
    output
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut declarations = Vec::new();
    for name in ["harn-vm", "harn-builtins", "harn-flow"] {
        walk_file(
            &root.join(format!("crates/{name}/src/lib.rs")),
            Condition::Known(true),
            &mut declarations,
        );
    }
    let declarations_text = render(&declarations, &root);
    let mut exposures = BTreeSet::new();
    for declaration in &declarations {
        let attributes = Punctuated::<Meta, Token![,]>::parse_terminated
            .parse2(declaration.attrs.clone())
            .expect("builtin attributes");
        for attr in attributes {
            if attr.path().is_ident("exposure") {
                let value = string_value(&attr);
                if let Some(method) = value.strip_prefix("harness.") {
                    let (field, method) = method.split_once('.').expect("capability and method");
                    exposures.insert((field.to_string(), method.to_string()));
                }
            }
        }
    }
    assert!(!exposures.is_empty(), "capability methods were not reached");
    let exposures_body = exposures
        .iter()
        .map(|(field, method)| format!("    ({field:?}, {method:?}),\n"))
        .collect::<String>();
    let exposures = format!("//! Generated by make gen-vm-exposures from native builtin attributes.\n//! Signatures and effects remain owned by those declarations.\n\npub static VM_DECLARED_CAPABILITY_METHODS: &[(&str, &str)] = &[\n{exposures_body}];\n");
    let check = std::env::args().any(|arg| arg == "--check");
    for (path, expected) in [
        (
            "crates/harn-stdlib-compiler/src/declarations.rs",
            declarations_text,
        ),
        ("crates/harn-stdlib-compiler/Cargo.toml", manifest(&root)),
        (
            "crates/harn-capability-contracts/src/vm_declared.rs",
            exposures,
        ),
    ] {
        let path = root.join(path);
        if check {
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                expected,
                "{} is stale; run make gen-vm-exposures",
                path.display()
            );
        } else {
            std::fs::write(&path, expected).unwrap();
        }
    }
    println!("{} native declarations projected", declarations.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN: &str =
        "#[harn_builtin(exposure = \"pure\", sig = \"actual() -> int\")] fn actual() {}";

    fn write(root: &Path, path: &str, source: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    #[test]
    fn source_walk_preserves_production_cfg_and_ignores_test_only_includes() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "lib.rs", "#[cfg(any(test, feature = \"postgres\"))] mod selected; #[cfg(test)] include!(concat!(env!(\"OUT_DIR\"), \"/fixture.rs\")); const TEXT: &str = \"harn_builtin\";");
        write(root.path(), "selected.rs", BUILTIN);
        let mut declarations = Vec::new();
        walk_file(
            &root.path().join("lib.rs"),
            Condition::Known(true),
            &mut declarations,
        );
        assert_eq!(declarations.len(), 1);
        assert_eq!(
            declarations[0].condition.tokens().to_string(),
            "feature = \"postgres\""
        );
    }

    #[test]
    fn outlined_and_inline_path_attributes_resolve_from_their_rust_module_context() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "lib.rs", "mod outer;");
        write(root.path(), "outer.rs", "#[path = \"shared.rs\"] mod sibling; mod inline { #[path = \"selected.rs\"] mod nested; }");
        write(root.path(), "shared.rs", BUILTIN);
        write(root.path(), "outer/inline/selected.rs", BUILTIN);
        let mut declarations = Vec::new();
        walk_file(
            &root.path().join("lib.rs"),
            Condition::Known(true),
            &mut declarations,
        );
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].source, root.path().join("shared.rs"));
        assert_eq!(
            declarations[1].source,
            root.path().join("outer/inline/selected.rs")
        );
    }

    #[test]
    fn included_declarations_keep_the_callers_cfg() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "lib.rs",
            "#[cfg(unix)] include!(\"declarations.rs\");",
        );
        write(root.path(), "declarations.rs", BUILTIN);
        let mut declarations = Vec::new();
        walk_file(
            &root.path().join("lib.rs"),
            Condition::Known(true),
            &mut declarations,
        );
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].condition.tokens().to_string(), "unix");
    }

    #[test]
    #[should_panic(expected = "exactly one module source")]
    fn missing_module_cannot_be_reported_as_an_empty_contract() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "lib.rs", "mod missing;");
        walk_file(
            &root.path().join("lib.rs"),
            Condition::Known(true),
            &mut Vec::new(),
        );
    }
}
