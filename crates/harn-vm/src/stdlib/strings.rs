use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{values_equal, VmError, VmValue};
use crate::vm::Vm;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

fn split_snake(s: &str) -> Vec<&str> {
    s.split('_').filter(|p| !p.is_empty()).collect()
}

fn split_kebab(s: &str) -> Vec<&str> {
    s.split('-').filter(|p| !p.is_empty()).collect()
}

/// Splits a camelCase or PascalCase string into lowercase words.
/// `"HTTPServer"` → `["http", "server"]`.
/// `"testFilePatterns"` → `["test", "file", "patterns"]`.
fn split_camel(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        if i > 0 && c.is_uppercase() {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let prev_lower_or_digit = prev.is_lowercase() || prev.is_ascii_digit();
            let acronym_end = prev.is_uppercase() && next.is_some_and(|n| n.is_lowercase());
            if (prev_lower_or_digit || acronym_end) && !cur.is_empty() {
                words.push(cur.clone());
                cur.clear();
            }
        }
        for lc in c.to_lowercase() {
            cur.push(lc);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn uppercase_first_str(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn lowercase_first_str(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn words_to_camel<S: AsRef<str>>(words: &[S]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        let lower = w.as_ref().to_lowercase();
        if i == 0 {
            out.push_str(&lower);
        } else {
            out.push_str(&uppercase_first_str(&lower));
        }
    }
    out
}

fn words_to_pascal<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|w| uppercase_first_str(&w.as_ref().to_lowercase()))
        .collect()
}

fn words_to_snake<S: AsRef<str>>(words: &[S]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push('_');
        }
        out.push_str(&w.as_ref().to_lowercase());
    }
    out
}

fn words_to_kebab<S: AsRef<str>>(words: &[S]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        out.push_str(&w.as_ref().to_lowercase());
    }
    out
}

use crate::stdlib::template::{
    render_asset_result, render_asset_with_provenance_result, render_template_result,
    PromptSourceSpan, PromptSpanKind, TemplateAsset,
};

fn render_template_string(args: &[VmValue]) -> Result<VmValue, VmError> {
    let template = args.first().map(|a| a.display()).unwrap_or_default();
    let bindings = args.get(1).and_then(|a| a.as_dict());
    let rendered =
        render_template_result(&template, bindings, None, None).map_err(VmError::from)?;
    Ok(VmValue::String(std::sync::Arc::from(rendered)))
}

fn render_asset(args: &[VmValue]) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let asset = resolve_render_target(&path)?;
    let bindings = args.get(1).and_then(|a| a.as_dict());
    let rendered = render_asset_result(&asset, bindings).map_err(VmError::from)?;
    Ok(VmValue::String(std::sync::Arc::from(rendered)))
}

/// Resolve a `render(...)` / `render_prompt(...)` target, honoring the
/// `@/` and `@<alias>/` package-root forms (issue #742) and falling back
/// to the legacy source-relative path resolver for plain strings.
fn resolve_render_target(path: &str) -> Result<TemplateAsset, VmError> {
    TemplateAsset::render_target(path)
        .map_err(|msg| VmError::Thrown(VmValue::String(std::sync::Arc::from(msg))))
}

/// `render_with_provenance(path, bindings)` — the debugger's hook for
/// the prompt-template source-map UX (burin-code #93/#94). Returns
/// `{ text: string, template_uri: string, spans: list<dict> }` where
/// each span carries the template range that produced an output byte
/// range so the IDE can highlight the originating `.harn.prompt`
/// section when a user clicks a chunk of the rendered prompt.
fn render_asset_with_provenance(args: &[VmValue]) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let asset = resolve_render_target(&path)?;
    let bindings = args.get(1).and_then(|a| a.as_dict());
    let (rendered, spans) =
        render_asset_with_provenance_result(&asset, bindings, true).map_err(VmError::from)?;
    // Register in the thread-local provenance map so the DAP adapter
    // can answer `burin/promptProvenance` / `burin/promptConsumers`
    // queries for this render by id — the IDE only needs the id;
    // span payloads arrive over the DAP channel on demand.
    let prompt_id = crate::stdlib::template::register_prompt(
        asset.uri.clone(),
        rendered.clone(),
        spans.clone(),
    );
    Ok(provenance_result_dict(
        rendered, asset.uri, prompt_id, &spans,
    ))
}

