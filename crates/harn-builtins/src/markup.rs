//! Markdown structure and visible text, shared by scripts and document extraction.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use markdown::mdast::Node;
use markdown::{Constructs, MdxSignal, ParseOptions};
use regex::Regex;
use serde::Serialize;

use harn_vm::stdlib::macros::harn_builtin;
use harn_vm_core::value::args::Args;
use harn_vm_core::value::{VmError, VmValue};

#[derive(Serialize)]
struct Heading {
    level: u8,
    text: String,
    anchor: String,
    line: usize,
    offset: usize,
}

#[derive(Serialize)]
struct Link {
    destination: String,
    image: bool,
    line: usize,
    offset: usize,
}

#[derive(Serialize)]
struct ProseBlock {
    text: String,
    heading: bool,
    line: usize,
}

#[derive(Default, Serialize)]
struct Document {
    headings: Vec<Heading>,
    links: Vec<Link>,
    blocks: Vec<ProseBlock>,
    text: String,
}

fn jsx_name(node: &Node) -> Option<&str> {
    match node {
        Node::MdxJsxFlowElement(element) => element.name.as_deref(),
        Node::MdxJsxTextElement(element) => element.name.as_deref(),
        _ => None,
    }
}

fn hidden(node: &Node) -> bool {
    matches!(jsx_name(node), Some("script" | "style"))
}

fn code_container(node: &Node) -> bool {
    matches!(jsx_name(node), Some("pre" | "code"))
}

fn nodes(root: &Node) -> impl Iterator<Item = &Node> {
    let mut pending = vec![root];
    std::iter::from_fn(move || {
        let node = pending.pop()?;
        if !hidden(node) && !code_container(node) {
            if let Some(children) = node.children() {
                pending.extend(children.iter().rev());
            }
        }
        Some(node)
    })
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn html_text(source: &str, prose: bool) -> String {
    let document = scraper::Html::parse_document(source);
    let mut text = Vec::new();
    for node in document.tree.nodes() {
        let Some(value) = node.value().as_text() else {
            continue;
        };
        let excluded = node.ancestors().any(|parent| {
            parent.value().as_element().is_some_and(|element| {
                matches!(element.name(), "head" | "script" | "style")
                    || (prose && matches!(element.name(), "pre" | "code" | "table"))
            })
        });
        if !excluded {
            text.push(value.to_string());
        }
    }
    normalize(&text.join(" "))
}

fn inline_text(root: &Node, prose: bool) -> String {
    let mut pending = vec![root];
    let mut output = String::new();
    while let Some(node) = pending.pop() {
        if hidden(node) || (prose && code_container(node)) {
            output.push(' ');
            continue;
        }
        match node {
            Node::Text(text) => output.push_str(&escape_html(&text.value)),
            Node::InlineCode(code) if !prose => output.push_str(&escape_html(&code.value)),
            Node::Code(code) if !prose => output.push_str(&escape_html(&code.value)),
            Node::Image(image) if !prose => output.push_str(&escape_html(&image.alt)),
            Node::ImageReference(image) if !prose => output.push_str(&escape_html(&image.alt)),
            Node::Html(html) => output.push_str(&html.value),
            Node::Break(_) => output.push(' '),
            Node::InlineCode(_)
            | Node::Code(_)
            | Node::Image(_)
            | Node::ImageReference(_)
            | Node::MdxjsEsm(_)
            | Node::MdxFlowExpression(_)
            | Node::MdxTextExpression(_)
            | Node::Yaml(_)
            | Node::Toml(_) => output.push(' '),
            Node::Link(link) if prose && bare_web_link(node, &link.url) => output.push(' '),
            _ => {
                if matches!(node, Node::TableCell(_)) {
                    output.push(' ');
                }
                if let Some(children) = node.children() {
                    pending.extend(children.iter().rev());
                }
            }
        }
    }
    html_text(&output, prose)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn bare_web_link(node: &Node, url: &str) -> bool {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }
    let label = inline_text(node, false);
    label == url
        || url
            .strip_prefix("http://")
            .is_some_and(|address| address.starts_with("www.") && label == address)
}

fn anchor(text: &str, used: &mut BTreeSet<String>) -> String {
    static OMIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[^\p{L}\p{M}\p{N}\p{Pc} -]").expect("static heading anchor pattern")
    });
    let base = OMIT.replace_all(&text.to_lowercase(), "").replace(' ', "-");
    let mut candidate = base.clone();
    let mut suffix = 0;
    while !used.insert(candidate.clone()) {
        suffix += 1;
        candidate = format!("{base}-{suffix}");
    }
    candidate
}

