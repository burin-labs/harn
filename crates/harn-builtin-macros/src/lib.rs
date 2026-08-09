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
use syn::{parse_macro_input, Expr, ExprLit, Ident, ItemFn, Lit, LitBool, LitStr, Meta, Token};

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
/// - `exposure = "pure" | "runtime_internal" | "privileged_wire" |
///   "harness.<capability>.<method>"` — closed source-visibility contract.
/// - `effects = ["fs.read@arg0", "fs.write@arg0+arg1", ...]` — typed effect
///   rows. Selectors are `argN`, `argN.field.path`, `eachN`, `const=VALUE`,
///   or `dynamic`. `effects = []` is an explicit purity declaration.
/// - `effects_authorized_by = "llm.call"` — an explicit capability grant that
///   may authorize this builtin's read-only declared effects.
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

/// Declare one method-dispatched capability surface without manufacturing a
/// runtime handler. The declaration contributes the same `BuiltinDef` shape
/// as `#[harn_builtin]`, so every consumer reads one manifest.
#[proc_macro]
pub fn harn_capability_method(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as CapabilityMethodInput);
    match expand_capability_method(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Declare a capability method in the dependency-leaf contract crate.
#[proc_macro]
pub fn harn_capability_contract(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as CapabilityMethodInput);
    match expand_leaf_capability_contract(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

struct CapabilityMethodInput {
    rust_name: Ident,
    exposure: LitStr,
    effects: Vec<LitStr>,
    signature: Expr,
    doc: LitStr,
    effects_authorized_by: Option<LitStr>,
}

impl Parse for CapabilityMethodInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let rust_name = input.parse()?;
        input.parse::<Token![,]>()?;
        let exposure = input.parse()?;
        input.parse::<Token![,]>()?;
        let effects_expr: Expr = input.parse()?;
        let effects = parse_str_array(&effects_expr)?;
        input.parse::<Token![,]>()?;
        let signature = input.parse()?;
        input.parse::<Token![,]>()?;
        let doc = input.parse()?;
        let effects_authorized_by = if input.is_empty() {
            None
        } else {
            input.parse::<Token![,]>()?;
            Some(input.parse()?)
        };
        if !input.is_empty() {
            return Err(input.error("unexpected capability method tokens"));
        }
        Ok(Self {
            rust_name,
            exposure,
            effects,
            signature,
            doc,
            effects_authorized_by,
        })
    }
}