fn provenance_result_dict(
    rendered: String,
    template_uri: String,
    prompt_id: String,
    spans: &[PromptSourceSpan],
) -> VmValue {
    let spans_list: Vec<VmValue> = spans.iter().map(span_to_vm_dict).collect();
    let mut out = std::collections::BTreeMap::new();
    out.insert(
        "text".to_string(),
        VmValue::String(std::sync::Arc::from(rendered)),
    );
    out.insert(
        "template_uri".to_string(),
        VmValue::String(std::sync::Arc::from(template_uri)),
    );
    out.insert(
        "prompt_id".to_string(),
        VmValue::String(std::sync::Arc::from(prompt_id)),
    );
    out.insert(
        "spans".to_string(),
        VmValue::List(std::sync::Arc::new(spans_list)),
    );
    VmValue::Dict(std::sync::Arc::new(out))
}

fn span_to_vm_dict(span: &PromptSourceSpan) -> VmValue {
    let mut d = std::collections::BTreeMap::new();
    d.insert(
        "template_line".into(),
        VmValue::Int(span.template_line as i64),
    );
    d.insert(
        "template_col".into(),
        VmValue::Int(span.template_col as i64),
    );
    d.insert(
        "output_start".into(),
        VmValue::Int(span.output_start as i64),
    );
    d.insert("output_end".into(), VmValue::Int(span.output_end as i64));
    d.insert(
        "kind".into(),
        VmValue::String(std::sync::Arc::from(span_kind_label(span.kind))),
    );
    d.insert(
        "template_uri".into(),
        VmValue::String(std::sync::Arc::from(span.template_uri.as_str())),
    );
    if let Some(ref v) = span.bound_value {
        d.insert(
            "bound_value".into(),
            VmValue::String(std::sync::Arc::from(v.as_str())),
        );
    }
    if let Some(parent) = span.parent_span.as_deref() {
        d.insert("parent_span".into(), span_to_vm_dict(parent));
    }
    VmValue::Dict(std::sync::Arc::new(d))
}

fn span_kind_label(kind: PromptSpanKind) -> &'static str {
    match kind {
        PromptSpanKind::Text => "text",
        PromptSpanKind::Expr => "expr",
        PromptSpanKind::LegacyBareInterp => "legacy_bare",
        PromptSpanKind::If => "if",
        PromptSpanKind::ForIteration => "for_iteration",
        PromptSpanKind::Include => "include",
        PromptSpanKind::Section => "section",
    }
}

pub(crate) fn register_string_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "format(template: string, ...rest: any) -> string",
    category = "strings"
)]
fn format_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let template = args.first().map(|a| a.display()).unwrap_or_default();

    // Dict → named placeholders `{key}`. Single-pass scan avoids double-substitution.
    if let Some(dict) = args.get(1).and_then(|a| a.as_dict()) {
        let mut result = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            if let Some(close) = rest[open..].find('}') {
                let key = &rest[open + 1..open + close];
                if let Some(val) = dict.get(key) {
                    result.push_str(&val.display());
                } else {
                    result.push_str(&rest[open..open + close + 1]);
                }
                rest = &rest[open + close + 1..];
            } else {
                result.push_str(&rest[open..]);
                rest = "";
                break;
            }
        }
        result.push_str(rest);
        return Ok(VmValue::String(std::sync::Arc::from(result)));
    }

    // Otherwise: positional `{}` placeholders.
    let mut result = String::with_capacity(template.len());
    let mut arg_iter = args.iter().skip(1);
    let mut rest = template.as_str();
    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        if let Some(arg) = arg_iter.next() {
            result.push_str(&arg.display());
        } else {
            result.push_str("{}");
        }
        rest = &rest[pos + 2..];
    }
    result.push_str(rest);
    Ok(VmValue::String(std::sync::Arc::from(result)))
}

#[harn_builtin(sig = "trim(text: string?) -> string", category = "strings")]
fn trim_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(s.trim())))
}

#[harn_builtin(sig = "lowercase(text: string?) -> string", category = "strings")]
fn lowercase_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(s.to_lowercase())))
}

#[harn_builtin(sig = "uppercase(text: string?) -> string", category = "strings")]
fn uppercase_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(s.to_uppercase())))
}

#[harn_builtin(
    sig = "split(text: string?, separator?: string) -> list",
    category = "strings"
)]
fn split_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let sep = args
        .get(1)
        .map(|a| a.display())
        .unwrap_or_else(|| " ".to_string());
    let parts: Vec<VmValue> = s
        .split(&sep)
        .map(|p| VmValue::String(std::sync::Arc::from(p)))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(parts)))
}

