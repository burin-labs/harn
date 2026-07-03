//! Interface fingerprints — a stable hash of a module's public surface.
//!
//! The fingerprint covers exactly the parts of a module that downstream
//! importers can observe:
//!
//! * public functions, pipelines, tools, skills (name + signature)
//! * public structs, enums, type aliases, interfaces (full shape)
//! * `pub import` re-exports (target path + selective names)
//!
//! It intentionally excludes anything internal — function bodies,
//! comments, private helpers, local variable bindings — so an edit that
//! changes only the implementation of a public function leaves the
//! fingerprint stable and dependents stay valid.
//!
//! The hash is BLAKE3 over a canonical textual rendering of the surface
//! (alphabetized, single source of truth so trivial reorderings don't
//! flip the fingerprint either).

use std::fmt::Write as _;
use std::path::Path;

use harn_parser::{
    peel_attributes, EnumVariant, InterfaceMethod, Node, Parser, SNode, ShapeField, StructField,
    TypeExpr, TypeParam, TypedParam, Variance, WhereClause,
};

use crate::read_module_source;

/// A 32-byte BLAKE3 digest of a module's public surface.
pub type Fingerprint = [u8; 32];

/// Compute the interface fingerprint for `program` (the parsed top-level
/// statements of a module). Returns the BLAKE3 digest of a canonical
/// textual rendering — see the module docs for what's included.
pub fn fingerprint_program(program: &[SNode]) -> Fingerprint {
    let canonical = canonicalize_program(program);
    blake3::hash(canonical.as_bytes()).into()
}

/// Convenience: parse `path` (real file or `<std>` virtual path) and
/// fingerprint its public surface. Returns `None` when the source can't
/// be read or doesn't lex/parse — callers can treat that as "no
/// fingerprint" rather than erroring.
pub fn fingerprint_file(path: &Path) -> Option<Fingerprint> {
    let source = read_module_source(path)?;
    fingerprint_source(&source)
}

/// Fingerprint an already-loaded source string. Lex + parse failures
/// return `None`.
pub fn fingerprint_source(source: &str) -> Option<Fingerprint> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().ok()?;
    let program = Parser::new(tokens).parse().ok()?;
    Some(fingerprint_program(&program))
}

/// Hex-encode a fingerprint for human-readable output (NDJSON events,
/// logs, etc.).
pub fn fingerprint_hex(fp: &Fingerprint) -> String {
    let mut out = String::with_capacity(fp.len() * 2);
    for b in fp {
        write!(&mut out, "{b:02x}").expect("write to String is infallible");
    }
    out
}

fn canonicalize_program(program: &[SNode]) -> String {
    let mut lines: Vec<String> = program.iter().filter_map(canonicalize_top_level).collect();
    lines.sort();
    lines.join("\n")
}

fn canonicalize_top_level(snode: &SNode) -> Option<String> {
    let (_attrs, inner) = peel_attributes(snode);
    match &inner.node {
        Node::FnDecl {
            name,
            type_params,
            params,
            return_type,
            where_clauses,
            is_pub,
            is_stream,
            ..
        } => is_pub.then(|| {
            format!(
                "fn{stream}:{name}{generics}({params}){ret}{wheres}",
                stream = if *is_stream { "*" } else { "" },
                generics = format_type_params(type_params),
                params = format_typed_params(params),
                ret = format_return(return_type),
                wheres = format_where_clauses(where_clauses),
            )
        }),
        Node::Pipeline {
            name,
            params,
            return_type,
            is_pub,
            extends,
            ..
        } => is_pub.then(|| {
            format!(
                "pipeline:{name}({params}){ret}{extends}",
                params = params.join(","),
                ret = format_return(return_type),
                extends = extends
                    .as_deref()
                    .map(|e| format!(" extends {e}"))
                    .unwrap_or_default(),
            )
        }),
        Node::ToolDecl {
            name,
            params,
            return_type,
            is_pub,
            ..
        } => is_pub.then(|| {
            format!(
                "tool:{name}({params}){ret}",
                params = format_typed_params(params),
                ret = format_return(return_type),
            )
        }),
        Node::SkillDecl { name, is_pub, .. } => {
            // Skill bodies are configuration that downstream importers
            // observe by reading the resulting registry dict, but a
            // skill is identified by its name from a typing perspective.
            // The conservative thing is to hash the name only and let
            // body edits propagate via runtime registration rather than
            // type-time invalidation.
            is_pub.then(|| format!("skill:{name}"))
        }
        Node::StructDecl {
            name,
            type_params,
            fields,
            is_pub,
        } => is_pub.then(|| {
            format!(
                "struct:{name}{generics}{{{fields}}}",
                generics = format_type_params(type_params),
                fields = format_struct_fields(fields),
            )
        }),
        Node::EnumDecl {
            name,
            type_params,
            variants,
            is_pub,
        } => is_pub.then(|| {
            format!(
                "enum:{name}{generics}{{{variants}}}",
                generics = format_type_params(type_params),
                variants = format_enum_variants(variants),
            )
        }),
        Node::InterfaceDecl {
            name,
            type_params,
            associated_types,
            methods,
        } => Some(format!(
            "interface:{name}{generics}{{assoc=[{assoc}]methods=[{methods}]}}",
            generics = format_type_params(type_params),
            assoc = format_associated_types(associated_types),
            methods = format_interface_methods(methods),
        )),
        Node::TypeDecl {
            name,
            type_params,
            type_expr,
            is_pub,
        } => is_pub.then(|| {
            format!(
                "type:{name}{generics}={ty}",
                generics = format_type_params(type_params),
                ty = format_type_expr(type_expr),
            )
        }),
        Node::ImportDecl { path, is_pub } => is_pub.then(|| format!("pub_import_wildcard:{path}")),
        Node::SelectiveImport {
            names,
            path,
            is_pub,
        } => is_pub.then(|| {
            let mut sorted = names.clone();
            sorted.sort();
            format!("pub_import_selective:{path}::{}", sorted.join(","))
        }),
        _ => None,
    }
}

