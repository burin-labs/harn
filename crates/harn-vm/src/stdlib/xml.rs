//! XML serialization built-ins.
//!
//! `to_xml(value, options?)` converts a Harn value into an XML document
//! suitable for embedding in `.harn.prompt` templates (e.g. the
//! `<previous_chats>...</previous_chats>` system-prompt blocks the
//! coding-agent harnesses use). `from_xml(text)` performs a best-effort
//! parse that round-trips the same shape.
//!
//! The mapping is deliberately compact:
//!
//! * Dict → tagged element per key (key is the tag name). Keys are
//!   sanitized to valid XML names; non-ASCII or punctuation gets
//!   replaced with `_`.
//! * List → repeated `<item>...</item>` children. The parent tag stays
//!   whatever wraps the list (top-level list serialises under the
//!   `root` tag unless `options.root` overrides).
//! * String/Int/Float/Bool/Duration → text node.
//! * Nil → empty self-closing element.
//! * Bytes → base64 text node with `encoding="base64"` attribute on
//!   the wrapping element.
//!
//! `options.pretty: bool` adds newline + indent between elements,
//! `options.root: string` overrides the top-level tag, `options.declaration: bool`
//! prepends `<?xml version="1.0" encoding="UTF-8"?>`, `options.item_tag: string`
//! overrides the `<item>` tag used for list children.
//!
//! `from_xml` uses an in-tree recursive-descent parser. It handles
//! tags, attributes, text content (with entity unescape), CDATA,
//! self-closing elements, comments, and the XML declaration. Namespace
//! prefixes are preserved verbatim in tag names; they are not
//! resolved.

use std::collections::BTreeMap;
use std::rc::Rc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_xml_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&TO_XML_IMPL_DEF, &FROM_XML_IMPL_DEF];

#[harn_builtin(
    sig = "to_xml(value: any, options?: dict) -> string",
    aliases = ["__to_xml"],
    category = "xml"
)]
fn to_xml_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    let options = args.get(1).and_then(VmValue::as_dict);
    let opts = XmlOptions::from_dict(options);
    let xml = render_xml(value, &opts)?;
    Ok(VmValue::String(Rc::from(xml)))
}

#[harn_builtin(
    sig = "from_xml(text: string, options?: dict) -> dict",
    aliases = ["__from_xml"],
    category = "xml"
)]
fn from_xml_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let options = args.get(1).and_then(VmValue::as_dict);
    let parse_opts = ParseOptions::from_dict(options);
    parse_xml(&text, &parse_opts)
}

struct XmlOptions {
    root: String,
    item_tag: String,
    pretty: bool,
    declaration: bool,
}

impl XmlOptions {
    fn from_dict(options: Option<&BTreeMap<String, VmValue>>) -> Self {
        let mut out = XmlOptions {
            root: "root".to_string(),
            item_tag: "item".to_string(),
            pretty: false,
            declaration: false,
        };
        let Some(dict) = options else {
            return out;
        };
        if let Some(VmValue::String(s)) = dict.get("root") {
            if !s.is_empty() {
                out.root = sanitize_tag(s);
            }
        }
        if let Some(VmValue::String(s)) = dict.get("item_tag") {
            if !s.is_empty() {
                out.item_tag = sanitize_tag(s);
            }
        }
        if let Some(VmValue::Bool(b)) = dict.get("pretty") {
            out.pretty = *b;
        }
        if let Some(VmValue::Bool(b)) = dict.get("declaration") {
            out.declaration = *b;
        }
        out
    }
}

fn render_xml(value: &VmValue, opts: &XmlOptions) -> Result<String, VmError> {
    let mut out = String::new();
    if opts.declaration {
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        if opts.pretty {
            out.push('\n');
        }
    }
    write_node(&mut out, &opts.root, value, opts, 0)?;
    Ok(out)
}

