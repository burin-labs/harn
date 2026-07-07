//! Cross-platform document conversion primitives.
//!
//! These builtins intentionally provide a dependency-light baseline:
//! text-like inputs become valid PDF bytes without shelling out to
//! platform converters. Higher-fidelity renderers can sit behind the
//! same stdlib Harn API later, but scripts should always have a portable
//! fallback that works in sandboxed runs and tests.

use std::sync::Arc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{optional_dict_arg, ErrorKind, OptionsParser};
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

const BUILTIN_RENDERER_ID: &str = "builtin_text_pdf";
const BUILTIN_RENDER_PDF: &str = "document_render_pdf";
const BUILTIN_EXTRACT_TEXT: &str = "document_extract_text";
const DEFAULT_TITLE: &str = "Harn Document";
const DEFAULT_PAGE_WIDTH_PT: usize = 612;
const DEFAULT_PAGE_HEIGHT_PT: usize = 792;
const DEFAULT_MARGIN_PT: usize = 54;
const DEFAULT_FONT_SIZE_PT: usize = 10;
const DEFAULT_LINE_HEIGHT_PT: usize = 14;
const SOURCE_FORMAT_TEXT: &str = "text";
const SOURCE_FORMAT_HTML: &str = "html";
const SOURCE_FORMAT_MARKDOWN: &str = "markdown";
const PDF_OUTPUT_FORMAT: &str = "pdf";
const RENDERER_KIND_BUILTIN: &str = "builtin";
const RENDERER_FIDELITY_TEXT_LAYOUT: &str = "text_layout";
const OPTION_RENDERER: &str = "renderer";
const OPTION_SOURCE_FORMAT: &str = "source_format";
const OPTION_TITLE: &str = "title";
const OPTION_PAGE_WIDTH_PT: &str = "page_width_pt";
const OPTION_PAGE_HEIGHT_PT: &str = "page_height_pt";
const OPTION_MARGIN_PT: &str = "margin_pt";
const OPTION_FONT_SIZE_PT: &str = "font_size_pt";
const OPTION_LINE_HEIGHT_PT: &str = "line_height_pt";
const OPTION_MAX_LINE_CHARS: &str = "max_line_chars";
const OPTION_METADATA: &str = "metadata";
const OPTION_PROVIDER_OPTIONS: &str = "provider_options";
const OPTION_RENDERER_OPTIONS: &str = "renderer_options";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceFormat {
    Text,
    Html,
    Markdown,
}

impl SourceFormat {
    fn parse(builtin: &str, value: Option<&str>, source: &str) -> Result<Self, VmError> {
        match value.unwrap_or_else(|| infer_source_format(source)) {
            SOURCE_FORMAT_TEXT | "txt" | "plain" | "text/plain" => Ok(Self::Text),
            SOURCE_FORMAT_HTML | "htm" | "text/html" => Ok(Self::Html),
            SOURCE_FORMAT_MARKDOWN | "md" | "text/markdown" => Ok(Self::Markdown),
            other => Err(VmError::Runtime(format!(
                "{builtin}: unsupported {OPTION_SOURCE_FORMAT} {other:?}; expected {SOURCE_FORMAT_TEXT}, {SOURCE_FORMAT_HTML}, or {SOURCE_FORMAT_MARKDOWN}"
            ))),
        }
    }
}

#[derive(Clone, Debug)]
struct PdfRenderOptions {
    source_format: SourceFormat,
    title: String,
    page_width_pt: usize,
    page_height_pt: usize,
    margin_pt: usize,
    font_size_pt: usize,
    line_height_pt: usize,
    max_line_chars: Option<usize>,
}