fn format_type_params(params: &[TypeParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            let var = match p.variance {
                Variance::Invariant => "",
                Variance::Covariant => "out ",
                Variance::Contravariant => "in ",
            };
            format!("{var}{}", p.name)
        })
        .collect();
    format!("<{}>", parts.join(","))
}

fn format_typed_params(params: &[TypedParam]) -> String {
    params
        .iter()
        .map(|p| {
            let mut s = String::new();
            if p.rest {
                s.push_str("...");
            }
            s.push_str(&p.name);
            if let Some(ty) = &p.type_expr {
                s.push(':');
                s.push_str(&format_type_expr(ty));
            }
            // Default values reference expressions whose shape we don't
            // walk into; presence-only is enough — adding a default to
            // a public parameter changes the callable contract.
            if p.default_value.is_some() {
                s.push_str("=?");
            }
            s
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn format_return(ret: &Option<TypeExpr>) -> String {
    match ret {
        Some(ty) => format!("->{}", format_type_expr(ty)),
        None => String::new(),
    }
}

fn format_where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = clauses
        .iter()
        .map(|w| format!("{}:{}", w.type_name, format_type_expr(&w.bound)))
        .collect();
    parts.sort();
    format!(" where {}", parts.join(","))
}

fn format_struct_fields(fields: &[StructField]) -> String {
    let mut rendered: Vec<String> = fields
        .iter()
        .map(|f| {
            let opt = if f.optional { "?" } else { "" };
            let ty = f
                .type_expr
                .as_ref()
                .map(format_type_expr)
                .unwrap_or_default();
            format!("{}{opt}:{ty}", f.name)
        })
        .collect();
    rendered.sort();
    rendered.join(",")
}

fn format_enum_variants(variants: &[EnumVariant]) -> String {
    let mut rendered: Vec<String> = variants
        .iter()
        .map(|v| format!("{}({})", v.name, format_typed_params(&v.fields)))
        .collect();
    rendered.sort();
    rendered.join(",")
}

fn format_associated_types(items: &[(String, Option<TypeExpr>)]) -> String {
    let mut rendered: Vec<String> = items
        .iter()
        .map(|(name, bound)| match bound {
            Some(ty) => format!("{name}:{}", format_type_expr(ty)),
            None => name.clone(),
        })
        .collect();
    rendered.sort();
    rendered.join(",")
}

fn format_interface_methods(methods: &[InterfaceMethod]) -> String {
    let mut rendered: Vec<String> = methods
        .iter()
        .map(|m| {
            format!(
                "{}{}({}){}",
                m.name,
                format_type_params(&m.type_params),
                format_typed_params(&m.params),
                format_return(&m.return_type),
            )
        })
        .collect();
    rendered.sort();
    rendered.join(",")
}

fn format_type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Union(parts) => {
            let mut rendered: Vec<String> = parts.iter().map(format_type_expr).collect();
            rendered.sort();
            format!("({})", rendered.join("|"))
        }
        TypeExpr::Intersection(parts) => {
            let mut rendered: Vec<String> = parts.iter().map(format_type_expr).collect();
            rendered.sort();
            format!("({})", rendered.join("&"))
        }
        TypeExpr::Shape(fields) => format!("{{{}}}", format_shape_fields(fields)),
        TypeExpr::OpenShape { fields, rests } => {
            let tails: Vec<String> = rests
                .iter()
                .map(|r| format!("...{}", format_type_expr(r)))
                .collect();
            format!("{{{}|{}}}", format_shape_fields(fields), tails.join(","))
        }
        TypeExpr::List(inner) => format!("list<{}>", format_type_expr(inner)),
        TypeExpr::DictType(k, v) => {
            format!("dict<{},{}>", format_type_expr(k), format_type_expr(v))
        }
        TypeExpr::Iter(inner) => format!("iter<{}>", format_type_expr(inner)),
        TypeExpr::Generator(inner) => format!("Generator<{}>", format_type_expr(inner)),
        TypeExpr::Stream(inner) => format!("Stream<{}>", format_type_expr(inner)),
        TypeExpr::Applied { name, args } => {
            let rendered: Vec<String> = args.iter().map(format_type_expr).collect();
            format!("{name}<{}>", rendered.join(","))
        }
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            let rendered: Vec<String> = params.iter().map(format_type_expr).collect();
            format!(
                "fn({})->{}",
                rendered.join(","),
                format_type_expr(return_type)
            )
        }
        TypeExpr::Never => "Never".to_string(),
        TypeExpr::LitString(s) => format!("\"{s}\""),
        TypeExpr::LitInt(n) => n.to_string(),
        TypeExpr::Owned(inner) => format!("owned<{}>", format_type_expr(inner)),
    }
}