fn parse_options(mdx: bool) -> Result<ParseOptions, VmError> {
    let mut options = ParseOptions::gfm();
    options.constructs.frontmatter = true;
    if mdx {
        options.constructs = Constructs {
            frontmatter: true,
            gfm_autolink_literal: true,
            gfm_strikethrough: true,
            gfm_table: true,
            gfm_task_list_item: true,
            ..Constructs::mdx()
        };
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|error| VmError::Runtime(format!("markdown_parse: {error}")))?;
        let parser = std::cell::RefCell::new(parser);
        options.mdx_esm_parse = Some(Box::new(move |source| {
            match parser.borrow_mut().parse(source, None) {
                Some(tree) if !tree.root_node().has_error() => MdxSignal::Ok,
                _ => MdxSignal::Eof(
                    "Incomplete import or export declaration".to_string(),
                    Box::new("harn".to_string()),
                    Box::new("mdx_module".to_string()),
                ),
            }
        }));
    }
    Ok(options)
}

fn analyze(source: &str, mdx: bool) -> Result<Document, VmError> {
    let root = markdown::to_mdast(source, &parse_options(mdx)?)
        .map_err(|error| VmError::Runtime(format!("markdown_parse: {error}")))?;
    let mut definitions = BTreeMap::new();
    for node in nodes(&root) {
        if let Node::Definition(definition) = node {
            definitions
                .entry(definition.identifier.as_str())
                .or_insert(definition.url.as_str());
        }
    }
    let mut report = Document::default();
    let mut anchors = BTreeSet::new();
    let mut text = Vec::new();
    for node in nodes(&root) {
        let line = node.position().map_or(1, |position| position.start.line);
        let offset = node.position().map_or(0, |position| position.start.offset);
        let link = match node {
            Node::Link(link) => Some((link.url.as_str(), false)),
            Node::Image(image) => Some((image.url.as_str(), true)),
            Node::LinkReference(link) => definitions
                .get(link.identifier.as_str())
                .map(|url| (*url, false)),
            Node::ImageReference(image) => definitions
                .get(image.identifier.as_str())
                .map(|url| (*url, true)),
            Node::Definition(definition) => Some((definition.url.as_str(), false)),
            _ => None,
        };
        if let Some((destination, image)) = link {
            report.links.push(Link {
                destination: destination.to_string(),
                image,
                line,
                offset,
            });
        }
        if matches!(node, Node::Heading(_) | Node::Paragraph(_)) {
            let rendered = inline_text(node, false);
            text.push(rendered.clone());
            let prose = inline_text(node, true);
            if !prose.is_empty() {
                report.blocks.push(ProseBlock {
                    text: prose,
                    heading: matches!(node, Node::Heading(_)),
                    line,
                });
            }
            if let Node::Heading(heading) = node {
                report.headings.push(Heading {
                    level: heading.depth,
                    anchor: anchor(&rendered, &mut anchors),
                    text: rendered,
                    line,
                    offset,
                });
            }
        } else if matches!(node, Node::Code(_) | Node::TableRow(_) | Node::Html(_))
            || code_container(node)
        {
            text.push(inline_text(node, false));
            if let Node::Html(html) = node {
                let prose = html_text(&html.value, true);
                if !prose.is_empty() {
                    report.blocks.push(ProseBlock {
                        text: prose,
                        heading: false,
                        line,
                    });
                }
            }
        }
    }
    report.text = text
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(report)
}

pub(super) fn markdown_text(source: &str) -> String {
    analyze(source, false)
        .expect("CommonMark accepts every input")
        .text
}

harn_vm_core::stdlib_module_installer!(Pure, pub(super) fn register_markdown_builtins);

/// Parse Markdown or MDX into headings, resolved links, prose blocks and plain text.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "markdown_parse(text: string, options?: {mdx?: bool}) -> {headings: list<{level: int, text: string, anchor: string, line: int, offset: int}>, links: list<{destination: string, image: bool, line: int, offset: int}>, blocks: list<{text: string, heading: bool, line: int}>, text: string}",
    category = "markdown"
)]
fn markdown_parse_impl(values: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let args = Args::new("markdown_parse", values);
    let source = args.string(0, "text")?;
    let mut options = args.options(1, "options")?;
    let mdx = options.bool_or("mdx", false)?;
    options.finish(&[])?;
    let report = analyze(source, mdx)?;
    let json = serde_json::to_value(report).expect("Markdown report contains only JSON values");
    Ok(harn_vm_core::schema::json_to_vm_value(&json))
}