fn write_node(
    out: &mut String,
    tag: &str,
    value: &VmValue,
    opts: &XmlOptions,
    depth: usize,
) -> Result<(), VmError> {
    let tag = sanitize_tag(tag);
    match value {
        VmValue::Nil => {
            push_indent(out, opts, depth);
            out.push('<');
            out.push_str(&tag);
            out.push_str(" />");
        }
        VmValue::Dict(d) => {
            push_indent(out, opts, depth);
            out.push('<');
            out.push_str(&tag);
            out.push('>');
            for (key, child) in d.iter() {
                if opts.pretty {
                    out.push('\n');
                }
                write_node(out, key, child, opts, depth + 1)?;
            }
            if opts.pretty && !d.is_empty() {
                out.push('\n');
                push_indent(out, opts, depth);
            }
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
        }
        VmValue::List(items) | VmValue::Set(items) => {
            push_indent(out, opts, depth);
            out.push('<');
            out.push_str(&tag);
            out.push('>');
            for item in items.iter() {
                if opts.pretty {
                    out.push('\n');
                }
                write_node(out, &opts.item_tag, item, opts, depth + 1)?;
            }
            if opts.pretty && !items.is_empty() {
                out.push('\n');
                push_indent(out, opts, depth);
            }
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
        }
        VmValue::Bytes(bytes) => {
            push_indent(out, opts, depth);
            out.push('<');
            out.push_str(&tag);
            out.push_str(" encoding=\"base64\">");
            out.push_str(&BASE64.encode(bytes.as_ref().as_slice()));
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
        }
        other => {
            push_indent(out, opts, depth);
            out.push('<');
            out.push_str(&tag);
            out.push('>');
            escape_text(out, &scalar_text(other));
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
        }
    }
    Ok(())
}

fn scalar_text(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => (**s).to_string(),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => n.to_string(),
        VmValue::Bool(b) => b.to_string(),
        VmValue::Duration(ms) => ms.to_string(),
        other => other.display(),
    }
}

fn push_indent(out: &mut String, opts: &XmlOptions, depth: usize) {
    if opts.pretty {
        for _ in 0..depth {
            out.push_str("  ");
        }
    }
}

fn escape_text(out: &mut String, text: &str) {
    // Span-copy: the typical case is "lots of text, few escapables",
    // so push longest unescaped runs at once and only spell out the
    // entity for the three XML-reserved bytes. All three are ASCII,
    // so we can scan the byte slice without worrying about UTF-8
    // boundaries (the entire UTF-8 multibyte tail is >= 0x80 and
    // never collides with `&`/`<`/`>`).
    let bytes = text.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let entity = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            _ => continue,
        };
        if start < i {
            out.push_str(&text[start..i]);
        }
        out.push_str(entity);
        start = i + 1;
    }
    if start < bytes.len() {
        out.push_str(&text[start..]);
    }
}