fn format_shape_fields(fields: &[ShapeField]) -> String {
    let mut rendered: Vec<String> = fields
        .iter()
        .map(|f| {
            let opt = if f.optional { "?" } else { "" };
            format!("{}{opt}:{}", f.name, format_type_expr(&f.type_expr))
        })
        .collect();
    rendered.sort();
    rendered.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(source: &str) -> Fingerprint {
        fingerprint_source(source).expect("source parses")
    }

    #[test]
    fn private_body_change_does_not_flip_fingerprint() {
        let before = fp("pub fn add(a: int, b: int) -> int { a + b }\n");
        let after = fp("pub fn add(a: int, b: int) -> int { let s = a + b; s }\n");
        assert_eq!(before, after);
    }

    #[test]
    fn private_helper_does_not_flip_fingerprint() {
        let before = fp("pub fn entry() { internal() }\nfn internal() { 1 }\n");
        let after = fp("pub fn entry() { internal() }\nfn internal() { 2 }\nfn extra() { 3 }\n");
        assert_eq!(before, after);
    }

    #[test]
    fn reordering_public_decls_does_not_flip_fingerprint() {
        let a = fp("pub fn alpha() {}\npub fn beta() {}\n");
        let b = fp("pub fn beta() {}\npub fn alpha() {}\n");
        assert_eq!(a, b);
    }

    #[test]
    fn changing_public_signature_flips_fingerprint() {
        let before = fp("pub fn add(a: int, b: int) -> int { a + b }\n");
        let after = fp("pub fn add(a: int, b: int, c: int) -> int { a + b + c }\n");
        assert_ne!(before, after);
    }

    #[test]
    fn changing_public_return_type_flips_fingerprint() {
        let before = fp("pub fn make() -> string { \"x\" }\n");
        let after = fp("pub fn make() -> int { 1 }\n");
        assert_ne!(before, after);
    }

    #[test]
    fn adding_pub_struct_field_flips_fingerprint() {
        let before = fp("pub struct Point { x: int, y: int }\n");
        let after = fp("pub struct Point { x: int, y: int, z: int }\n");
        assert_ne!(before, after);
    }

    #[test]
    fn pub_re_export_change_flips_fingerprint() {
        let before = fp("pub import { foo } from \"./a\"\n");
        let after = fp("pub import { foo, bar } from \"./a\"\n");
        assert_ne!(before, after);
    }

    #[test]
    fn adding_pub_decl_flips_fingerprint() {
        let before = fp("pub fn alpha() {}\n");
        let after = fp("pub fn alpha() {}\npub fn beta() {}\n");
        assert_ne!(before, after);
    }

    #[test]
    fn changing_only_non_pub_imports_does_not_flip_fingerprint() {
        let before = fp("import \"./a\"\npub fn entry() {}\n");
        let after = fp("import \"./b\"\npub fn entry() {}\n");
        // Private imports affect what _this_ module sees but not what
        // downstreams see, so they're outside the public surface.
        assert_eq!(before, after);
    }

    #[test]
    fn hex_is_64_chars() {
        let h = fingerprint_hex(&fp("pub fn x() {}\n"));
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