impl PdfRenderOptions {
    fn parse(source: &str, value: Option<&crate::value::DictMap>) -> Result<Self, VmError> {
        let Some(dict) = value else {
            return Ok(Self::default_for(source, None));
        };
        let mut parser = OptionsParser::new(BUILTIN_RENDER_PDF, dict, ErrorKind::Runtime);
        let renderer = parser
            .optional_string(OPTION_RENDERER)?
            .unwrap_or_else(|| BUILTIN_RENDERER_ID.to_string());
        if renderer != "auto" && renderer != BUILTIN_RENDERER_ID {
            return Err(VmError::Runtime(format!(
                "{BUILTIN_RENDER_PDF}: unsupported {OPTION_RENDERER} {renderer:?}; available renderer is {BUILTIN_RENDERER_ID:?}"
            )));
        }
        let source_format = SourceFormat::parse(
            BUILTIN_RENDER_PDF,
            parser.optional_string(OPTION_SOURCE_FORMAT)?.as_deref(),
            source,
        )?;
        let title = parser
            .optional_string_raw(OPTION_TITLE)?
            .unwrap_or_else(|| DEFAULT_TITLE.to_string());
        let page_width_pt = positive_usize(
            parser.optional_usize(OPTION_PAGE_WIDTH_PT)?,
            DEFAULT_PAGE_WIDTH_PT,
        );
        let page_height_pt = positive_usize(
            parser.optional_usize(OPTION_PAGE_HEIGHT_PT)?,
            DEFAULT_PAGE_HEIGHT_PT,
        );
        let margin_pt = positive_usize(parser.optional_usize(OPTION_MARGIN_PT)?, DEFAULT_MARGIN_PT);
        let font_size_pt = positive_usize(
            parser.optional_usize(OPTION_FONT_SIZE_PT)?,
            DEFAULT_FONT_SIZE_PT,
        );
        let line_height_pt = positive_usize(
            parser.optional_usize(OPTION_LINE_HEIGHT_PT)?,
            font_size_pt + 4,
        );
        let max_line_chars = parser.optional_usize(OPTION_MAX_LINE_CHARS)?;
        parser.allow(OPTION_METADATA);
        parser.allow(OPTION_PROVIDER_OPTIONS);
        parser.allow(OPTION_RENDERER_OPTIONS);
        parser.finish_strict(&[])?;
        Ok(Self {
            source_format,
            title,
            page_width_pt,
            page_height_pt,
            margin_pt,
            font_size_pt,
            line_height_pt,
            max_line_chars: max_line_chars.filter(|value| *value > 0),
        })
    }

    fn default_for(source: &str, title: Option<String>) -> Self {
        Self {
            source_format: SourceFormat::parse(BUILTIN_RENDER_PDF, None, source)
                .unwrap_or(SourceFormat::Text),
            title: title.unwrap_or_else(|| DEFAULT_TITLE.to_string()),
            page_width_pt: DEFAULT_PAGE_WIDTH_PT,
            page_height_pt: DEFAULT_PAGE_HEIGHT_PT,
            margin_pt: DEFAULT_MARGIN_PT,
            font_size_pt: DEFAULT_FONT_SIZE_PT,
            line_height_pt: DEFAULT_LINE_HEIGHT_PT,
            max_line_chars: None,
        }
    }

    fn effective_line_chars(&self) -> usize {
        if let Some(explicit) = self.max_line_chars {
            return explicit.clamp(8, 240);
        }
        let usable_width = self
            .page_width_pt
            .saturating_sub(self.margin_pt.saturating_mul(2))
            .max(120);
        (usable_width / (self.font_size_pt.max(1) / 2).max(4)).clamp(24, 160)
    }

    fn lines_per_page(&self) -> usize {
        let usable_height = self
            .page_height_pt
            .saturating_sub(self.margin_pt.saturating_mul(2))
            .max(self.line_height_pt.max(1));
        (usable_height / self.line_height_pt.max(1)).max(1)
    }
}

fn positive_usize(value: Option<usize>, default: usize) -> usize {
    value.filter(|value| *value > 0).unwrap_or(default)
}

pub(crate) fn register_document_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &DOCUMENT_RENDER_PDF_IMPL_DEF,
    &DOCUMENT_EXTRACT_TEXT_IMPL_DEF,
    &DOCUMENT_PDF_CAPABILITIES_IMPL_DEF,
];

#[harn_builtin(
    sig = "document_render_pdf(source: string | bytes | nil, options?: dict) -> bytes",
    category = "document",
    doc = "Render text-like document input to valid PDF bytes using Harn's built-in cross-platform renderer."
)]
fn document_render_pdf_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = document_source_arg(args.first());
    let options = PdfRenderOptions::parse(
        &source,
        optional_dict_arg(args, 1, BUILTIN_RENDER_PDF, "options", ErrorKind::Runtime)?,
    )?;
    let text = extract_text_for_format(&source, options.source_format);
    let pdf = render_text_pdf(&text, &options);
    Ok(VmValue::Bytes(Arc::new(pdf)))
}