#[harn_builtin(
    sig = "unicode_normalize(text: string?, form?: string) -> string",
    category = "strings"
)]
fn unicode_normalize_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let form = args
        .get(1)
        .map(|a| a.display().to_uppercase())
        .unwrap_or_else(|| "NFC".to_string());
    let normalized: String = match form.as_str() {
        "NFC" => s.nfc().collect(),
        "NFD" => s.nfd().collect(),
        "NFKC" => s.nfkc().collect(),
        "NFKD" => s.nfkd().collect(),
        _ => {
            return Err(VmError::Runtime(
                "unicode_normalize: form must be NFC, NFD, NFKC, or NFKD".to_string(),
            ));
        }
    };
    Ok(VmValue::String(std::sync::Arc::from(normalized)))
}

#[harn_builtin(sig = "unicode_graphemes(text: string?) -> list", category = "strings")]
fn unicode_graphemes_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::List(std::sync::Arc::new(
        UnicodeSegmentation::graphemes(s.as_str(), true)
            .map(|grapheme| VmValue::String(std::sync::Arc::from(grapheme)))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "str_pad(text: string?, width: int, fill?: string, side?: string) -> string",
    category = "strings"
)]
fn str_pad_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let width = args.get(1).and_then(VmValue::as_int).unwrap_or(0).max(0) as usize;
    let fill = args
        .get(2)
        .map(|a| a.display())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| " ".to_string());
    let fill = UnicodeSegmentation::graphemes(fill.as_str(), true)
        .next()
        .unwrap_or(" ");
    let side = args
        .get(3)
        .map(|a| a.display().to_lowercase())
        .unwrap_or_else(|| "right".to_string());
    let grapheme_count = UnicodeSegmentation::graphemes(s.as_str(), true).count();
    if grapheme_count >= width {
        return Ok(VmValue::String(std::sync::Arc::from(s)));
    }
    let needed = width - grapheme_count;
    let (left, right) = match side.as_str() {
        "left" => (needed, 0),
        "right" => (0, needed),
        "both" => (needed / 2, needed - needed / 2),
        _ => {
            return Err(VmError::Runtime(
                "str_pad: side must be left, right, or both".to_string(),
            ));
        }
    };
    Ok(VmValue::String(std::sync::Arc::from(format!(
        "{}{}{}",
        crate::limits::checked_repeat(fill, left)?,
        s,
        crate::limits::checked_repeat(fill, right)?
    ))))
}

#[harn_builtin(
    sig = "starts_with(text: string?, prefix: string?) -> bool",
    category = "strings"
)]
fn starts_with_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let prefix = args.get(1).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::Bool(s.starts_with(&prefix)))
}

#[harn_builtin(
    sig = "ends_with(text: string?, suffix: string) -> bool",
    category = "strings"
)]
fn ends_with_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let suffix = args.get(1).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::Bool(s.ends_with(&suffix)))
}

#[harn_builtin(
    sig = "contains(haystack: string | list, needle: any) -> bool",
    category = "strings"
)]
fn contains_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::String(s) => {
            let substr = args.get(1).map(|a| a.display()).unwrap_or_default();
            Ok(VmValue::Bool(s.contains(&substr)))
        }
        VmValue::List(items) => {
            let target = args.get(1).unwrap_or(&VmValue::Nil);
            Ok(VmValue::Bool(
                items.iter().any(|item| values_equal(item, target)),
            ))
        }
        _ => Ok(VmValue::Bool(false)),
    }
}

#[harn_builtin(
    sig = "index_of(haystack: string?, needle: string, from?: int) -> int",
    category = "strings"
)]
fn index_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // The missing sibling of `starts_with`/`ends_with`/`contains`. Char-indexed
    // to pair with `substring`/`slice`: `from` is a char offset and the result
    // is a char index (or -1 when absent).
    let haystack = args.first().map(|a| a.display()).unwrap_or_default();
    let needle = args.get(1).map(|a| a.display()).unwrap_or_default();
    let chars: Vec<char> = haystack.chars().collect();
    let from = args.get(2).and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
    let from = from.min(chars.len());
    let byte_from: usize = chars[..from].iter().map(|c| c.len_utf8()).sum();
    match haystack[byte_from..].find(&needle) {
        Some(rel) => {
            let char_idx = haystack[..byte_from + rel].chars().count();
            Ok(VmValue::Int(char_idx as i64))
        }
        None => Ok(VmValue::Int(-1)),
    }
}

