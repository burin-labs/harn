//! `#[harn_builtin]` proc-macro.
//!
//! Annotates a Rust function that implements one builtin and emits both a
//! runtime registration entry and a parser `BuiltinSignature` from a single
//! declaration. This is the only supported way to register stdlib builtins —
//! see `CONTRIBUTING.md` ("Adding a stdlib builtin") for the wire-up
//! checklist and `crates/harn-vm/src/stdlib/bytes.rs`, `runtime_scope.rs`,
//! and `strings.rs` for sync, async, and `aliases = [...]` examples
//! respectively. The macro contributes each emitted `VmBuiltinDef` to the
//! workspace-global `ALL_BUILTIN_DEFS` linkme distributed slice, so simply
//! annotating a fn (in a module already pulled into `harn-vm`) is enough to
//! make it land in the registry — no per-module aggregation edits required.

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{parse_macro_input, Expr, ItemFn, LitBool, LitStr, Meta, Token};

mod sig_parser;

/// Marks a Rust function as the runtime handler for a Harn builtin. Emits a
/// sibling `static <NAME>_DEF: harn_vm::stdlib::macros::VmBuiltinDef = ...`
/// containing the signature, aliases, handler pointer, and metadata.
///
/// # Attribute keys
///
/// - `sig = "name(a: dict, b: dict) -> dict"` — Harn-style signature parsed
///   into a `BuiltinSignature`. Mutually exclusive with `sig_expr`.
/// - `sig_expr = <Rust expr returning BuiltinSignature>` — full struct
///   literal used verbatim. Escape hatch for shapes, complex generics, etc.
/// - `aliases = ["__foo"]` — additional names sharing this impl + signature.
/// - `category = "collections"` — observability label (optional).
/// - `kind = "sync" | "async"` — defaults to `sync`. `async` wraps the user
///   fn into `Pin<Box<dyn Future<...>>>`.
/// - `parser_only = true` — emit only the signature; no runtime registration.
/// - `runtime_only = true` — emit only the runtime entry; signature suppressed.
/// - `doc = "..."` — override doc string (defaults to the fn's `///` block).
#[proc_macro_attribute]
pub fn harn_builtin(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as BuiltinAttrs);
    let item_fn = parse_macro_input!(item as ItemFn);
    match expand(attrs, item_fn) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[derive(Debug, Default)]
struct BuiltinAttrs {
    sig: Option<LitStr>,
    sig_expr: Option<Expr>,
    aliases: Vec<LitStr>,
    category: Option<LitStr>,
    kind: BuiltinKind,
    parser_only: bool,
    runtime_only: bool,
    doc: Option<LitStr>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum BuiltinKind {
    #[default]
    Sync,
    Async,
}

impl Parse for BuiltinAttrs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut out = BuiltinAttrs::default();
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in metas {
            match &meta {
                Meta::NameValue(nv) => {
                    let key = nv
                        .path
                        .get_ident()
                        .ok_or_else(|| syn::Error::new(nv.path.span(), "expected identifier key"))?
                        .to_string();
                    match key.as_str() {
                        "sig" => out.sig = Some(parse_lit_str(&nv.value)?),
                        "sig_expr" => out.sig_expr = Some(nv.value.clone()),
                        "category" => out.category = Some(parse_lit_str(&nv.value)?),
                        "doc" => out.doc = Some(parse_lit_str(&nv.value)?),
                        "kind" => {
                            let s = parse_lit_str(&nv.value)?;
                            out.kind = match s.value().as_str() {
                                "sync" => BuiltinKind::Sync,
                                "async" => BuiltinKind::Async,
                                other => {
                                    return Err(syn::Error::new(
                                        s.span(),
                                        format!(
                                            "unknown kind {other:?}, expected \"sync\" or \"async\""
                                        ),
                                    ));
                                }
                            };
                        }
                        "parser_only" => out.parser_only = parse_lit_bool(&nv.value)?,
                        "runtime_only" => out.runtime_only = parse_lit_bool(&nv.value)?,
                        "aliases" => out.aliases = parse_str_array(&nv.value)?,
                        other => {
                            return Err(syn::Error::new(
                                nv.path.span(),
                                format!("unknown #[harn_builtin] key: {other}"),
                            ));
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "expected key = value attributes",
                    ))
                }
            }
        }
        if let (Some(sig_lit), Some(_)) = (out.sig.as_ref(), out.sig_expr.as_ref()) {
            return Err(syn::Error::new(
                sig_lit.span(),
                "specify either `sig` (Harn-style string) or `sig_expr` (raw Rust expression), not both",
            ));
        }
        if out.sig.is_none() && out.sig_expr.is_none() && !out.runtime_only {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[harn_builtin] requires `sig = \"...\"`, `sig_expr = ...`, or `runtime_only = true`",
            ));
        }
        Ok(out)
    }
}

fn parse_lit_str(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(s.clone()),
        other => Err(syn::Error::new(other.span(), "expected string literal")),
    }
}

fn parse_lit_bool(expr: &Expr) -> syn::Result<bool> {
    match expr {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(LitBool { value, .. }),
            ..
        }) => Ok(*value),
        other => Err(syn::Error::new(other.span(), "expected boolean literal")),
    }
}

