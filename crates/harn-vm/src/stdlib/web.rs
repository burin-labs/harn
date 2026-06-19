//! Pure web-source helpers backing `std/web`.

use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn vm_str(value: impl AsRef<str>) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value.as_ref()))
}

fn vm_list(values: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(values))
}

fn vm_dict(values: crate::value::DictMap) -> VmValue {
    VmValue::dict(values)
}

fn selector(pattern: &str) -> Selector {
    Selector::parse(pattern).expect("static selector should parse")
}

fn normalized_text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn element_text(element: ElementRef<'_>) -> String {
    normalized_text(element.text())
}

fn lower_attr(element: ElementRef<'_>, name: &str) -> Option<String> {
    element
        .value()
        .attr(name)
        .map(|value| value.trim().to_ascii_lowercase())
}

fn rel_contains(element: ElementRef<'_>, token: &str) -> bool {
    lower_attr(element, "rel")
        .map(|rel| rel.split_ascii_whitespace().any(|part| part == token))
        .unwrap_or(false)
}

fn resolve_url(base: Option<&Url>, href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    Url::parse(trimmed)
        .or_else(|_| {
            base.map(|base| base.join(trimmed))
                .unwrap_or_else(|| Url::parse(trimmed))
        })
        .map(|url| url.to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn html_text_without_scripts(html: &str) -> String {
    let stripped = Regex::new(
        r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>|<noscript\b[^>]*>.*?</noscript>",
    )
    .expect("static regex should parse")
    .replace_all(html, " ");
    let document = Html::parse_document(&stripped);
    let body_selector = selector("body");
    if let Some(body) = document.select(&body_selector).next() {
        return element_text(body);
    }
    normalized_text(document.root_element().text())
}

fn extract_meta(document: &Html) -> VmValue {
    let meta_selector = selector("meta");
    let mut meta = crate::value::DictMap::new();
    for element in document.select(&meta_selector) {
        let value = element
            .value()
            .attr("content")
            .or_else(|| element.value().attr("charset"))
            .map(str::trim)
            .unwrap_or("");
        if value.is_empty() {
            continue;
        }
        let key = element
            .value()
            .attr("name")
            .or_else(|| element.value().attr("property"))
            .or_else(|| element.value().attr("http-equiv"))
            .or_else(|| element.value().attr("charset"))
            .map(|key| key.trim().to_ascii_lowercase());
        if let Some(key) = key.filter(|key| !key.is_empty()) {
            meta.insert(key, vm_str(value));
        }
    }
    vm_dict(meta)
}

fn extract_canonical(document: &Html, base: Option<&Url>) -> VmValue {
    let link_selector = selector("link[href]");
    for element in document.select(&link_selector) {
        if rel_contains(element, "canonical") {
            if let Some(href) = element.value().attr("href") {
                return vm_str(resolve_url(base, href));
            }
        }
    }
    VmValue::Nil
}

fn extract_links(document: &Html, base: Option<&Url>) -> VmValue {
    let link_selector = selector("a[href]");
    let mut links = Vec::new();
    for element in document.select(&link_selector) {
        let Some(href) = element
            .value()
            .attr("href")
            .map(str::trim)
            .filter(|href| !href.is_empty())
        else {
            continue;
        };
        let mut row = crate::value::DictMap::new();
        row.insert("href".to_string(), vm_str(href));
        row.insert("url".to_string(), vm_str(resolve_url(base, href)));
        row.insert("text".to_string(), vm_str(element_text(element)));
        if let Some(title) = element
            .value()
            .attr("title")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            row.insert("title".to_string(), vm_str(title));
        }
        if let Some(rel) = element
            .value()
            .attr("rel")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            row.insert("rel".to_string(), vm_str(rel));
        }
        links.push(vm_dict(row));
    }
    vm_list(links)
}

fn extract_json_ld(document: &Html) -> VmValue {
    let script_selector = selector("script[type]");
    let mut blocks = Vec::new();
    for element in document.select(&script_selector) {
        let script_type = lower_attr(element, "type").unwrap_or_default();
        if script_type.split(';').next().map(str::trim) != Some("application/ld+json") {
            continue;
        }
        let raw = element
            .text()
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(parsed) => blocks.push(json_to_vm_value(&parsed)),
            Err(_) => blocks.push(vm_str(raw)),
        }
    }
    vm_list(blocks)
}