#[harn_builtin(
    sig = "replace(text: string?, old: string, new: string) -> string",
    category = "strings"
)]
fn replace_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let old = args.get(1).map(|a| a.display()).unwrap_or_default();
    let new = args.get(2).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(s.replace(&old, &new))))
}

#[harn_builtin(
    sig = "join(items: list, separator?: string) -> string",
    category = "strings"
)]
fn join_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let sep = args.get(1).map(|a| a.display()).unwrap_or_default();
    match args.first() {
        Some(VmValue::List(items)) => {
            let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
            Ok(VmValue::String(std::sync::Arc::from(parts.join(&sep))))
        }
        _ => Ok(VmValue::String(std::sync::Arc::from(""))),
    }
}

#[harn_builtin(
    sig = "substring(text: string?, start: int, length?: int) -> string",
    category = "strings"
)]
fn substring_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let start = args.get(1).and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
    let chars: Vec<char> = s.chars().collect();
    let start = start.min(chars.len());
    match args.get(2).and_then(|a| a.as_int()) {
        Some(length) => {
            let length = (length.max(0) as usize).min(chars.len() - start);
            let result: String = chars[start..start + length].iter().collect();
            Ok(VmValue::String(std::sync::Arc::from(result)))
        }
        None => {
            let result: String = chars[start..].iter().collect();
            Ok(VmValue::String(std::sync::Arc::from(result)))
        }
    }
}

#[harn_builtin(sig = "chars(text: string?) -> list", category = "strings")]
fn chars_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first() {
        // Borrow the backing `Arc<str>` directly for the common string case so
        // a full-file scan does not copy the source before materializing it.
        Some(VmValue::String(text)) => Ok(VmValue::chars_list(text)),
        Some(other) => Ok(VmValue::chars_list(&other.display())),
        None => Ok(VmValue::List(std::sync::Arc::new(Vec::new()))),
    }
}

#[harn_builtin(sig = "snake_to_camel(text: string?) -> string", category = "strings")]
fn snake_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_camel(
        &split_snake(&s),
    ))))
}

#[harn_builtin(sig = "snake_to_pascal(text: string?) -> string", category = "strings")]
fn snake_to_pascal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_pascal(
        &split_snake(&s),
    ))))
}

#[harn_builtin(sig = "camel_to_snake(text: string?) -> string", category = "strings")]
fn camel_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_snake(
        &split_camel(&s),
    ))))
}

#[harn_builtin(sig = "pascal_to_snake(text: string?) -> string", category = "strings")]
fn pascal_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_snake(
        &split_camel(&s),
    ))))
}

#[harn_builtin(sig = "kebab_to_camel(text: string?) -> string", category = "strings")]
fn kebab_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_camel(
        &split_kebab(&s),
    ))))
}

#[harn_builtin(sig = "camel_to_kebab(text: string?) -> string", category = "strings")]
fn camel_to_kebab_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_kebab(
        &split_camel(&s),
    ))))
}

#[harn_builtin(sig = "snake_to_kebab(text: string?) -> string", category = "strings")]
fn snake_to_kebab_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_kebab(
        &split_snake(&s),
    ))))
}

#[harn_builtin(sig = "kebab_to_snake(text: string?) -> string", category = "strings")]
fn kebab_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(words_to_snake(
        &split_kebab(&s),
    ))))
}

#[harn_builtin(sig = "pascal_to_camel(text: string?) -> string", category = "strings")]
fn pascal_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(lowercase_first_str(
        &s,
    ))))
}

#[harn_builtin(sig = "camel_to_pascal(text: string?) -> string", category = "strings")]
fn camel_to_pascal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(uppercase_first_str(
        &s,
    ))))
}

#[harn_builtin(sig = "title_case(text: string?) -> string", category = "strings")]
fn title_case_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for c in s.chars() {
        if c.is_whitespace() {
            at_word_start = true;
            out.push(c);
        } else if at_word_start {
            out.extend(c.to_uppercase());
            at_word_start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    Ok(VmValue::String(std::sync::Arc::from(out)))
}

#[harn_builtin(sig = "uppercase_first(text: string?) -> string", category = "strings")]
fn uppercase_first_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(uppercase_first_str(
        &s,
    ))))
}