/// XML tag names must start with a letter or underscore and only
/// contain letters, digits, `-`, `_`, `.`. Replace anything else with
/// `_` and prepend `_` if the first character isn't valid. Empty input
/// becomes `item`.
fn sanitize_tag(name: &str) -> String {
    if name.is_empty() {
        return "item".to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if is_xml_name_start(first) {
            out.push(first);
        } else {
            out.push('_');
            if is_xml_name_char(first) {
                out.push(first);
            } else {
                out.push('_');
            }
        }
    }
    for ch in chars {
        if is_xml_name_char(ch) {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn is_xml_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_xml_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' || ch == ':'
}

// --- Parser ---

/// Hard cap on XML element nesting depth. The parser is recursive, so
/// without a depth check an adversarial document like `<a><a><a>...`
/// would blow the stack. 256 leaves plenty of headroom for legitimate
/// prompt-shape XML (system-prompt blocks rarely nest past a handful
/// of levels) while keeping recursion well inside Rust's default
/// 8 MiB main-thread stack and the 1–2 MiB worker thread stacks we
/// spawn for harn-runtime tasks.
const MAX_XML_DEPTH: usize = 256;

#[derive(Default)]
struct ParseOptions {
    preserve_repeated_tag: bool,
}

impl ParseOptions {
    fn from_dict(options: Option<&BTreeMap<String, VmValue>>) -> Self {
        let mut out = ParseOptions::default();
        let Some(dict) = options else {
            return out;
        };
        if let Some(VmValue::Bool(b)) = dict.get("preserve_repeated_tag") {
            out.preserve_repeated_tag = *b;
        }
        out
    }
}

fn parse_xml(text: &str, parse_opts: &ParseOptions) -> Result<VmValue, VmError> {
    let bytes = text.as_bytes();
    let mut p = Parser {
        src: bytes,
        pos: 0,
        depth: 0,
        opts: parse_opts,
    };
    p.skip_ws_and_prologue()?;
    if p.pos >= p.src.len() {
        return Ok(VmValue::Dict(Rc::new(BTreeMap::new())));
    }
    let (tag, value) = p.parse_element()?;
    p.skip_ws_and_prologue().ok();
    let mut out = BTreeMap::new();
    out.insert(tag, value);
    Ok(VmValue::Dict(Rc::new(out)))
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
    opts: &'a ParseOptions,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn rest(&self) -> &'a [u8] {
        &self.src[self.pos..]
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.rest().starts_with(needle)
    }

    fn consume(&mut self, needle: &[u8]) -> Result<(), VmError> {
        if !self.starts_with(needle) {
            return Err(parse_error(format!(
                "expected `{}`",
                std::str::from_utf8(needle).unwrap_or("?")
            )));
        }
        self.pos += needle.len();
        Ok(())
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len()
            && matches!(self.src[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    /// Skips whitespace, comments (`<!-- ... -->`), processing instructions
    /// (`<?xml ... ?>`), and DOCTYPE declarations.
    fn skip_ws_and_prologue(&mut self) -> Result<(), VmError> {
        loop {
            self.skip_ws();
            if self.starts_with(b"<?") {
                let end = find_subslice(&self.src[self.pos..], b"?>")
                    .ok_or_else(|| parse_error("unterminated <?...?>".into()))?;
                self.pos += end + 2;
            } else if self.starts_with(b"<!--") {
                let end = find_subslice(&self.src[self.pos + 4..], b"-->")
                    .ok_or_else(|| parse_error("unterminated <!--".into()))?;
                self.pos += 4 + end + 3;
            } else if self.starts_with(b"<![CDATA[") {
                // CDATA is part of element content, not the prologue —
                // hand it back to the element body parser.
                break;
            } else if self.starts_with(b"<!") {
                // DOCTYPE or other declaration — skip to the next '>'.
                let end = find_byte(&self.src[self.pos..], b'>')
                    .ok_or_else(|| parse_error("unterminated <!...>".into()))?;
                self.pos += end + 1;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn parse_element(&mut self) -> Result<(String, VmValue), VmError> {
        if self.depth >= MAX_XML_DEPTH {
            return Err(parse_error(format!(
                "max nesting depth {MAX_XML_DEPTH} exceeded"
            )));
        }
        self.depth += 1;
        let result = self.parse_element_inner();
        self.depth -= 1;
        result
    }

    fn parse_element_inner(&mut self) -> Result<(String, VmValue), VmError> {
        self.consume(b"<")?;
        let tag = self.read_name()?;
        let attrs = self.read_attrs()?;
        if self.starts_with(b"/>") {
            self.pos += 2;
            let value = if attrs.is_empty() {
                VmValue::Nil
            } else {
                attrs_to_value(attrs)
            };
            return Ok((tag, value));
        }
        self.consume(b">")?;
        let mut children: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
        let mut text_buf = String::new();
        loop {
            self.skip_ws_and_prologue()?;
            if self.starts_with(b"</") {
                self.pos += 2;
                let close_tag = self.read_name()?;
                self.skip_ws();
                self.consume(b">")?;
                if close_tag != tag {
                    return Err(parse_error(format!(
                        "mismatched close tag: opened <{tag}>, closed </{close_tag}>"
                    )));
                }
                break;
            }
            if self.starts_with(b"<![CDATA[") {
                self.pos += 9;
                let end = find_subslice(&self.src[self.pos..], b"]]>")
                    .ok_or_else(|| parse_error("unterminated CDATA".into()))?;
                let chunk = std::str::from_utf8(&self.src[self.pos..self.pos + end])
                    .map_err(|e| parse_error(format!("cdata utf-8: {e}")))?;
                text_buf.push_str(chunk);
                self.pos += end + 3;
                continue;
            }
            if self.starts_with(b"<") {
                // Append accumulated text as a virtual `@text` field when we
                // hit a child element so mixed content is preserved.
                let (child_tag, child_value) = self.parse_element()?;
                children.entry(child_tag).or_default().push(child_value);
                continue;
            }
            // Plain text up to the next `<`.
            let chunk_end = find_byte(&self.src[self.pos..], b'<').unwrap_or(self.rest().len());
            let chunk = std::str::from_utf8(&self.src[self.pos..self.pos + chunk_end])
                .map_err(|e| parse_error(format!("text utf-8: {e}")))?;
            unescape_into(chunk, &mut text_buf)?;
            self.pos += chunk_end;
            if chunk_end == 0 {
                return Err(parse_error("unterminated element".into()));
            }
        }
        let value = finalize_element(children, text_buf, attrs, self.opts);
        Ok((tag, value))
    }

    fn read_name(&mut self) -> Result<String, VmError> {
        let start = self.pos;
        if self.pos >= self.src.len() || !is_xml_name_start(self.src[self.pos] as char) {
            return Err(parse_error("expected element name".into()));
        }
        self.pos += 1;
        while self.pos < self.src.len() && is_xml_name_char(self.src[self.pos] as char) {
            self.pos += 1;
        }
        Ok(std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|e| parse_error(format!("name utf-8: {e}")))?
            .to_string())
    }

    fn read_attrs(&mut self) -> Result<Vec<(String, String)>, VmError> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if self.starts_with(b"/>") || self.starts_with(b">") {
                return Ok(out);
            }
            let name = self.read_name()?;
            self.skip_ws();
            self.consume(b"=")?;
            self.skip_ws();
            let quote = self
                .peek()
                .ok_or_else(|| parse_error("expected attribute value".into()))?;
            if quote != b'"' && quote != b'\'' {
                return Err(parse_error("attribute value must be quoted".into()));
            }
            self.pos += 1;
            let end = find_byte(&self.src[self.pos..], quote)
                .ok_or_else(|| parse_error("unterminated attribute value".into()))?;
            let raw = std::str::from_utf8(&self.src[self.pos..self.pos + end])
                .map_err(|e| parse_error(format!("attr utf-8: {e}")))?;
            let mut buf = String::with_capacity(raw.len());
            unescape_into(raw, &mut buf)?;
            self.pos += end + 1;
            out.push((name, buf));
        }
    }
}

fn parse_error(msg: String) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(format!("from_xml: {msg}"))))
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn find_byte(hay: &[u8], byte: u8) -> Option<usize> {
    hay.iter().position(|b| *b == byte)
}

fn unescape_into(text: &str, out: &mut String) -> Result<(), VmError> {
    let mut chars = text.char_indices();
    while let Some((idx, ch)) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let rest = &text[idx..];
        let semi = rest
            .find(';')
            .ok_or_else(|| parse_error("entity missing `;`".into()))?;
        let entity = &rest[1..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                let code = u32::from_str_radix(&e[2..], 16)
                    .map_err(|err| parse_error(format!("bad hex entity &{e};: {err}")))?;
                let ch = char::from_u32(code)
                    .ok_or_else(|| parse_error(format!("invalid codepoint &{e};")))?;
                out.push(ch);
            }
            e if e.starts_with('#') => {
                let code = e[1..]
                    .parse::<u32>()
                    .map_err(|err| parse_error(format!("bad numeric entity &{e};: {err}")))?;
                let ch = char::from_u32(code)
                    .ok_or_else(|| parse_error(format!("invalid codepoint &{e};")))?;
                out.push(ch);
            }
            other => {
                return Err(parse_error(format!("unknown entity &{other};")));
            }
        }
        // Advance the iterator past the matched entity.
        for _ in 0..semi {
            chars.next();
        }
    }
    Ok(())
}

fn finalize_element(
    children: BTreeMap<String, Vec<VmValue>>,
    text: String,
    attrs: Vec<(String, String)>,
    opts: &ParseOptions,
) -> VmValue {
    let trimmed = text.trim();
    if children.is_empty() && attrs.is_empty() {
        if trimmed.is_empty() {
            return VmValue::Nil;
        }
        return scalar_from_text(trimmed);
    }
    // Plain repeated-child case (e.g. `<list><item>a</item><item>b</item></list>`).
    // The default collapse is render-side symmetric with `to_xml`'s
    // list serialization (item_tag wraps each element). When the
    // caller passes `{preserve_repeated_tag: true}` the inner tag is
    // kept so general-purpose XML (`<addresses><a>1</a><a>2</a></addresses>`)
    // round-trips losslessly.
    if children.len() == 1 && attrs.is_empty() && trimmed.is_empty() {
        let (tag, values) = children.into_iter().next().unwrap();
        if values.len() > 1 {
            if opts.preserve_repeated_tag {
                let mut out = BTreeMap::new();
                out.insert(tag, VmValue::List(Rc::new(values)));
                return VmValue::Dict(Rc::new(out));
            }
            return VmValue::List(Rc::new(values));
        }
        let mut out = BTreeMap::new();
        out.insert(tag, values.into_iter().next().unwrap());
        return VmValue::Dict(Rc::new(out));
    }
    let mut out = BTreeMap::new();
    for (tag, mut values) in children {
        if values.len() == 1 {
            out.insert(tag, values.pop().unwrap());
        } else {
            out.insert(tag, VmValue::List(Rc::new(values)));
        }
    }
    if !attrs.is_empty() {
        let mut attr_dict = BTreeMap::new();
        for (k, v) in attrs {
            attr_dict.insert(k, VmValue::String(Rc::from(v)));
        }
        out.insert("@attr".to_string(), VmValue::Dict(Rc::new(attr_dict)));
    }
    if !trimmed.is_empty() {
        out.insert(
            "@text".to_string(),
            VmValue::String(Rc::from(trimmed.to_string())),
        );
    }
    VmValue::Dict(Rc::new(out))
}

fn scalar_from_text(text: &str) -> VmValue {
    if let Ok(n) = text.parse::<i64>() {
        return VmValue::Int(n);
    }
    if text.chars().any(|c| c == '.' || c == 'e' || c == 'E') {
        if let Ok(n) = text.parse::<f64>() {
            return VmValue::Float(n);
        }
    }
    match text {
        "true" => VmValue::Bool(true),
        "false" => VmValue::Bool(false),
        _ => VmValue::String(Rc::from(text.to_string())),
    }
}

fn attrs_to_value(attrs: Vec<(String, String)>) -> VmValue {
    let mut out = BTreeMap::new();
    let mut attr_dict = BTreeMap::new();
    for (k, v) in attrs {
        attr_dict.insert(k, VmValue::String(Rc::from(v)));
    }
    out.insert("@attr".to_string(), VmValue::Dict(Rc::new(attr_dict)));
    VmValue::Dict(Rc::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, VmValue)]) -> VmValue {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::Dict(Rc::new(map))
    }

    fn opts() -> XmlOptions {
        XmlOptions {
            root: "root".into(),
            item_tag: "item".into(),
            pretty: false,
            declaration: false,
        }
    }

    fn parse(xml: &str) -> Result<VmValue, VmError> {
        parse_xml(xml, &ParseOptions::default())
    }

    #[test]
    fn renders_flat_dict() {
        let value = dict(&[
            ("name", VmValue::String(Rc::from("Ada"))),
            ("year", VmValue::Int(1815)),
        ]);
        let xml = render_xml(&value, &opts()).unwrap();
        assert_eq!(xml, "<root><name>Ada</name><year>1815</year></root>");
    }

    #[test]
    fn renders_list_as_repeated_items() {
        let value = dict(&[(
            "previous_chats",
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("x.jsonl")),
                VmValue::String(Rc::from("y.jsonl")),
            ])),
        )]);
        let xml = render_xml(&value, &opts()).unwrap();
        assert_eq!(
            xml,
            "<root><previous_chats><item>x.jsonl</item><item>y.jsonl</item></previous_chats></root>"
        );
    }

    #[test]
    fn escapes_special_chars_in_text() {
        let value = dict(&[("msg", VmValue::String(Rc::from("<a> & </b>")))]);
        let xml = render_xml(&value, &opts()).unwrap();
        assert_eq!(xml, "<root><msg>&lt;a&gt; &amp; &lt;/b&gt;</msg></root>");
    }

    #[test]
    fn sanitizes_tag_names() {
        assert_eq!(sanitize_tag("hello world"), "hello_world");
        assert_eq!(sanitize_tag("9foo"), "_9foo");
        assert_eq!(sanitize_tag("a.b-c_d"), "a.b-c_d");
        assert_eq!(sanitize_tag(""), "item");
    }

    #[test]
    fn pretty_mode_indents_children() {
        let mut o = opts();
        o.pretty = true;
        let value = dict(&[
            ("a", VmValue::String(Rc::from("x"))),
            ("b", VmValue::String(Rc::from("y"))),
        ]);
        let xml = render_xml(&value, &o).unwrap();
        assert_eq!(xml, "<root>\n  <a>x</a>\n  <b>y</b>\n</root>");
    }

    #[test]
    fn declaration_prepends_xml_decl() {
        let mut o = opts();
        o.declaration = true;
        let value = dict(&[("x", VmValue::Int(1))]);
        let xml = render_xml(&value, &o).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    }

    #[test]
    fn parse_flat_dict_round_trip() {
        let xml = "<root><name>Ada</name><year>1815</year></root>";
        let parsed = parse(xml).unwrap();
        let outer = parsed.as_dict().unwrap();
        let inner = outer.get("root").and_then(VmValue::as_dict).unwrap();
        assert_eq!(
            inner.get("name").map(VmValue::display).unwrap(),
            "Ada".to_string()
        );
        assert_eq!(inner.get("year").and_then(VmValue::as_int), Some(1815));
    }

    #[test]
    fn parse_repeated_children_into_list() {
        let xml =
            "<root><previous_chats><item>x.jsonl</item><item>y.jsonl</item></previous_chats></root>";
        let parsed = parse(xml).unwrap();
        let outer = parsed.as_dict().unwrap();
        let inner = outer.get("root").and_then(VmValue::as_dict).unwrap();
        let chats = inner.get("previous_chats").unwrap();
        let items: Vec<String> = match chats {
            VmValue::List(items) => items.iter().map(VmValue::display).collect(),
            VmValue::Dict(d) => match d.get("item") {
                Some(VmValue::List(items)) => items.iter().map(VmValue::display).collect(),
                Some(VmValue::String(s)) => vec![(**s).to_string()],
                other => panic!("unexpected dict child: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(items, vec!["x.jsonl".to_string(), "y.jsonl".to_string()]);
    }

    #[test]
    fn parse_handles_declaration_and_comments() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- a comment -->
<root><a>1</a></root>"#;
        let parsed = parse(xml).unwrap();
        let root = parsed.as_dict().unwrap().get("root").cloned().unwrap();
        let a = root.as_dict().unwrap().get("a").cloned().unwrap();
        assert_eq!(a.as_int(), Some(1));
    }

    #[test]
    fn parse_handles_cdata() {
        let xml = "<root><raw><![CDATA[<not parsed>]]></raw></root>";
        let parsed = parse(xml).unwrap();
        let root = parsed.as_dict().unwrap().get("root").cloned().unwrap();
        let raw = root.as_dict().unwrap().get("raw").cloned().unwrap();
        assert_eq!(raw.display(), "<not parsed>".to_string());
    }

    #[test]
    fn parse_handles_attributes() {
        let xml = r#"<root><item id="42" name="foo">x</item></root>"#;
        let parsed = parse(xml).unwrap();
        let root = parsed.as_dict().unwrap().get("root").cloned().unwrap();
        let item = root.as_dict().unwrap().get("item").cloned().unwrap();
        let item_dict = item.as_dict().unwrap();
        let attrs = item_dict.get("@attr").cloned().unwrap();
        let attrs = attrs.as_dict().unwrap();
        assert_eq!(
            attrs.get("id").map(VmValue::display),
            Some("42".to_string())
        );
        assert_eq!(
            attrs.get("name").map(VmValue::display),
            Some("foo".to_string())
        );
        assert_eq!(
            item_dict.get("@text").map(VmValue::display),
            Some("x".to_string())
        );
    }

    #[test]
    fn parse_rejects_mismatched_tags() {
        let xml = "<a></b>";
        let err = parse(xml).unwrap_err();
        match err {
            VmError::Thrown(VmValue::String(msg)) => {
                assert!(msg.contains("mismatched"), "got: {msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_pathologically_nested_xml() {
        // 257 nested elements blows the depth budget by one. We don't
        // build a 10k-element string because the recursion limit
        // itself is the contract — once we trip it the inner branches
        // never run.
        let depth = MAX_XML_DEPTH + 1;
        let mut xml = String::with_capacity(depth * 8);
        for _ in 0..depth {
            xml.push_str("<a>");
        }
        for _ in 0..depth {
            xml.push_str("</a>");
        }
        let err = parse(&xml).unwrap_err();
        match err {
            VmError::Thrown(VmValue::String(msg)) => {
                assert!(
                    msg.contains("max nesting depth"),
                    "expected depth error, got: {msg}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn preserve_repeated_tag_keeps_inner_tag() {
        let xml = "<addresses><a>1</a><a>2</a></addresses>";
        let opts = ParseOptions {
            preserve_repeated_tag: true,
        };
        let parsed = parse_xml(xml, &opts).unwrap();
        let outer = parsed.as_dict().unwrap();
        let addresses = outer
            .get("addresses")
            .and_then(VmValue::as_dict)
            .expect("addresses dict");
        let inner = addresses
            .get("a")
            .cloned()
            .expect("inner tag preserved under `a`");
        let items: Vec<i64> = match inner {
            VmValue::List(items) => items.iter().filter_map(VmValue::as_int).collect(),
            other => panic!("expected list under `a`, got {other:?}"),
        };
        assert_eq!(items, vec![1, 2]);
    }

    #[test]
    fn default_parse_collapses_repeated_tag_to_list() {
        let xml = "<addresses><a>1</a><a>2</a></addresses>";
        let parsed = parse(xml).unwrap();
        let outer = parsed.as_dict().unwrap();
        match outer.get("addresses") {
            Some(VmValue::List(items)) => {
                let ints: Vec<i64> = items.iter().filter_map(VmValue::as_int).collect();
                assert_eq!(ints, vec![1, 2]);
            }
            other => panic!("expected list, got {other:?}"),
        }
    }
}