fn extract_tables(document: &Html) -> VmValue {
    let table_selector = selector("table");
    let caption_selector = selector("caption");
    let row_selector = selector("tr");
    let th_selector = selector("th");
    let cell_selector = selector("th, td");
    let td_selector = selector("td");
    let mut tables = Vec::new();

    for table in document.select(&table_selector) {
        let mut rendered = crate::value::DictMap::new();
        let caption = table
            .select(&caption_selector)
            .next()
            .map(element_text)
            .unwrap_or_default();
        rendered.insert("caption".to_string(), vm_str(caption));

        let mut headers: Vec<VmValue> = Vec::new();
        let mut rows: Vec<VmValue> = Vec::new();
        for row in table.select(&row_selector) {
            let th_cells = row
                .select(&th_selector)
                .map(element_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>();
            let td_cells = row
                .select(&td_selector)
                .map(element_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>();
            if headers.is_empty() && !th_cells.is_empty() {
                headers = th_cells.iter().map(vm_str).collect();
            }
            if !td_cells.is_empty() {
                let cells = row
                    .select(&cell_selector)
                    .map(element_text)
                    .filter(|text| !text.is_empty())
                    .map(vm_str)
                    .collect::<Vec<_>>();
                rows.push(vm_list(cells));
            }
        }
        rendered.insert("headers".to_string(), vm_list(headers));
        rendered.insert("rows".to_string(), vm_list(rows));
        tables.push(vm_dict(rendered));
    }
    vm_list(tables)
}

fn extract_html(html: &str, source_url: Option<&str>) -> VmValue {
    let document = Html::parse_document(html);
    let base = source_url.and_then(|url| Url::parse(url).ok());
    let title_selector = selector("title");
    let title = document
        .select(&title_selector)
        .next()
        .map(element_text)
        .filter(|text| !text.is_empty())
        .map(vm_str)
        .unwrap_or(VmValue::Nil);

    let mut out = crate::value::DictMap::new();
    out.insert("title".to_string(), title);
    out.insert("meta".to_string(), extract_meta(&document));
    out.insert(
        "canonical_url".to_string(),
        extract_canonical(&document, base.as_ref()),
    );
    out.insert("links".to_string(), extract_links(&document, base.as_ref()));
    out.insert("tables".to_string(), extract_tables(&document));
    out.insert("json_ld".to_string(), extract_json_ld(&document));
    out.insert("text".to_string(), vm_str(html_text_without_scripts(html)));
    vm_dict(out)
}

fn web_error(name: &str, message: impl std::fmt::Display) -> VmError {
    VmError::Thrown(vm_str(format!("{name}: {message}")))
}

pub(crate) fn register_web_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "__web_extract_html(html: string, source_url?: string?) -> dict",
    category = "web"
)]
fn web_extract_html_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let html = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let source_url = args.get(1).and_then(|value| match value {
        VmValue::Nil => None,
        other => Some(other.display()),
    });
    Ok(extract_html(&html, source_url.as_deref()))
}

#[harn_builtin(
    sig = "__web_resolve_url(base_url: string, href: string) -> string?",
    category = "web"
)]
fn web_resolve_url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let base = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let href = args.get(1).map(|value| value.display()).unwrap_or_default();
    if href.trim().is_empty() {
        return Ok(VmValue::Nil);
    }
    let parsed_base = Url::parse(&base).map_err(|error| web_error("__web_resolve_url", error))?;
    Ok(vm_str(resolve_url(Some(&parsed_base), &href)))
}

#[harn_builtin(
    sig = "__web_origin_url(url: string, path?: string) -> string",
    category = "web"
)]
fn web_origin_url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let path = args
        .get(1)
        .map(|value| value.display())
        .unwrap_or_else(|| "/".to_string());
    let mut parsed = Url::parse(&raw).map_err(|error| web_error("__web_origin_url", error))?;
    let normalized_path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    parsed.set_path(&normalized_path);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok(vm_str(parsed.as_str()))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &WEB_EXTRACT_HTML_IMPL_DEF,
    &WEB_RESOLVE_URL_IMPL_DEF,
    &WEB_ORIGIN_URL_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::extract_html;

    #[test]
    fn extracts_core_html_metadata() {
        let value = extract_html(
            r#"
            <html><head>
              <title>Example &amp; Test</title>
              <meta name="description" content="A short description">
              <link rel="canonical" href="/canonical">
              <script type="application/ld+json">{"name":"Example"}</script>
            </head><body>
              <a href="/docs">Docs</a>
              <table><tr><th>Name</th><th>Price</th></tr><tr><td>Pro</td><td>$20</td></tr></table>
            </body></html>
            "#,
            Some("https://example.com/base/page"),
        );
        let dict = value.as_dict().expect("extract_html returns a dict");
        assert_eq!(dict.get("title").unwrap().display(), "Example & Test");
        assert_eq!(
            dict.get("canonical_url").unwrap().display(),
            "https://example.com/canonical"
        );
        assert!(dict.get("text").unwrap().display().contains("Docs"));
    }
}