#[harn_builtin(sig = "lowercase_first(text: string?) -> string", category = "strings")]
fn lowercase_first_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(lowercase_first_str(
        &s,
    ))))
}

#[harn_builtin(sig = "dirname(path: string?) -> string", category = "strings")]
fn dirname_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.parent() {
        Some(parent) => Ok(VmValue::String(std::sync::Arc::from(
            parent.to_string_lossy().as_ref(),
        ))),
        None => Ok(VmValue::String(std::sync::Arc::from(""))),
    }
}

#[harn_builtin(sig = "basename(path: string?) -> string", category = "strings")]
fn basename_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.file_name() {
        Some(name) => Ok(VmValue::String(std::sync::Arc::from(
            name.to_string_lossy().as_ref(),
        ))),
        None => Ok(VmValue::String(std::sync::Arc::from(""))),
    }
}

#[harn_builtin(sig = "extname(path: string?) -> string", category = "strings")]
fn extname_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.extension() {
        Some(ext) => Ok(VmValue::String(std::sync::Arc::from(format!(
            ".{}",
            ext.to_string_lossy()
        )))),
        None => Ok(VmValue::String(std::sync::Arc::from(""))),
    }
}

#[harn_builtin(
    sig = "render(path: string?, bindings?: dict) -> string",
    aliases = ["render_prompt"],
    category = "strings"
)]
fn render_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    render_asset(args)
}

#[harn_builtin(
    sig = "render_string(template: string, bindings?: dict) -> string",
    category = "strings"
)]
fn render_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    render_template_string(args)
}

#[harn_builtin(
    sig = "render_with_provenance(path: string?, bindings?: dict) -> dict",
    category = "strings"
)]
fn render_with_provenance_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    render_asset_with_provenance(args)
}

// #106: pipelines invoke prompt_mark_rendered(prompt_id) just
// before passing a rendered prompt to llm_call. The builtin
// records (prompt_id, next_event_index) against the thread-local
// render-index map, which the DAP `burin/promptConsumers`
// response exposes so the IDE template gutter can jump-to-next-
// render. The `next_event_index` is a session-opaque counter —
// the IDE correlates it to AgentEvent.index via timestamp in the
// JSONL, or via the monotonic per-session render counter when
// scrubbing by render ordinal.
#[harn_builtin(
    sig = "prompt_mark_rendered(prompt_id: string) -> int",
    category = "strings"
)]
fn prompt_mark_rendered_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::String(prompt_id)) = args.first() else {
        return Err(VmError::TypeError(
            "prompt_mark_rendered: prompt_id must be a string".into(),
        ));
    };
    let event_index = crate::stdlib::template::next_prompt_render_ordinal();
    crate::stdlib::template::record_prompt_render_index(prompt_id, event_index);
    Ok(VmValue::Int(event_index as i64))
}

// Internal LLM render-context plumbing — paired push/pop hooks used
// by the agent_loop / handler-stack stdlib so user-authored
// `.harn.prompt` partials can branch on `llm.provider`,
// `llm.capabilities.native_tools`, etc. while rendering inside an
// LLM-aware frame. Userspace never calls these directly; the stdlib
// wraps every push in a matching `defer { __pop_llm_render_context() }`.
#[harn_builtin(
    sig = "__push_llm_render_context(provider: string, model?: string) -> bool",
    category = "strings"
)]
fn push_llm_render_context_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    let model = args.get(1).map(|a| a.display()).unwrap_or_default();
    // Skip the push when the caller hasn't resolved a concrete
    // provider yet (empty or `"auto"`). Templates rendered while
    // the unresolved options are in scope simply see `llm = nil`
    // instead of an empty-capabilities frame that would silently
    // mis-flag every capability check as false.
    if provider.is_empty() || provider.eq_ignore_ascii_case("auto") {
        return Ok(VmValue::Bool(false));
    }
    crate::stdlib::template::push_llm_render_context(
        crate::stdlib::template::LlmRenderContext::resolve(&provider, &model),
    );
    Ok(VmValue::Bool(true))
}