fn parse_str_array(expr: &Expr) -> syn::Result<Vec<LitStr>> {
    match expr {
        Expr::Array(arr) => arr.elems.iter().map(parse_lit_str).collect(),
        Expr::Reference(r) => parse_str_array(&r.expr),
        other => Err(syn::Error::new(
            other.span(),
            "expected array of string literals, e.g. [\"alias1\", \"alias2\"]",
        )),
    }
}

fn expand(attrs: BuiltinAttrs, item_fn: ItemFn) -> syn::Result<TokenStream2> {
    let fn_name = &item_fn.sig.ident;
    let def_ident = format_ident!("{}_DEF", fn_name.to_string().to_uppercase());
    let support = quote!(crate::stdlib::macros);

    // Build the BuiltinSignature expression.
    let sig_expr = if let Some(expr) = &attrs.sig_expr {
        quote!(#expr)
    } else if let Some(sig_lit) = &attrs.sig {
        sig_parser::parse_sig(&sig_lit.value(), sig_lit.span(), &support)?
    } else {
        // runtime_only — emit a placeholder signature with the fn name.
        let name_str = fn_name.to_string();
        quote!(#support::BuiltinSignature::simple(
            #name_str,
            &[],
            #support::TY_ANY,
        ))
    };

    // Surface the human-readable sig text (e.g. `"foo(a: dict) -> dict"`)
    // through to the runtime metadata layer so `harn explain` /
    // `harn-vm-tools` / the alignment-test metadata check keep parity
    // with the pre-migration DSL builder.
    let signature_text_expr = match &attrs.sig {
        Some(sig_lit) => {
            let raw = sig_lit.value();
            quote!(::core::option::Option::Some(#raw))
        }
        None => quote!(::core::option::Option::None),
    };

    let aliases = attrs.aliases.iter().map(|s| quote!(#s));
    let aliases_arr = quote!(&[#(#aliases),*]);

    let category = match &attrs.category {
        Some(c) => {
            let v = c.value();
            quote!(::core::option::Option::Some(#v))
        }
        None => quote!(::core::option::Option::None),
    };

    // Doc: explicit override, else extract from /// comments on the fn.
    let doc = if let Some(d) = &attrs.doc {
        let v = d.value();
        quote!(::core::option::Option::Some(#v))
    } else {
        let collected: String = item_fn
            .attrs
            .iter()
            .filter_map(|a| {
                if a.path().is_ident("doc") {
                    if let Meta::NameValue(nv) = &a.meta {
                        if let Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            return Some(s.value().trim().to_string());
                        }
                    }
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n");
        if collected.is_empty() {
            quote!(::core::option::Option::None)
        } else {
            quote!(::core::option::Option::Some(#collected))
        }
    };

    let parser_only = attrs.parser_only;
    let runtime_only = attrs.runtime_only;

    // Handler wiring depends on sync vs async. For `async fn` user
    // functions we emit a sibling thunk that boxes the future to match the
    // `AsyncHandler` signature.
    let async_thunk_ident = format_ident!("__harn_async_wrap_{}", fn_name);
    let (handler_expr, extra_items) = match (attrs.kind, attrs.parser_only) {
        (_, true) => (quote!(#support::VmBuiltinHandler::None), quote!()),
        (BuiltinKind::Sync, _) => (quote!(#support::VmBuiltinHandler::Sync(#fn_name)), quote!()),
        (BuiltinKind::Async, _) => {
            // Async builtins receive an explicit `AsyncBuiltinCtx` handle as
            // their first parameter (harn#2668). The macro threads it from the
            // dispatch loop into the user fn so handler bodies mint child VMs /
            // forward output through the ctx they were given, never an ambient
            // task-local.
            let is_async_fn = item_fn.sig.asyncness.is_some();
            if is_async_fn {
                let thunk = quote! {
                    #[doc(hidden)]
                    #[allow(non_snake_case)]
                    fn #async_thunk_ident(
                        ctx: crate::vm::AsyncBuiltinCtx,
                        args: ::std::vec::Vec<#support::VmValue>,
                    ) -> #support::AsyncBuiltinFuture {
                        ::std::boxed::Box::pin(#fn_name(ctx, args))
                    }
                };
                (
                    quote!(#support::VmBuiltinHandler::Async(#async_thunk_ident)),
                    thunk,
                )
            } else {
                (
                    quote!(#support::VmBuiltinHandler::Async(#fn_name)),
                    quote!(),
                )
            }
        }
    };

    // Sibling linkme entry that registers `#def_ident` into the
    // workspace-global `ALL_BUILTIN_DEFS` distributed slice — eliminates
    // the need for per-module `MODULE_BUILTINS` arrays + a hand-maintained
    // aggregator in `stdlib.rs`. The entry name is derived from the def
    // identifier so two builtins in different modules never collide on
    // the static name.
    let link_ident = format_ident!("__{}_LINKME", fn_name.to_string().to_uppercase());

    let out = quote! {
        #item_fn

        #extra_items

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static #def_ident: #support::VmBuiltinDef = #support::VmBuiltinDef {
            sig: #sig_expr,
            aliases: #aliases_arr,
            handler: #handler_expr,
            category: #category,
            doc: #doc,
            signature_text: #signature_text_expr,
            parser_only: #parser_only,
            runtime_only: #runtime_only,
        };

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        #[#support::distributed_slice(#support::ALL_BUILTIN_DEFS)]
        static #link_ident: &'static #support::VmBuiltinDef = &#def_ident;
    };
    Ok(out)
}
