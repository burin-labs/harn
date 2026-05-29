//! Hand-rolled mini-grammar parser for the Harn-style signature string.
//!
//! Grammar (informal):
//!
//! ```text
//! sig         = type_params? name "(" params? ")" return?
//! type_params = "<" ident ("," ident)* ("where" where_clause ("," where_clause)*)? ">"
//! where_clause = ident ":" ident
//! name        = ident
//! params      = param ("," param)* ("," "..." ident (":" ty)?)?
//! param       = ident ("?")? (":" ty)?       // ? AFTER name = optional param
//! return      = "->" ty
//! ty          = ty_atom ("|" ty_atom)*
//! ty_atom     = ident                               // primitive, generic, or named
//!             | ident "<" ty ("," ty)* ">"          // application, e.g. list<T>
//!             | ident "?"                            // sugar for ty | nil
//!             | "Schema" "<" ident ">"               // SchemaOf marker
//!             | "(" ty ("," ty)* ")" "->" ty        // fn type
//!             | "{" field ("," field)* "}"          // shape literal
//!             | int_literal | string_literal        // literal types
//!             | "@" name                             // Ty const injection: `@NAME` →
//!                                                    //   `<support>::shapes::NAME`;
//!                                                    //   `@a::b::C` → absolute path
//! field       = ident ":" ty ("?")?                 // ? AFTER type = optional field
//! ```
//!
//! Note the `?` placement asymmetry: on a *param*, `?` goes after the name
//! to mark it as optional (`drop_nil?: bool`). On a *type*, `?` goes after
//! the type to express `T | nil` (`value: dict?`). On a *shape field*, `?`
//! follows the type to mark the field as optional. This keeps each kind
//! unambiguous.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;

#[derive(Debug, Clone, Eq, PartialEq)]
enum Tok {
    Ident(String),
    Int(i64),
    Str(String),
    PathInject(String), // @ident or @PATH::TO::ITEM (capital-letter aware ident chain)
    Dot,                // `.` — used in dotted builtin names like `git.repo.discover`
    LParen,
    RParen,
    LAngle,
    RAngle,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Question,
    Pipe,
    Arrow,     // "->"
    DotDotDot, // "..."
}