#[harn_builtin(sig = "__pop_llm_render_context() -> nil", category = "strings")]
fn pop_llm_render_context_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::stdlib::template::pop_llm_render_context();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "repeat(text: string?, count: int) -> string",
    category = "strings"
)]
fn repeat_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let count = args.get(1).and_then(VmValue::as_int).unwrap_or(0);
    if count <= 0 {
        return Ok(VmValue::String(std::sync::Arc::from("")));
    }
    // Reject pathological sizes so a typo doesn't try to allocate
    // multi-gigabyte strings (or panic `capacity overflow`) inside a script.
    let repeated = crate::limits::checked_repeat(&s, count as usize)?;
    Ok(VmValue::String(std::sync::Arc::from(repeated)))
}

#[harn_builtin(
    sig = "indent(text: string?, prefix?: string) -> string",
    category = "strings"
)]
fn indent_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let prefix = args
        .get(1)
        .map(|a| a.display())
        .unwrap_or_else(|| "  ".to_string());
    if prefix.is_empty() || text.is_empty() {
        return Ok(VmValue::String(std::sync::Arc::from(text)));
    }
    let mut out = String::with_capacity(text.len() + prefix.len() * 4);
    for line in text.split_inclusive('\n') {
        // Don't pad blank lines — matches Python's textwrap.indent
        // default behavior so callers don't get trailing whitespace
        // on empty lines.
        let visible = line.trim_end_matches('\n');
        if !visible.is_empty() {
            out.push_str(&prefix);
        }
        out.push_str(line);
    }
    Ok(VmValue::String(std::sync::Arc::from(out)))
}

#[harn_builtin(sig = "dedent(text: string?) -> string", category = "strings")]
fn dedent_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(dedent_str(&text))))
}

#[harn_builtin(
    sig = "word_wrap(text: string?, width?: int) -> string",
    category = "strings"
)]
fn word_wrap_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let width = args.get(1).and_then(VmValue::as_int).unwrap_or(80).max(1) as usize;
    Ok(VmValue::String(std::sync::Arc::from(word_wrap_str(
        &text, width,
    ))))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &FORMAT_IMPL_DEF,
    &TRIM_IMPL_DEF,
    &LOWERCASE_IMPL_DEF,
    &UPPERCASE_IMPL_DEF,
    &SPLIT_IMPL_DEF,
    &UNICODE_NORMALIZE_IMPL_DEF,
    &UNICODE_GRAPHEMES_IMPL_DEF,
    &STR_PAD_IMPL_DEF,
    &STARTS_WITH_IMPL_DEF,
    &ENDS_WITH_IMPL_DEF,
    &CONTAINS_IMPL_DEF,
    &INDEX_OF_IMPL_DEF,
    &REPLACE_IMPL_DEF,
    &JOIN_IMPL_DEF,
    &SUBSTRING_IMPL_DEF,
    &CHARS_IMPL_DEF,
    &SNAKE_TO_CAMEL_IMPL_DEF,
    &SNAKE_TO_PASCAL_IMPL_DEF,
    &CAMEL_TO_SNAKE_IMPL_DEF,
    &PASCAL_TO_SNAKE_IMPL_DEF,
    &KEBAB_TO_CAMEL_IMPL_DEF,
    &CAMEL_TO_KEBAB_IMPL_DEF,
    &SNAKE_TO_KEBAB_IMPL_DEF,
    &KEBAB_TO_SNAKE_IMPL_DEF,
    &PASCAL_TO_CAMEL_IMPL_DEF,
    &CAMEL_TO_PASCAL_IMPL_DEF,
    &TITLE_CASE_IMPL_DEF,
    &UPPERCASE_FIRST_IMPL_DEF,
    &LOWERCASE_FIRST_IMPL_DEF,
    &DIRNAME_IMPL_DEF,
    &BASENAME_IMPL_DEF,
    &EXTNAME_IMPL_DEF,
    &RENDER_IMPL_DEF,
    &RENDER_STRING_IMPL_DEF,
    &RENDER_WITH_PROVENANCE_IMPL_DEF,
    &PROMPT_MARK_RENDERED_IMPL_DEF,
    &PUSH_LLM_RENDER_CONTEXT_IMPL_DEF,
    &POP_LLM_RENDER_CONTEXT_IMPL_DEF,
    &REPEAT_IMPL_DEF,
    &INDENT_IMPL_DEF,
    &DEDENT_IMPL_DEF,
    &WORD_WRAP_IMPL_DEF,
];