#[harn_builtin(
    sig = "document_extract_text(source: string | bytes | nil, options?: dict) -> string",
    category = "document",
    doc = "Extract normalized text from text, HTML, or Markdown input."
)]
fn document_extract_text_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = document_source_arg(args.first());
    let options = optional_dict_arg(args, 1, BUILTIN_EXTRACT_TEXT, "options", ErrorKind::Runtime)?;
    let format = if let Some(dict) = options {
        let mut parser = OptionsParser::new(BUILTIN_EXTRACT_TEXT, dict, ErrorKind::Runtime);
        let format = SourceFormat::parse(
            BUILTIN_EXTRACT_TEXT,
            parser.optional_string(OPTION_SOURCE_FORMAT)?.as_deref(),
            &source,
        )?;
        parser.finish_strict(&[])?;
        format
    } else {
        SourceFormat::parse(BUILTIN_EXTRACT_TEXT, None, &source)?
    };
    Ok(VmValue::String(arcstr::ArcStr::from(
        extract_text_for_format(&source, format),
    )))
}

#[harn_builtin(
    sig = "document_pdf_capabilities(options?: dict) -> dict",
    category = "document",
    doc = "Describe the built-in PDF rendering capability available in this Harn runtime."
)]
fn document_pdf_capabilities_impl(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut renderer = crate::value::DictMap::new();
    renderer.put_str("id", BUILTIN_RENDERER_ID);
    renderer.put_str("kind", RENDERER_KIND_BUILTIN);
    renderer.put_str("fidelity", RENDERER_FIDELITY_TEXT_LAYOUT);
    renderer.insert(
        crate::value::intern_key("source_formats"),
        VmValue::List(Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from(SOURCE_FORMAT_TEXT)),
            VmValue::String(arcstr::ArcStr::from(SOURCE_FORMAT_HTML)),
            VmValue::String(arcstr::ArcStr::from(SOURCE_FORMAT_MARKDOWN)),
        ])),
    );
    renderer.insert(
        crate::value::intern_key("output_formats"),
        VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
            PDF_OUTPUT_FORMAT,
        ))])),
    );
    renderer.insert(
        crate::value::intern_key("external_dependencies"),
        VmValue::List(Arc::new(Vec::new())),
    );

    let mut out = crate::value::DictMap::new();
    out.put_str("default_renderer", BUILTIN_RENDERER_ID);
    out.insert(
        crate::value::intern_key("renderers"),
        VmValue::List(Arc::new(vec![VmValue::dict(renderer)])),
    );
    Ok(VmValue::dict(out))
}

fn document_source_arg(value: Option<&VmValue>) -> String {
    match value {
        Some(VmValue::String(text)) => text.to_string(),
        Some(VmValue::Bytes(bytes)) => String::from_utf8_lossy(bytes.as_slice()).into_owned(),
        Some(VmValue::Nil) | None => String::new(),
        Some(other) => other.display(),
    }
}

fn infer_source_format(source: &str) -> &'static str {
    let trimmed = source.trim_start();
    if trimmed.starts_with("<!doctype html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<body")
        || trimmed.starts_with("<div")
        || trimmed.starts_with("<p")
    {
        "html"
    } else {
        SOURCE_FORMAT_TEXT
    }
}

fn extract_text_for_format(source: &str, format: SourceFormat) -> String {
    match format {
        SourceFormat::Text => normalize_text(source),
        SourceFormat::Html => extract_html_text(source),
        SourceFormat::Markdown => strip_markdown(source),
    }
}

fn normalize_text(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

fn extract_html_text(source: &str) -> String {
    let document = scraper::Html::parse_document(source);
    let body_selector = scraper::Selector::parse("body").expect("static body selector");
    let root_selector = scraper::Selector::parse("html").expect("static html selector");
    let mut raw = Vec::new();
    let mut selected = false;
    for node in document.select(&body_selector) {
        selected = true;
        raw.extend(node.text());
    }
    if !selected {
        for node in document.select(&root_selector) {
            selected = true;
            raw.extend(node.text());
        }
    }
    if !selected {
        raw.extend(document.root_element().text());
    }
    collapse_inline_whitespace(&raw.join(" "))
}

fn strip_markdown(source: &str) -> String {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in normalize_text(source).lines() {
        let mut line = raw.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            line = line.trim_start_matches('#').trim_start();
            line = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .or_else(|| line.strip_prefix("+ "))
                .unwrap_or(line);
            line = line.strip_prefix("> ").unwrap_or(line);
        }
        lines.push(strip_markdown_inline(line));
    }
    lines.join("\n")
}