fn expand_capability_method(input: CapabilityMethodInput) -> syn::Result<TokenStream2> {
    let support = quote!(crate::stdlib::macros);
    let (sig_expr, signature_text, signature_attr) = match &input.signature {
        Expr::Lit(ExprLit {
            lit: Lit::Str(signature),
            ..
        }) => (
            sig_parser::parse_sig(&signature.value(), signature.span(), &support)?,
            Some(signature.value()),
            Some(signature.clone()),
        ),
        expression => (quote!(#expression), None, None),
    };
    let attrs = BuiltinAttrs {
        sig: signature_attr,
        exposure: Some(input.exposure),
        effects: input.effects,
        effects_declared: true,
        effects_authorized_by: input.effects_authorized_by,
        parser_only: true,
        ..BuiltinAttrs::default()
    };
    let contract = contract_expr(&attrs, &support)?;
    let upper = input.rust_name.to_string().to_uppercase();
    let def_ident = format_ident!("{upper}_DEF");
    let link_ident = format_ident!("__{upper}_LINKME");
    let doc = input.doc.value();
    let signature_text_expr = match signature_text {
        Some(signature) => quote!(::core::option::Option::Some(#signature)),
        None => quote!(::core::option::Option::None),
    };
    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static #def_ident: #support::VmBuiltinDef = #support::VmBuiltinDef {
            sig: #sig_expr,
            contract: #contract,
            aliases: &[],
            handler: #support::VmBuiltinHandler::None,
            category: ::core::option::Option::Some("capability"),
            doc: ::core::option::Option::Some(#doc),
            signature_text: #signature_text_expr,
            parser_only: true,
            runtime_only: false,
        };

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        #[#support::distributed_slice(#support::ALL_BUILTIN_DEFS)]
        static #link_ident: &'static #support::VmBuiltinDef = &#def_ident;
    })
}

fn expand_leaf_capability_contract(input: CapabilityMethodInput) -> syn::Result<TokenStream2> {
    let support = quote!(crate::support);
    let (sig_expr, signature_text, signature_attr) = match &input.signature {
        Expr::Lit(ExprLit {
            lit: Lit::Str(signature),
            ..
        }) => (
            sig_parser::parse_sig(&signature.value(), signature.span(), &support)?,
            Some(signature.value()),
            Some(signature.clone()),
        ),
        expression => (quote!(#expression), None, None),
    };
    let attrs = BuiltinAttrs {
        sig: signature_attr,
        exposure: Some(input.exposure),
        effects: input.effects,
        effects_declared: true,
        effects_authorized_by: input.effects_authorized_by,
        parser_only: true,
        ..BuiltinAttrs::default()
    };
    let contract = contract_expr(&attrs, &support)?;
    let upper = input.rust_name.to_string().to_uppercase();
    let def_ident = format_ident!("{upper}_DEF");
    let doc = input.doc.value();
    let signature_text_expr = match signature_text {
        Some(signature) => quote!(::core::option::Option::Some(#signature)),
        None => quote!(::core::option::Option::None),
    };
    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static #def_ident: #support::CapabilityMethodDef = #support::CapabilityMethodDef {
            signature: #sig_expr,
            contract: #contract,
            doc: #doc,
            signature_text: #signature_text_expr,
        };
    })
}

#[derive(Debug, Default)]
struct BuiltinAttrs {
    sig: Option<LitStr>,
    sig_expr: Option<Expr>,
    aliases: Vec<LitStr>,
    exposure: Option<LitStr>,
    effects: Vec<LitStr>,
    effects_declared: bool,
    effects_authorized_by: Option<LitStr>,
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
                        "exposure" => out.exposure = Some(parse_lit_str(&nv.value)?),
                        "effects" => {
                            out.effects = parse_str_array(&nv.value)?;
                            out.effects_declared = true;
                        }
                        "effects_authorized_by" => {
                            out.effects_authorized_by = Some(parse_lit_str(&nv.value)?);
                        }
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
        if out.exposure.is_some() != out.effects_declared {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`exposure` and `effects` must be declared together",
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
    let contract_expr = contract_expr(&attrs, &support)?;

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
            contract: #contract_expr,
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

fn contract_expr(attrs: &BuiltinAttrs, support: &TokenStream2) -> syn::Result<TokenStream2> {
    let Some(exposure) = attrs.exposure.as_ref() else {
        return Ok(quote!(#support::BuiltinContract::UNDECLARED));
    };

    let effects = attrs
        .effects
        .iter()
        .map(|effect| parse_effect_spec(&effect.value(), effect.span(), support))
        .collect::<syn::Result<Vec<_>>>()?;
    let effects = quote!(&[#(#effects),*]);
    if let Some(authority) = attrs.effects_authorized_by.as_ref() {
        if attrs.effects.is_empty() {
            return Err(syn::Error::new(
                authority.span(),
                "`effects_authorized_by` requires at least one declared effect",
            ));
        }
        if let Some(effect) = attrs.effects.iter().find(|effect| {
            let head = effect.value();
            let access = head
                .split_once('@')
                .map_or(head.as_str(), |(head, _)| head)
                .split_once('.')
                .map(|(_, access)| access);
            !matches!(access, Some("read" | "observe"))
        }) {
            return Err(syn::Error::new(
                effect.span(),
                "`effects_authorized_by` may only delegate read or observe effects",
            ));
        }
    }
    let raw = exposure.value();
    if attrs.effects_authorized_by.is_some() && !raw.starts_with("harness.") {
        return Err(syn::Error::new(
            exposure.span(),
            "`effects_authorized_by` is only valid for Harness methods",
        ));
    }
    match raw.as_str() {
        "pure" => {
            if !attrs.effects.is_empty() {
                return Err(syn::Error::new(
                    exposure.span(),
                    "pure builtins must declare `effects = []`",
                ));
            }
            Ok(quote!(#support::BuiltinContract::PURE))
        }
        "runtime_internal" => {
            if !attrs.effects.is_empty() {
                return Err(syn::Error::new(
                    exposure.span(),
                    "runtime-internal builtins cannot declare script effects",
                ));
            }
            Ok(quote!(#support::BuiltinContract::RUNTIME_INTERNAL))
        }
        "privileged_wire" => Ok(quote!(#support::BuiltinContract::privileged_wire(#effects))),
        _ => {
            if let Some(index) = raw.strip_prefix("capability_arg:") {
                let authority_argument = index.parse::<u16>().map_err(|_| {
                    syn::Error::new(
                        exposure.span(),
                        "capability argument exposure must be `capability_arg:<index>`",
                    )
                })?;
                if attrs.effects.is_empty() {
                    return Err(syn::Error::new(
                        exposure.span(),
                        "capability argument builtins must declare at least one effect",
                    ));
                }
                return Ok(quote!(
                    #support::BuiltinContract::capability_function(
                        #authority_argument,
                        #effects,
                    )
                ));
            }
            let Some(rest) = raw.strip_prefix("harness.") else {
                return Err(syn::Error::new(
                    exposure.span(),
                    "unknown exposure; expected `pure`, `runtime_internal`, \
                     `privileged_wire`, `capability_arg:<index>`, or \
                     `harness.<capability>.<method>`",
                ));
            };
            let Some((capability, method)) = rest.split_once('.') else {
                return Err(syn::Error::new(
                    exposure.span(),
                    "harness exposure must be `harness.<capability>.<method>`",
                ));
            };
            if method.is_empty() || method.contains('.') {
                return Err(syn::Error::new(
                    exposure.span(),
                    "harness method must be one non-empty identifier",
                ));
            }
            let capability = capability_expr(capability, exposure.span(), support)?;
            if let Some(authority) = attrs.effects_authorized_by.as_ref() {
                let raw_authority = authority.value();
                let Some((authority_capability, authority_operation)) =
                    raw_authority.split_once('.')
                else {
                    return Err(syn::Error::new(
                        authority.span(),
                        "effect authority must be `<capability>.<operation>`",
                    ));
                };
                if authority_operation.is_empty() || authority_operation.contains('.') {
                    return Err(syn::Error::new(
                        authority.span(),
                        "effect authority operation must be one non-empty identifier",
                    ));
                }
                let authority_capability =
                    capability_expr(authority_capability, authority.span(), support)?;
                Ok(
                    quote!(#support::BuiltinContract::harness_with_effect_authorization(
                        #capability,
                        #method,
                        #effects,
                        #support::EffectAuthorization::new(
                            #authority_capability,
                            #authority_operation,
                        ),
                    )),
                )
            } else {
                Ok(quote!(#support::BuiltinContract::harness(
                    #capability,
                    #method,
                    #effects,
                )))
            }
        }
    }
}

fn capability_expr(
    name: &str,
    span: proc_macro2::Span,
    support: &TokenStream2,
) -> syn::Result<TokenStream2> {
    let variant = harn_builtin_meta::CapabilityId::from_field_name(name)
        .map(harn_builtin_meta::CapabilityId::variant_name)
        .ok_or_else(|| syn::Error::new(span, format!("unknown harness capability `{name}`")))?;
    let ident = format_ident!("{variant}");
    Ok(quote!(#support::CapabilityId::#ident))
}

fn parse_effect_spec(
    raw: &str,
    span: proc_macro2::Span,
    support: &TokenStream2,
) -> syn::Result<TokenStream2> {
    let (head, selectors) = raw
        .split_once('@')
        .map_or((raw, None), |(head, selectors)| (head, Some(selectors)));
    let Some((kind, access)) = head.split_once('.') else {
        return Err(syn::Error::new(
            span,
            "effect must be `<kind>.<access>` with optional `@selectors`",
        ));
    };
    let kind = match kind {
        "stdio" => quote!(#support::EffectKind::Stdio),
        "fs" => quote!(#support::EffectKind::Fs),
        "env" => quote!(#support::EffectKind::Env),
        "clock" => quote!(#support::EffectKind::Clock),
        "random" => quote!(#support::EffectKind::Random),
        "network" => quote!(#support::EffectKind::Network),
        "process" => quote!(#support::EffectKind::Process),
        "llm" => quote!(#support::EffectKind::Llm),
        "tool" => quote!(#support::EffectKind::Tool),
        "mcp" => quote!(#support::EffectKind::Mcp),
        "host" => quote!(#support::EffectKind::Host),
        "authority" => quote!(#support::EffectKind::Authority),
        "worker" => quote!(#support::EffectKind::Worker),
        "secret" => quote!(#support::EffectKind::Secret),
        "observability" => quote!(#support::EffectKind::Observability),
        "channel" => quote!(#support::EffectKind::Channel),
        "state" => quote!(#support::EffectKind::State),
        _ => {
            return Err(syn::Error::new(
                span,
                format!("unknown effect kind `{kind}`"),
            ))
        }
    };
    let access = match access {
        "read" => quote!(#support::EffectAccess::Read),
        "write" => quote!(#support::EffectAccess::Write),
        "mutate" => quote!(#support::EffectAccess::Mutate),
        "observe" => quote!(#support::EffectAccess::Observe),
        _ => {
            return Err(syn::Error::new(
                span,
                format!("unknown effect access `{access}`"),
            ))
        }
    };
    let selectors = selectors
        .filter(|selectors| !selectors.is_empty())
        .map(|selectors| {
            selectors
                .split('+')
                .map(|selector| parse_resource_selector(selector, span, support))
                .collect::<syn::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(quote!(#support::EffectSpec::new(
        #kind,
        #access,
        &[#(#selectors),*],
    )))
}

fn parse_resource_selector(
    raw: &str,
    span: proc_macro2::Span,
    support: &TokenStream2,
) -> syn::Result<TokenStream2> {
    if raw == "dynamic" {
        return Ok(quote!(#support::ResourceSelector::Dynamic));
    }
    if let Some(value) = raw.strip_prefix("const=") {
        if value.is_empty() {
            return Err(syn::Error::new(span, "constant selector cannot be empty"));
        }
        return Ok(quote!(#support::ResourceSelector::Constant(#value)));
    }
    if let Some(index) = raw.strip_prefix("each") {
        let index = parse_selector_index(index, span)?;
        return Ok(quote!(#support::ResourceSelector::EachArgument(#index)));
    }
    let Some(rest) = raw.strip_prefix("arg") else {
        return Err(syn::Error::new(
            span,
            format!("unknown resource selector `{raw}`"),
        ));
    };
    let mut parts = rest.split('.');
    let index = parse_selector_index(parts.next().unwrap_or_default(), span)?;
    let path = parts.collect::<Vec<_>>();
    if path.is_empty() {
        Ok(quote!(#support::ResourceSelector::Argument(#index)))
    } else if path.iter().any(|part| part.is_empty()) {
        Err(syn::Error::new(span, "resource field path cannot be empty"))
    } else {
        Ok(quote!(#support::ResourceSelector::Field {
            argument: #index,
            path: &[#(#path),*],
        }))
    }
}

fn parse_selector_index(raw: &str, span: proc_macro2::Span) -> syn::Result<u16> {
    raw.parse::<u16>()
        .map_err(|_| syn::Error::new(span, format!("invalid argument index `{raw}`")))
}