fn tokenize(s: &str, span: Span) -> syn::Result<Vec<Tok>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'<' => {
                out.push(Tok::LAngle);
                i += 1;
            }
            b'>' => {
                out.push(Tok::RAngle);
                i += 1;
            }
            b'{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            b'}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            b'?' => {
                out.push(Tok::Question);
                i += 1;
            }
            b'|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                out.push(Tok::Arrow);
                i += 2;
            }
            b'.' if i + 2 < bytes.len() && bytes[i + 1] == b'.' && bytes[i + 2] == b'.' => {
                out.push(Tok::DotDotDot);
                i += 3;
            }
            b'.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            b'@' => {
                // @PATH::TO::ITEM — path injection for predeclared shape consts
                // or any value usable as a Rust path.
                i += 1;
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b':')
                {
                    i += 1;
                }
                if start == i {
                    return Err(syn::Error::new(
                        span,
                        "expected identifier or path after `@` in sig string",
                    ));
                }
                out.push(Tok::PathInject(s[start..i].to_string()));
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() => {
                // Negative int literal (only as a Ty::LitInt).
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let lit = &s[start..i];
                let n: i64 = lit
                    .parse()
                    .map_err(|_| syn::Error::new(span, format!("invalid int literal {lit:?}")))?;
                out.push(Tok::Int(n));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let lit = &s[start..i];
                let n: i64 = lit
                    .parse()
                    .map_err(|_| syn::Error::new(span, format!("invalid int literal {lit:?}")))?;
                out.push(Tok::Int(n));
            }
            b'"' => {
                // String literal type. Simple: no escape sequences except \" and \\.
                let start = i + 1;
                i += 1;
                let mut buf = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        match bytes[i + 1] {
                            b'"' => buf.push('"'),
                            b'\\' => buf.push('\\'),
                            b'n' => buf.push('\n'),
                            b't' => buf.push('\t'),
                            other => {
                                return Err(syn::Error::new(
                                    span,
                                    format!(
                                        "unsupported escape \\{} in string literal at offset {}",
                                        other as char, start
                                    ),
                                ));
                            }
                        }
                        i += 2;
                    } else {
                        buf.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i >= bytes.len() {
                    return Err(syn::Error::new(
                        span,
                        format!("unterminated string literal starting at offset {start}"),
                    ));
                }
                i += 1; // closing quote
                out.push(Tok::Str(buf));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].to_string()));
            }
            other => {
                return Err(syn::Error::new(
                    span,
                    format!("unexpected character {:?} in sig string", other as char),
                ));
            }
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    span: Span,
    support: &'a TokenStream2,
    type_params: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok], span: Span, support: &'a TokenStream2) -> Self {
        Self {
            toks,
            pos: 0,
            span,
            support,
            type_params: Vec::new(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, want: &Tok) -> syn::Result<()> {
        match self.peek() {
            Some(t) if t == want => {
                self.pos += 1;
                Ok(())
            }
            Some(other) => Err(syn::Error::new(
                self.span,
                format!("expected {want:?}, found {other:?}"),
            )),
            None => Err(syn::Error::new(
                self.span,
                format!("expected {want:?}, found end of input"),
            )),
        }
    }

    fn expect_ident(&mut self) -> syn::Result<String> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            other => Err(syn::Error::new(
                self.span,
                format!("expected identifier, found {other:?}"),
            )),
        }
    }

    fn parse_sig(&mut self) -> syn::Result<TokenStream2> {
        // Optional `<T, U where T: Iter>` prefix.
        let (type_params, where_clauses) = if matches!(self.peek(), Some(Tok::LAngle)) {
            self.bump();
            self.parse_type_params_block()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.type_params = type_params.clone();

        // Name — may be dotted: `git.repo.discover`.
        let mut name = self.expect_ident()?;
        while matches!(self.peek(), Some(Tok::Dot)) {
            self.bump();
            let segment = self.expect_ident()?;
            name.push('.');
            name.push_str(&segment);
        }

        // Params.
        self.expect(&Tok::LParen)?;
        let (params_tokens, has_rest) = self.parse_params()?;
        self.expect(&Tok::RParen)?;

        // Return.
        let returns_ts = if matches!(self.peek(), Some(Tok::Arrow)) {
            self.bump();
            self.parse_ty()?
        } else {
            let support = self.support;
            quote!(#support::TY_NIL)
        };

        // Build the BuiltinSignature literal.
        let support = self.support;
        let tp_lits = type_params.iter().map(|s| quote!(#s));
        let wc_lits = where_clauses
            .iter()
            .map(|(tp, iface)| quote!((#tp, #iface)));
        let out = quote! {
            #support::BuiltinSignature {
                name: #name,
                params: &[#(#params_tokens),*],
                returns: #returns_ts,
                type_params: &[#(#tp_lits),*],
                has_rest: #has_rest,
                where_clauses: &[#(#wc_lits),*],
            }
        };
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    fn parse_type_params_block(&mut self) -> syn::Result<(Vec<String>, Vec<(String, String)>)> {
        let mut params = Vec::new();
        let mut where_clauses = Vec::new();
        // First ident.
        params.push(self.expect_ident()?);
        loop {
            match self.peek() {
                Some(Tok::Comma) => {
                    self.bump();
                    // Either more type params, or the `where` clause.
                    if let Some(Tok::Ident(s)) = self.peek() {
                        if s == "where" {
                            self.bump();
                            // where T: Iface, U: Iface...
                            loop {
                                let tp = self.expect_ident()?;
                                self.expect(&Tok::Colon)?;
                                let iface = self.expect_ident()?;
                                where_clauses.push((tp, iface));
                                if matches!(self.peek(), Some(Tok::Comma)) {
                                    self.bump();
                                } else {
                                    break;
                                }
                            }
                            break;
                        }
                    }
                    params.push(self.expect_ident()?);
                }
                Some(Tok::RAngle) => break,
                Some(other) => {
                    return Err(syn::Error::new(
                        self.span,
                        format!("unexpected {other:?} in type-param block"),
                    ));
                }
                None => {
                    return Err(syn::Error::new(self.span, "unterminated type-param block"));
                }
            }
        }
        self.expect(&Tok::RAngle)?;
        Ok((params, where_clauses))
    }

    fn parse_params(&mut self) -> syn::Result<(Vec<TokenStream2>, bool)> {
        let mut params = Vec::new();
        let mut has_rest = false;
        if matches!(self.peek(), Some(Tok::RParen)) {
            return Ok((params, false));
        }
        loop {
            if matches!(self.peek(), Some(Tok::DotDotDot)) {
                self.bump();
                let name = self.expect_ident()?;
                let ty = if matches!(self.peek(), Some(Tok::Colon)) {
                    self.bump();
                    self.parse_ty()?
                } else {
                    let support = self.support;
                    quote!(#support::TY_ANY)
                };
                let support = self.support;
                params.push(quote!(#support::Param::new(#name, #ty)));
                has_rest = true;
                break;
            }
            let name = self.expect_ident()?;
            // `name?` marks the param as optional (trailing `?` after the
            // ident, before the colon). Distinguished from `name: ty?` where
            // the `?` is the nil-union sugar on the type.
            let optional = if matches!(self.peek(), Some(Tok::Question)) {
                self.bump();
                true
            } else {
                false
            };
            let ty = if matches!(self.peek(), Some(Tok::Colon)) {
                self.bump();
                self.parse_ty()?
            } else {
                let support = self.support;
                quote!(#support::TY_ANY)
            };
            let support = self.support;
            if optional {
                params.push(quote!(#support::Param::optional(#name, #ty)));
            } else {
                params.push(quote!(#support::Param::new(#name, #ty)));
            }
            match self.peek() {
                Some(Tok::Comma) => {
                    self.bump();
                }
                _ => break,
            }
        }
        Ok((params, has_rest))
    }

    fn parse_ty(&mut self) -> syn::Result<TokenStream2> {
        let first = self.parse_ty_atom()?;
        // Union chain: t | t | t
        if matches!(self.peek(), Some(Tok::Pipe)) {
            let support = self.support;
            let mut members = vec![first];
            while matches!(self.peek(), Some(Tok::Pipe)) {
                self.bump();
                members.push(self.parse_ty_atom()?);
            }
            Ok(quote!(#support::Ty::Union(&[#(#members),*])))
        } else {
            Ok(first)
        }
    }

    fn parse_ty_atom(&mut self) -> syn::Result<TokenStream2> {
        let support = self.support;
        // Fn type: "(t, t) -> t"
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.bump();
            let mut params = Vec::new();
            if !matches!(self.peek(), Some(Tok::RParen)) {
                params.push(self.parse_ty()?);
                while matches!(self.peek(), Some(Tok::Comma)) {
                    self.bump();
                    params.push(self.parse_ty()?);
                }
            }
            self.expect(&Tok::RParen)?;
            self.expect(&Tok::Arrow)?;
            let ret = self.parse_ty()?;
            // Need to leak the ret type as a static reference for Ty::Fn — use
            // a const block to produce &'static Ty.
            let ts = quote! {
                #support::Ty::Fn(
                    &[#(#params),*],
                    {
                        const RET: #support::Ty = #ret;
                        &RET
                    },
                )
            };
            return self.maybe_trailing_optional(ts);
        }
        // Shape literal: "{x: int, y: string?}"
        if matches!(self.peek(), Some(Tok::LBrace)) {
            self.bump();
            let mut fields = Vec::new();
            if !matches!(self.peek(), Some(Tok::RBrace)) {
                loop {
                    let fname = self.expect_ident()?;
                    self.expect(&Tok::Colon)?;
                    let fty = self.parse_ty()?;
                    let foptional = matches!(self.peek(), Some(Tok::Question));
                    if foptional {
                        self.bump();
                    }
                    if foptional {
                        fields.push(quote!(#support::ShapeFieldDescriptor::optional(#fname, #fty)));
                    } else {
                        fields.push(quote!(#support::ShapeFieldDescriptor::new(#fname, #fty)));
                    }
                    match self.peek() {
                        Some(Tok::Comma) => {
                            self.bump();
                            // Tolerate trailing comma.
                            if matches!(self.peek(), Some(Tok::RBrace)) {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }
            self.expect(&Tok::RBrace)?;
            return self.maybe_trailing_optional(quote!(#support::Ty::Shape(&[#(#fields),*])));
        }
        // Int literal type: "0", "-1"
        if let Some(Tok::Int(_)) = self.peek() {
            if let Some(Tok::Int(n)) = self.bump() {
                return self.maybe_trailing_optional(quote!(#support::Ty::LitInt(#n)));
            }
        }
        // String literal type: "\"pass\""
        if let Some(Tok::Str(_)) = self.peek() {
            if let Some(Tok::Str(s)) = self.bump() {
                return self.maybe_trailing_optional(quote!(#support::Ty::LitString(#s)));
            }
        }
        // Shape/type injection: `@LLM_CALL_OPTIONS` or `@my::crate::CONST`.
        //
        // The named const must evaluate to a `Ty` (typically a
        // `Ty::Shape(&[...])` from `harn_builtin_meta::shapes`). A bare
        // `@NAME` resolves under `#support::shapes` (which re-exports
        // `harn_builtin_meta::shapes`); a `::`-qualified `@a::b::C` is used
        // verbatim as an absolute path. This lets a single readable `sig`
        // string name the rich structural contracts the grammar can't spell
        // inline, e.g. `options?: @LLM_CALL_OPTIONS) -> @LLM_CALL_RESULT`.
        if let Some(Tok::PathInject(_)) = self.peek() {
            if let Some(Tok::PathInject(p)) = self.bump() {
                let injected = if p.contains("::") {
                    let path: syn::Path = syn::parse_str(&p).map_err(|e| {
                        syn::Error::new(
                            self.span,
                            format!("invalid Rust path after `@`: {p:?} ({e})"),
                        )
                    })?;
                    quote!(#path)
                } else {
                    let ident = syn::Ident::new(&p, self.span);
                    quote!(#support::shapes::#ident)
                };
                return self.maybe_trailing_optional(injected);
            }
        }
        // Ident, ident<...>, ident?
        let ident = self.expect_ident()?;
        // Application: ident<...>
        if matches!(self.peek(), Some(Tok::LAngle)) {
            self.bump();
            // Special case: Schema<T> → Ty::SchemaOf("T")
            if ident == "Schema" {
                let tp = self.expect_ident()?;
                self.expect(&Tok::RAngle)?;
                return self.maybe_trailing_optional(quote!(#support::Ty::SchemaOf(#tp)));
            }
            let mut args = Vec::new();
            args.push(self.parse_ty()?);
            while matches!(self.peek(), Some(Tok::Comma)) {
                self.bump();
                args.push(self.parse_ty()?);
            }
            self.expect(&Tok::RAngle)?;
            let name = ident;
            return self.maybe_trailing_optional(quote!(#support::Ty::Apply(#name, &[#(#args),*])));
        }
        // Sugar: ident?
        let maybe_optional = matches!(self.peek(), Some(Tok::Question));
        if maybe_optional {
            self.bump();
            let inner = ident_to_ty(&ident, &self.type_params, support);
            return Ok(quote! {
                #support::Ty::Union(&[#inner, #support::TY_NIL])
            });
        }
        Ok(ident_to_ty(&ident, &self.type_params, support))
    }

    /// After parsing a compound type atom (shape, fn, application, literal,
    /// path injection), check for a trailing `?` and wrap in `T | nil`.
    fn maybe_trailing_optional(&mut self, ty: TokenStream2) -> syn::Result<TokenStream2> {
        if matches!(self.peek(), Some(Tok::Question)) {
            self.bump();
            let support = self.support;
            Ok(quote!(#support::Ty::Union(&[#ty, #support::TY_NIL])))
        } else {
            Ok(ty)
        }
    }
}

fn ident_to_ty(name: &str, type_params: &[String], support: &TokenStream2) -> TokenStream2 {
    // Generic type parameter takes precedence.
    if type_params.iter().any(|tp| tp == name) {
        return quote!(#support::Ty::Generic(#name));
    }
    match name {
        "any" => quote!(#support::TY_ANY),
        "bool" => quote!(#support::TY_BOOL),
        "bytes" => quote!(#support::TY_BYTES),
        "closure" => quote!(#support::TY_CLOSURE),
        "dict" => quote!(#support::TY_DICT),
        "duration" => quote!(#support::TY_DURATION),
        "float" => quote!(#support::TY_FLOAT),
        "int" => quote!(#support::TY_INT),
        "list" => quote!(#support::TY_LIST),
        "never" => quote!(#support::TY_NEVER),
        "nil" => quote!(#support::TY_NIL),
        "string" => quote!(#support::TY_STRING),
        // Common unions get the predeclared constants.
        "string_or_nil" => quote!(#support::TY_STRING_OR_NIL),
        "int_or_nil" => quote!(#support::TY_INT_OR_NIL),
        "dict_or_nil" => quote!(#support::TY_DICT_OR_NIL),
        "bytes_or_nil" => quote!(#support::TY_BYTES_OR_NIL),
        "number" => quote!(#support::TY_NUMBER),
        other => quote!(#support::Ty::Named(#other)),
    }
}

/// Parse a Harn-style signature string into a token stream that builds a
/// `BuiltinSignature` const literal. `support` is the path prefix to use for
/// types (e.g. `crate::stdlib::macros`).
pub fn parse_sig(src: &str, span: Span, support: &TokenStream2) -> syn::Result<TokenStream2> {
    let toks = tokenize(src, span)?;
    let mut p = Parser::new(&toks, span, support);
    let out = p.parse_sig()?;
    if p.pos != p.toks.len() {
        return Err(syn::Error::new(
            span,
            format!(
                "trailing tokens in sig string after position {} ({:?} remaining)",
                p.pos,
                &p.toks[p.pos..]
            ),
        ));
    }
    Ok(out)
}