fn strip_markdown_inline(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '*' | '_' | '`') {
            continue;
        }
        if ch == '[' {
            let mut label = String::new();
            for inner in chars.by_ref() {
                if inner == ']' {
                    break;
                }
                label.push(inner);
            }
            if chars.peek() == Some(&'(') {
                let _ = chars.next();
                for inner in chars.by_ref() {
                    if inner == ')' {
                        break;
                    }
                }
            }
            out.push_str(&label);
            continue;
        }
        out.push(ch);
    }
    out
}

fn collapse_inline_whitespace(source: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for ch in source.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

fn render_text_pdf(text: &str, options: &PdfRenderOptions) -> Vec<u8> {
    let lines = wrap_lines(text, options.effective_line_chars());
    let lines_per_page = options.lines_per_page();
    let pages: Vec<Vec<String>> = if lines.is_empty() {
        vec![vec![String::new()]]
    } else {
        lines
            .chunks(lines_per_page)
            .map(|chunk| chunk.to_vec())
            .collect()
    };

    let font_id = 3 + pages.len() * 2;
    let mut objects = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids = (0..pages.len())
        .map(|index| format!("{} 0 R", 3 + index * 2))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids,
        pages.len()
    ));
    for (index, page_lines) in pages.iter().enumerate() {
        let page_id = 3 + index * 2;
        let content_id = page_id + 1;
        let content = pdf_page_content(page_lines, options);
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
            options.page_width_pt, options.page_height_pt, font_id, content_id
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content.len(),
            content
        ));
    }
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
    }
    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_offset
        )
        .as_bytes(),
    );
    out
}

fn pdf_page_content(lines: &[String], options: &PdfRenderOptions) -> String {
    let start_y = options.page_height_pt.saturating_sub(options.margin_pt);
    let mut content = format!(
        "BT\n/F1 {} Tf\n{} TL\n{} {} Td\n",
        options.font_size_pt, options.line_height_pt, options.margin_pt, start_y
    );
    if !options.title.trim().is_empty() {
        content.push_str(&format!("({}) Tj\nT*\n", escape_pdf_string(&options.title)));
    }
    for line in lines {
        content.push_str(&format!("({}) Tj\nT*\n", escape_pdf_string(line)));
    }
    content.push_str("ET\n");
    content
}

fn wrap_lines(source: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let normalized = raw_line.replace('\t', "    ");
        if normalized.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in normalized.split_whitespace() {
            let extra = usize::from(!current.is_empty());
            if current.chars().count() + word.chars().count() + extra > max_chars
                && !current.is_empty()
            {
                out.push(std::mem::take(&mut current));
            }
            if word.chars().count() > max_chars {
                for ch in word.chars() {
                    if current.chars().count() >= max_chars {
                        out.push(std::mem::take(&mut current));
                    }
                    current.push(ch);
                }
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn escape_pdf_string(source: &str) -> String {
    let mut out = String::new();
    for ch in source.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("    "),
            ch if ch.is_control() => {}
            ch if ch.is_ascii() => out.push(ch),
            ch => out.push_str(&pdf_doc_encoding_fallback(ch)),
        }
    }
    out
}

fn pdf_doc_encoding_fallback(ch: char) -> String {
    match ch {
        '’' | '‘' => "'".to_string(),
        '“' | '”' => "\"".to_string(),
        '–' | '—' => "-".to_string(),
        '…' => "...".to_string(),
        _ => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    #[test]
    fn render_pdf_returns_valid_pdf_envelope() {
        let value = document_render_pdf_impl(&[s("hello receipt")], &mut String::new()).unwrap();
        let VmValue::Bytes(bytes) = value else {
            panic!("expected bytes");
        };
        let text = String::from_utf8(bytes.as_ref().clone()).expect("ascii pdf");
        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("xref\n"));
        assert!(text.contains("trailer\n"));
        assert!(text.ends_with("%%EOF\n"));
    }

    #[test]
    fn html_extraction_uses_visible_text() {
        let got = extract_text_for_format(
            "<html><body><h1>Receipt</h1><p>Amount $5.00</p></body></html>",
            SourceFormat::Html,
        );
        assert_eq!(got, "Receipt Amount $5.00");
    }

    #[test]
    fn markdown_extraction_strips_common_markup() {
        let got = extract_text_for_format(
            "# Title\n- **Amount** [link](https://x.test)",
            SourceFormat::Markdown,
        );
        assert_eq!(got, "Title\nAmount link");
    }
}
