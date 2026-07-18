//! `define_opcodes!` function-like proc-macro.
//!
//! Single source of truth for the VM's opcode set. One invocation in
//! `harn-vm/src/chunk.rs` emits the `Op` enum, the byte-to-variant mapping,
//! the sync and async dispatch tables, the disassembly renderer, and the
//! classification helpers (`op_reads_outer_name`, `is_adaptive_binary_op`).
//! Adding or renaming an opcode is a one-line edit; generated tables keep
//! dispatch, disassembly, and classification coverage aligned.

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{braced, bracketed, parenthesized, parse_macro_input, Expr, Ident, LitStr, Token};

/// Parse a single opcode entry. Syntax:
///
/// ```ignore
/// VariantName {
///     <kind>(<expr>[, <expr>]),
///     disasm: <helper>("<LABEL>"),
///     flags: [<flag>, ...]   // optional
/// };
/// ```
///
/// where `<kind>` is one of:
/// - `sync(expr)` — `expr: Result<(), VmError>`
/// - `sync_void(expr)` — `expr: ()`, wrapped to `Ok(())`
/// - `sync_return(expr)` — `expr: VmError`, wrapped to `return Some(Err(expr))`
/// - `split(sync_expr, async_expr)` — sync path returns `Option<Result>`,
///   async path returns `Result<(), VmError>`
/// - `async_op(expr)` — `expr: Result<(), VmError>`, only dispatched in async
///   table; sync table emits `return None`. Named `async_op` instead of
///   `async` to avoid the reserved-keyword friction even though `syn` would
///   tolerate the raw identifier.
struct OpcodeEntry {
    variant: Ident,
    dispatch: Dispatch,
    disasm: Disasm,
    flags: Vec<Ident>,
}

enum Dispatch {
    Sync(Expr),
    SyncVoid(Expr),
    SyncReturn(Expr),
    Split { sync: Expr, async_: Expr },
    Async(Expr),
}

struct Disasm {
    helper: Ident,
    label: LitStr,
}

impl Parse for OpcodeEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let variant: Ident = input.parse()?;
        let body;
        braced!(body in input);

        let kind: Ident = body.parse()?;
        let kind_args;
        parenthesized!(kind_args in body);

        let dispatch = match kind.to_string().as_str() {
            "sync" => {
                let expr: Expr = kind_args.parse()?;
                Dispatch::Sync(expr)
            }
            "sync_void" => {
                let expr: Expr = kind_args.parse()?;
                Dispatch::SyncVoid(expr)
            }
            "sync_return" => {
                let expr: Expr = kind_args.parse()?;
                Dispatch::SyncReturn(expr)
            }
            "split" => {
                let sync: Expr = kind_args.parse()?;
                kind_args.parse::<Token![,]>()?;
                let async_: Expr = kind_args.parse()?;
                Dispatch::Split { sync, async_ }
            }
            "async_op" => {
                let expr: Expr = kind_args.parse()?;
                Dispatch::Async(expr)
            }
            other => {
                return Err(syn::Error::new(
                    kind.span(),
                    format!(
                        "unknown opcode kind `{other}` — expected one of: \
                         sync, sync_void, sync_return, split, async_op"
                    ),
                ));
            }
        };

        body.parse::<Token![,]>()?;
        let disasm_kw: Ident = body.parse()?;
        if disasm_kw != "disasm" {
            return Err(syn::Error::new(
                disasm_kw.span(),
                "expected `disasm:` after dispatch kind",
            ));
        }
        body.parse::<Token![:]>()?;
        let helper: Ident = body.parse()?;
        let helper_args;
        parenthesized!(helper_args in body);
        let label: LitStr = helper_args.parse()?;
        let disasm = Disasm { helper, label };

        let mut flags = Vec::new();
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
            if !body.is_empty() {
                let flags_kw: Ident = body.parse()?;
                if flags_kw != "flags" {
                    return Err(syn::Error::new(
                        flags_kw.span(),
                        "expected `flags: [...]` after `disasm`",
                    ));
                }
                body.parse::<Token![:]>()?;
                let flags_inner;
                bracketed!(flags_inner in body);
                let parsed: Punctuated<Ident, Token![,]> =
                    Punctuated::parse_terminated(&flags_inner)?;
                flags = parsed.into_iter().collect();
            }
        }

        Ok(Self {
            variant,
            dispatch,
            disasm,
            flags,
        })
    }
}