/// Strips the common leading whitespace from every non-blank line. The
/// minimum prefix length wins (tabs and spaces are compared verbatim,
/// matching Python's `textwrap.dedent` semantics). Useful for heredoc-
/// style `.harn.prompt` rendering where the surrounding code indents the
/// content but the rendered prompt should appear flush-left.
fn dedent_str(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut shortest: Option<&str> = None;
    for line in &lines {
        let visible = line.trim_end_matches('\n');
        if visible.trim().is_empty() {
            continue;
        }
        let leading_len = visible.len() - visible.trim_start().len();
        let leading = &visible[..leading_len];
        shortest = Some(match shortest {
            None => leading,
            Some(prev) => common_prefix(prev, leading),
        });
        if shortest.map(str::is_empty).unwrap_or(false) {
            break;
        }
    }
    let prefix = shortest.unwrap_or("");
    if prefix.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for line in lines {
        let visible = line.trim_end_matches('\n');
        if visible.trim().is_empty() {
            out.push_str(line);
        } else if let Some(stripped) = line.strip_prefix(prefix) {
            out.push_str(stripped);
        } else {
            out.push_str(line);
        }
    }
    out
}

fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    // Walk back to a char boundary so multibyte content stays intact.
    let mut end = end;
    while end > 0 && !a.is_char_boundary(end) {
        end -= 1;
    }
    &a[..end]
}

/// Greedy word-wrap that breaks on ASCII whitespace and preserves the
/// original line structure (existing newlines split paragraphs). Words
/// longer than `width` are emitted on their own line rather than split
/// mid-token.
fn word_wrap_str(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first_line = true;
    for line in text.split('\n') {
        if !first_line {
            out.push('\n');
        }
        first_line = false;
        let mut col = 0usize;
        let mut first_word = true;
        for word in line.split_whitespace() {
            let word_len = UnicodeSegmentation::graphemes(word, true).count();
            if first_word {
                out.push_str(word);
                col = word_len;
                first_word = false;
            } else if col + 1 + word_len > width {
                out.push('\n');
                out.push_str(word);
                col = word_len;
            } else {
                out.push(' ');
                out.push_str(word);
                col += 1 + word_len;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{dedent_str, word_wrap_str};

    #[test]
    fn dedent_strips_minimum_common_prefix() {
        let input = "    line one\n      line two\n    line three\n";
        let result = dedent_str(input);
        assert_eq!(result, "line one\n  line two\nline three\n");
    }

    #[test]
    fn dedent_ignores_blank_lines_when_computing_prefix() {
        let input = "  one\n\n  two\n";
        let result = dedent_str(input);
        assert_eq!(result, "one\n\ntwo\n");
    }

    #[test]
    fn dedent_with_no_common_prefix_is_passthrough() {
        let input = "  foo\nbar\n";
        let result = dedent_str(input);
        assert_eq!(result, "  foo\nbar\n");
    }

    #[test]
    fn word_wrap_breaks_at_word_boundary() {
        let result = word_wrap_str("the quick brown fox jumps over the lazy dog", 15);
        assert_eq!(result, "the quick brown\nfox jumps over\nthe lazy dog");
    }

    #[test]
    fn word_wrap_preserves_paragraphs() {
        let result = word_wrap_str("alpha beta\ngamma delta", 20);
        assert_eq!(result, "alpha beta\ngamma delta");
    }

    #[test]
    fn word_wrap_long_word_keeps_word_intact() {
        let result = word_wrap_str("aa supercalifragilistic bb", 5);
        assert_eq!(result, "aa\nsupercalifragilistic\nbb");
    }

    #[test]
    fn index_of_is_char_indexed_with_from_and_miss() {
        use crate::value::VmValue;
        let s = |v: &str| VmValue::String(std::sync::Arc::from(v));
        let call = |args: Vec<VmValue>| -> i64 {
            let mut out = String::new();
            match super::index_of_impl(&args, &mut out).unwrap() {
                VmValue::Int(n) => n,
                other => panic!("expected int, got {other:?}"),
            }
        };
        assert_eq!(call(vec![s("hello world"), s("o")]), 4);
        // `from` skips the first match and is a char offset, not a byte offset.
        assert_eq!(call(vec![s("hello world"), s("o"), VmValue::Int(5)]), 7);
        assert_eq!(call(vec![s("hello"), s("z")]), -1);
        // A 2-byte char before the needle still yields a char index (5), not a
        // byte index (6) — pairs with `substring`.
        assert_eq!(call(vec![s("café x"), s("x")]), 5);
    }
}
