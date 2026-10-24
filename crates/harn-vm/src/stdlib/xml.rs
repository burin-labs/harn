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
//! `from_xml` validates one complete document with `roxmltree`, then maps
//! elements into Harn values. Qualified names retain their source prefixes.
//! DTDs are refused; parsing never fetches external entities.

use crate::value::VmDictExt;
use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_xml_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    aliases = ["__to_xml"],
    exposure = "pure",
    sig = "to_xml(value: any, options?: dict) -> string",
    category = "xml"
)]
fn to_xml_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    let options = args.get(1).and_then(VmValue::as_dict);
    let opts = XmlOptions::from_dict(options);
    let xml = render_xml(value, &opts)?;
    Ok(VmValue::string(xml))
}

#[harn_builtin(
    aliases = ["__from_xml"],
    exposure = "pure",
    sig = "from_xml(text: string, options?: dict) -> dict",
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
    fn from_dict(options: Option<&crate::value::DictMap>) -> Self {
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
    roxmltree::Document::parse(&out)
        .map_err(|error| VmError::Thrown(VmValue::string(format!("to_xml: {error}"))))?;
    Ok(out)
}

fn write_node(
    out: &mut String,
    tag: &str,
    value: &VmValue,
    opts: &XmlOptions,
    depth: usize,
) -> Result<(), VmError> {
    if depth >= MAX_XML_DEPTH {
        return Err(VmError::Thrown(VmValue::string(format!(
            "to_xml: max nesting depth {MAX_XML_DEPTH} exceeded"
        ))));
    }
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
        VmValue::List(_) | VmValue::Set(_) => {
            let items: &[VmValue] = match value {
                VmValue::Set(set) => set.items(),
                VmValue::List(items) => items,
                _ => &[],
            };
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

/// Escape the three XML-reserved characters (`&`, `<`, `>`) for embedding text
/// into XML-ish prompt scaffolding — `<artifact>` bodies, `<system-reminder>`
/// bodies, and the like. The single span-copy escaper for the whole crate, so
/// the prompt-assembly surfaces can never drift apart on how they escape.
pub(crate) fn escape_xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_text(&mut out, text);
    out
}

#[expect(
    clippy::string_slice,
    reason = "span bounds sit adjacent to the ASCII bytes `&`/`<`/`>`"
)]
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
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.'
}

// --- Parser ---

/// Bound both recursive value conversion and rendering on worker threads.
const MAX_XML_DEPTH: usize = 256;

#[derive(Default)]
struct ParseOptions {
    preserve_repeated_tag: bool,
}

impl ParseOptions {
    fn from_dict(options: Option<&crate::value::DictMap>) -> Self {
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
    if text.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
        return Ok(VmValue::dict_map(Default::default()));
    }
    let document =
        roxmltree::Document::parse(text).map_err(|error| parse_error(error.to_string()))?;
    let root = document.root_element();
    Ok(VmValue::dict([(
        element_name(root),
        element_value(root, parse_opts, 0)?,
    )]))
}

fn element_value(
    node: roxmltree::Node<'_, '_>,
    opts: &ParseOptions,
    depth: usize,
) -> Result<VmValue, VmError> {
    if depth >= MAX_XML_DEPTH {
        return Err(parse_error(format!(
            "max nesting depth {MAX_XML_DEPTH} exceeded"
        )));
    }
    let mut children: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
    let mut text = String::new();
    for child in node.children() {
        if child.is_element() {
            children
                .entry(element_name(child))
                .or_default()
                .push(element_value(child, opts, depth + 1)?);
        } else if child.is_text() {
            text.push_str(child.text().unwrap_or_default());
        }
    }
    let source = node.document().input_text();
    let mut attrs: Vec<_> = node
        .attributes()
        .map(|attr| {
            (
                source_name(source, attr.range().start).to_owned(),
                attr.value().to_owned(),
            )
        })
        .collect();
    for namespace in node.namespaces() {
        if node
            .parent_element()
            .and_then(|parent| parent.lookup_namespace_uri(namespace.name()))
            != Some(namespace.uri())
        {
            let name = namespace
                .name()
                .map_or_else(|| "xmlns".to_owned(), |prefix| format!("xmlns:{prefix}"));
            attrs.push((name, namespace.uri().to_owned()));
        }
    }
    Ok(finalize_element(children, text, attrs, opts))
}

fn element_name(node: roxmltree::Node<'_, '_>) -> String {
    source_name(node.document().input_text(), node.range().start + 1).to_owned()
}

fn source_name(source: &str, start: usize) -> &str {
    // Positions come from the validated XML tree; read the original prefix,
    // since multiple prefixes can name the same namespace.
    source
        .get(start..)
        .expect("XML position is a character boundary")
        .split([' ', '\t', '\r', '\n', '/', '>', '='])
        .next()
        .expect("validated XML name")
}

fn parse_error(msg: String) -> VmError {
    VmError::Thrown(VmValue::string(format!("from_xml: {msg}")))
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
    if !opts.preserve_repeated_tag
        && attrs.is_empty()
        && trimmed.is_empty()
        && children.len() == 1
        && children
            .values()
            .next()
            .is_some_and(|values| values.len() > 1)
    {
        return VmValue::list(children.into_values().next().expect("one child group"));
    }
    let mut out = BTreeMap::new();
    for (tag, mut values) in children {
        if values.len() == 1 {
            out.insert(tag, values.pop().unwrap());
        } else {
            out.insert(tag, VmValue::list(values));
        }
    }
    if !attrs.is_empty() {
        let mut attr_dict = BTreeMap::new();
        for (k, v) in attrs {
            attr_dict.insert(k, VmValue::string(v));
        }
        out.put("@attr", VmValue::dict(attr_dict));
    }
    if !trimmed.is_empty() {
        out.put_str("@text", trimmed);
    }
    VmValue::dict(out)
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
        _ => VmValue::string(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, VmValue)]) -> VmValue {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::dict(map)
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
    fn sanitizes_tag_names() {
        assert_eq!(sanitize_tag("hello world"), "hello_world");
        assert_eq!(sanitize_tag("9foo"), "_9foo");
        assert_eq!(sanitize_tag("a.b-c_d"), "a.b-c_d");
        assert_eq!(sanitize_tag(""), "item");
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
                assert!(msg.contains("expected"), "got: {msg}");
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
}