struct Opcodes {
    entries: Vec<OpcodeEntry>,
}

impl Parse for Opcodes {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        while !input.is_empty() {
            let entry: OpcodeEntry = input.parse()?;
            input.parse::<Token![;]>()?;
            entries.push(entry);
        }
        Ok(Self { entries })
    }
}

/// Emit the VM opcode definitions from a centralized declarative table.
///
/// See the crate-level doc for syntax. Generated outputs:
/// - `pub enum Op { ... }` (`#[repr(u8)]`, `Debug + Clone + Copy + Eq`)
/// - `impl Op { const ALL: &[Self]; const COUNT: usize; fn from_byte(...) -> Option<Self> }`
/// - `impl crate::vm::Vm { fn execute_op_sync(...); fn execute_op_async(...) }`
/// - `impl crate::chunk::Chunk { fn disassemble_op(...) }`
/// - One free `pub(crate) fn <flag>(op: Op) -> bool` per declared flag.
///   The well-known flags `reads_outer_name` and `adaptive_binary` map
///   to `op_reads_outer_name` and `is_adaptive_binary_op` respectively
///   so existing call sites need no renames.
#[proc_macro]
pub fn define_opcodes(input: TokenStream) -> TokenStream {
    let opcodes = parse_macro_input!(input as Opcodes);
    match expand(&opcodes) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(opcodes: &Opcodes) -> syn::Result<TokenStream2> {
    if opcodes.entries.is_empty() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "define_opcodes! requires at least one opcode entry",
        ));
    }

    let variants = opcodes.entries.iter().map(|e| &e.variant);
    let all_variants: Vec<_> = opcodes.entries.iter().map(|e| &e.variant).collect();

    // Sync dispatch arms.
    let sync_arms = opcodes.entries.iter().map(|e| {
        let v = &e.variant;
        match &e.dispatch {
            Dispatch::Sync(expr) => quote!(Op::#v => #expr,),
            Dispatch::SyncVoid(expr) => quote!(Op::#v => { #expr; Ok(()) },),
            Dispatch::SyncReturn(expr) => quote!(Op::#v => return ::core::option::Option::Some(::core::result::Result::Err(#expr)),),
            Dispatch::Split { sync, .. } => quote!(Op::#v => return #sync,),
            Dispatch::Async(_) => quote!(Op::#v => return ::core::option::Option::None,),
        }
    });

    // Async dispatch arms — only `async` and `split` kinds appear here.
    // Sync-only opcodes hit a debug_assert!-guarded panic to flag any
    // out-of-sync caller (the hot loop never calls execute_op_async for
    // a sync opcode because execute_op_sync returns Some(...) for them).
    let async_arms = opcodes.entries.iter().filter_map(|e| {
        let v = &e.variant;
        match &e.dispatch {
            Dispatch::Async(expr) => Some(quote!(Op::#v => #expr,)),
            Dispatch::Split { async_, .. } => Some(quote!(Op::#v => #async_,)),
            _ => None,
        }
    });

    // Disasm arms — call the per-opcode helper with the label.
    // Helpers live in `crate::chunk` as `pub(crate) fn disasm_<helper>(...)`
    // and take `(&Chunk, &mut usize, &str)`. Fully-qualifying the path
    // means this dispatch table can be emitted into any module without
    // forcing a `use crate::chunk::*` at the call site.
    let disasm_arms = opcodes.entries.iter().map(|e| {
        let v = &e.variant;
        let helper = format_ident!("disasm_{}", e.disasm.helper);
        let label = &e.disasm.label;
        quote!(Op::#v => crate::chunk::#helper(self, ip, #label),)
    });

    // Flag-derived classification helpers. Collect the set of distinct
    // flag names declared across all entries so each generated predicate
    // has a contributor list and zero-cost `matches!` macro expansion.
    let mut flag_groups: std::collections::BTreeMap<String, Vec<&Ident>> =
        std::collections::BTreeMap::new();
    for entry in &opcodes.entries {
        for flag in &entry.flags {
            flag_groups
                .entry(flag.to_string())
                .or_default()
                .push(&entry.variant);
        }
    }

    // Each flag becomes a free `fn` at module scope, named for the flag.
    // Hand-rolled mapping ties into the existing call sites (chunk.rs,
    // arithmetic.rs) so the macro replaces the predicate definitions
    // without forcing every caller to switch namespaces.
    let flag_fns = flag_groups.iter().map(|(name, variants)| {
        let fn_ident = match name.as_str() {
            "reads_outer_name" => format_ident!("op_reads_outer_name"),
            "adaptive_binary" => format_ident!("is_adaptive_binary_op"),
            other => format_ident!("op_has_flag_{}", other),
        };
        quote!(
            #[inline]
            pub(crate) fn #fn_ident(op: Op) -> bool {
                matches!(op, #(Op::#variants)|*)
            }
        )
    });

    let opcode_count = all_variants.len();

    let out = quote! {
        /// Bytecode opcodes for the Harn VM. Defined by
        /// `define_opcodes!`; the `u8` representation is the on-disk
        /// bytecode encoding.
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy, ::core::cmp::PartialEq, ::core::cmp::Eq)]
        #[repr(u8)]
        pub enum Op {
            #(#variants),*
        }

        impl Op {
            /// Every opcode in declaration order. The index is the
            /// canonical `u8` representation; `from_byte` is the
            /// inverse mapping.
            pub(crate) const ALL: &'static [Self] = &[#(Op::#all_variants),*];

            /// Number of declared opcodes (== `ALL.len()`).
            pub(crate) const COUNT: usize = #opcode_count;

            #[inline]
            pub(crate) fn from_byte(byte: u8) -> ::core::option::Option<Self> {
                Self::ALL.get(byte as usize).copied()
            }
        }

        impl crate::vm::Vm {
            /// Sync dispatch table. Returns `Some(Ok(()))` / `Some(Err(_))`
            /// when the opcode completed synchronously and `None` when
            /// the caller must fall through to [`Self::execute_op_async`].
            /// See the per-arm comments in `define_opcodes!` for what
            /// each variant does on dispatch.
            pub(super) fn execute_op_sync(&mut self, op: Op) -> ::core::option::Option<::core::result::Result<(), crate::value::VmError>> {
                let result: ::core::result::Result<(), crate::value::VmError> = match op {
                    #(#sync_arms)*
                };
                ::core::option::Option::Some(result)
            }

            /// Async dispatch table. The caller must have observed `None`
            /// from [`Self::execute_op_sync`] for this opcode; reaching the
            /// catch-all is a coverage bug between the two halves.
            pub(super) fn execute_op_async(
                &mut self,
                op: Op,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<(), crate::value::VmError>,
            > + ::core::marker::Send
                   + '_ {
                OpcodeDispatchFuture::new(async move {
                    match op {
                        #(#async_arms)*
                        sync_op => {
                            debug_assert!(
                                false,
                                "execute_op_async called with sync opcode {sync_op:?} \
                                 — define_opcodes! kept the two halves aligned"
                            );
                            ::core::result::Result::Err(crate::value::VmError::Runtime(format!(
                                "internal VM dispatch error: {sync_op:?} is not an async opcode"
                            )))
                        }
                    }
                })
            }
        }

        impl crate::chunk::Chunk {
            /// Render a single opcode at `ip` (positioned immediately
            /// after the opcode byte) into a human-readable disassembly
            /// line. Each helper advances `ip` past the operand bytes
            /// it consumes. Delegated from
            /// [`crate::chunk::Chunk::disassemble`].
            #[allow(clippy::too_many_lines)]
            pub(crate) fn disassemble_op(&self, op: Op, ip: &mut usize, out: &mut String) {
                let line = match op {
                    #(#disasm_arms)*
                };
                out.push_str(&line);
                out.push('\n');
            }
        }

        #(#flag_fns)*
    };

    Ok(out)
}
