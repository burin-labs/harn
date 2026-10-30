use crate::stdlib::macros::harn_builtin;
use crate::text::case::{
    lowercase_first, split_camel, split_kebab, split_snake, uppercase_first, words_to_camel,
    words_to_kebab, words_to_pascal, words_to_snake,
};
use crate::value::{values_equal, VmError, VmValue};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::stdlib::template::{render_asset_result, render_template_result, TemplateAsset};

fn render_template_string(args: &[VmValue]) -> Result<VmValue, VmError> {
    let template = args.first().map(|a| a.display()).unwrap_or_default();
    let bindings = args.get(1).and_then(|a| a.as_dict());
    let rendered =
        render_template_result(&template, bindings, None, None).map_err(VmError::from)?;
    Ok(VmValue::string(rendered))
}

fn render_asset(args: &[VmValue]) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let asset = resolve_render_target(&path)?;
    let bindings = args.get(1).and_then(|a| a.as_dict());
    let rendered = render_asset_result(&asset, bindings).map_err(VmError::from)?;
    Ok(VmValue::string(rendered))
}

/// Resolve a `render(...)` / `render_prompt(...)` target, honoring the
/// `@/` and `@<alias>/` package-root forms (issue #742) and falling back
/// to the legacy source-relative path resolver for plain strings.
fn resolve_render_target(path: &str) -> Result<TemplateAsset, VmError> {
    TemplateAsset::render_target(path).map_err(|msg| VmError::Thrown(VmValue::string(msg)))
}

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_string_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "format(template: string, ...rest: any) -> string",
    category = "strings"
)]
#[expect(
    clippy::string_slice,
    reason = "offsets are find() results for 1-byte ASCII braces"
)]
fn format_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let template = args.first().map(|a| a.display()).unwrap_or_default();

    // Dict → named placeholders `{key}`. Single-pass scan avoids double-substitution.
    if let Some(dict) = args.get(1).and_then(|a| a.as_dict()) {
        let mut result = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            // A miss keeps the `{` literal and rescans after it, so a placeholder
            // nested in literal braces (`{"a": {x}}`) still substitutes.
            let after_open = &rest[open + 1..];
            match after_open
                .find('}')
                .map(|close| (close, dict.get(&after_open[..close])))
            {
                Some((close, Some(val))) => {
                    result.push_str(&val.display());
                    rest = &after_open[close + 1..];
                }
                _ => {
                    result.push('{');
                    rest = after_open;
                }
            }
        }
        result.push_str(rest);
        return Ok(VmValue::string(result));
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
    Ok(VmValue::string(result))
}

#[harn_builtin(
    exposure = "pure",
    sig_expr = harn_builtin_meta::signatures::PORTABLE_TRIM,
    category = "strings"
)]
fn trim_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(harn_kernel::pure::trim_text(&s)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "lowercase(text: string?) -> string",
    category = "strings"
)]
fn lowercase_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(s.to_lowercase()))
}

#[harn_builtin(
    exposure = "pure",
    sig = "uppercase(text: string?) -> string",
    category = "strings"
)]
fn uppercase_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(s.to_uppercase()))
}

use crate::value::{char_substring, split_separator_required};

#[harn_builtin(
    exposure = "pure",
    sig = "split(text: string?, separator: string) -> list",
    category = "strings"
)]
fn split_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let Some(sep) = args.get(1).map(|a| a.display()) else {
        return Err(split_separator_required());
    };
    let parts: Vec<VmValue> = s.split(&sep).map(VmValue::string).collect();
    Ok(VmValue::list(parts))
}

#[harn_builtin(
    exposure = "pure",
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
    Ok(VmValue::string(normalized))
}

#[harn_builtin(
    exposure = "pure",
    sig = "unicode_graphemes(text: string?) -> list",
    category = "strings"
)]
fn unicode_graphemes_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::strings(UnicodeSegmentation::graphemes(
        s.as_str(),
        true,
    )))
}

#[harn_builtin(
    exposure = "pure",
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
        return Ok(VmValue::string(s));
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
    Ok(VmValue::string(format!(
        "{}{}{}",
        harn_vm_core::limits::checked_repeat(fill, left)?,
        s,
        harn_vm_core::limits::checked_repeat(fill, right)?
    )))
}

#[harn_builtin(
    exposure = "pure",
    sig_expr = harn_builtin_meta::signatures::PORTABLE_STARTS_WITH,
    category = "strings"
)]
fn starts_with_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let prefix = args.get(1).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::Bool(harn_kernel::pure::starts_with_text(
        &s, &prefix,
    )))
}

#[harn_builtin(
    exposure = "pure",
    sig = "ends_with(text: string?, suffix: string) -> bool",
    category = "strings"
)]
fn ends_with_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let suffix = args.get(1).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::Bool(harn_kernel::pure::ends_with_text(
        &s, &suffix,
    )))
}

#[harn_builtin(
    exposure = "pure",
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
    exposure = "pure",
    sig = "index_of(haystack: string?, needle: string, from?: int) -> int",
    category = "strings"
)]
#[expect(
    clippy::string_slice,
    reason = "char_to_byte_offset returns a clamped char-boundary offset"
)]
fn index_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // The missing sibling of `starts_with`/`ends_with`/`contains`. Char-indexed
    // to pair with `substring`/`slice`: `from` is a char offset and the result
    // is a char index (or -1 when absent).
    let haystack = args.first().map(|a| a.display()).unwrap_or_default();
    let needle = args.get(1).map(|a| a.display()).unwrap_or_default();
    let from = args.get(2).and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
    let byte_from = crate::value::char_to_byte_offset(&haystack, from);
    match haystack[byte_from..].find(&needle) {
        Some(rel) => Ok(VmValue::Int(crate::value::byte_offset_to_char_index(
            &haystack,
            byte_from + rel,
        ) as i64)),
        None => Ok(VmValue::Int(-1)),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig_expr = harn_builtin_meta::signatures::PORTABLE_REPLACE,
    category = "strings"
)]
fn replace_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let old = args.get(1).map(|a| a.display()).unwrap_or_default();
    let new = args.get(2).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(harn_kernel::pure::replace_text(
        &s, &old, &new,
    )))
}

#[harn_builtin(
    exposure = "pure",
    sig = "join(items: list, separator?: string) -> string",
    category = "strings"
)]
fn join_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let sep = args.get(1).map(|a| a.display()).unwrap_or_default();
    match args.first() {
        Some(VmValue::List(items)) => {
            let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
            Ok(VmValue::string(parts.join(&sep)))
        }
        _ => Ok(VmValue::string("")),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "substring(text: string?, start: int, end?: int) -> string",
    category = "strings"
)]
fn substring_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let start = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
    let end = args.get(2).and_then(|a| a.as_int());
    Ok(VmValue::string(char_substring(&s, start, end)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "chars(text: string?) -> list",
    category = "strings"
)]
fn chars_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first() {
        // Borrow the backing `Arc<str>` directly for the common string case so
        // a full-file scan does not copy the source before materializing it.
        Some(VmValue::String(text)) => Ok(VmValue::chars_list(text)),
        Some(other) => Ok(VmValue::chars_list(&other.display())),
        None => Ok(VmValue::list(Vec::new())),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "snake_to_camel(text: string?) -> string",
    category = "strings"
)]
fn snake_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_camel(&split_snake(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "snake_to_pascal(text: string?) -> string",
    category = "strings"
)]
fn snake_to_pascal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_pascal(&split_snake(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "camel_to_snake(text: string?) -> string",
    category = "strings"
)]
fn camel_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_snake(&split_camel(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "pascal_to_snake(text: string?) -> string",
    category = "strings"
)]
fn pascal_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_snake(&split_camel(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "kebab_to_camel(text: string?) -> string",
    category = "strings"
)]
fn kebab_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_camel(&split_kebab(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "camel_to_kebab(text: string?) -> string",
    category = "strings"
)]
fn camel_to_kebab_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_kebab(&split_camel(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "snake_to_kebab(text: string?) -> string",
    category = "strings"
)]
fn snake_to_kebab_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_kebab(&split_snake(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "kebab_to_snake(text: string?) -> string",
    category = "strings"
)]
fn kebab_to_snake_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_snake(&split_kebab(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "pascal_to_camel(text: string?) -> string",
    category = "strings"
)]
fn pascal_to_camel_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(words_to_camel(&split_camel(&s))))
}

#[harn_builtin(
    exposure = "pure",
    sig = "camel_to_pascal(text: string?) -> string",
    category = "strings"
)]
fn camel_to_pascal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(uppercase_first(&s)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "title_case(text: string?) -> string",
    category = "strings"
)]
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
    Ok(VmValue::string(out))
}

#[harn_builtin(
    exposure = "pure",
    sig = "uppercase_first(text: string?) -> string",
    category = "strings"
)]
fn uppercase_first_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(uppercase_first(&s)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "lowercase_first(text: string?) -> string",
    category = "strings"
)]
fn lowercase_first_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(lowercase_first(&s)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "dirname(path: string?) -> string",
    category = "strings"
)]
fn dirname_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.parent() {
        Some(parent) => Ok(VmValue::string(parent.to_string_lossy().as_ref())),
        None => Ok(VmValue::string("")),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "basename(path: string?) -> string",
    category = "strings"
)]
fn basename_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.file_name() {
        Some(name) => Ok(VmValue::string(name.to_string_lossy().as_ref())),
        None => Ok(VmValue::string("")),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "extname(path: string?) -> string",
    category = "strings"
)]
fn extname_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let p = std::path::Path::new(&path);
    match p.extension() {
        Some(ext) => Ok(VmValue::string(format!(".{}", ext.to_string_lossy()))),
        None => Ok(VmValue::string("")),
    }
}

#[harn_builtin(
    exposure = "runtime_internal",
    aliases = ["render_prompt"],
    sig = "render(path: string?, bindings?: dict) -> string",
    category = "strings"
)]
fn render_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    render_asset(args)
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "render_string(template: string, bindings?: dict) -> string",
    category = "strings"
)]
fn render_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    render_template_string(args)
}

// Internal LLM render-context plumbing — paired push/pop hooks used
// by the agent_loop / handler-stack stdlib so user-authored
// `.harn.prompt` partials can branch on `llm.provider`,
// `llm.capabilities.native_tools`, etc. while rendering inside an
// LLM-aware frame. Userspace never calls these directly; the stdlib
// wraps every push in a matching `defer { __pop_llm_render_context() }`.
#[harn_builtin(
    exposure = "harness.agent.push_llm_render_context",
    // Ambient thread-local render frame for `.harn.prompt` branching — not a
    // durable model-facing state effect. Claiming state.mutate rejected the
    // push under agent-loop execution policy and aborted turns mid-flight.
    effects = [],
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

#[harn_builtin(
    exposure = "harness.agent.pop_llm_render_context",
    // Pair with `push_llm_render_context`: ambient stack pop, not durable state.
    effects = [],
    sig = "__pop_llm_render_context() -> nil",
    category = "strings"
)]
fn pop_llm_render_context_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::stdlib::template::pop_llm_render_context();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "pure",
    sig = "repeat(text: string?, count: int) -> string",
    category = "strings"
)]
fn repeat_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = args.first().map(|a| a.display()).unwrap_or_default();
    let count = args.get(1).and_then(VmValue::as_int).unwrap_or(0);
    if count <= 0 {
        return Ok(VmValue::string(""));
    }
    // Reject pathological sizes so a typo doesn't try to allocate
    // multi-gigabyte strings (or panic `capacity overflow`) inside a script.
    let repeated = harn_vm_core::limits::checked_repeat(&s, count as usize)?;
    Ok(VmValue::string(repeated))
}

#[harn_builtin(
    exposure = "pure",
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
        return Ok(VmValue::string(text));
    }
    let mut out = String::with_capacity(text.len() + prefix.len() * 4);
    for line in text.split_inclusive('\n') {
        // Don't pad blank lines — matches Python's textwrap.indent
        // default behavior so callers don't get trailing whitespace
        // on empty lines.
        if !line.trim().is_empty() {
            out.push_str(&prefix);
        }
        out.push_str(line);
    }
    Ok(VmValue::string(out))
}

#[harn_builtin(
    exposure = "pure",
    sig = "dedent(text: string?) -> string",
    category = "strings"
)]
fn dedent_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::string(dedent_str(&text)))
}

#[harn_builtin(
    exposure = "pure",
    sig = "word_wrap(text: string?, width?: int) -> string",
    category = "strings"
)]
fn word_wrap_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let width = args.get(1).and_then(VmValue::as_int).unwrap_or(80).max(1) as usize;
    Ok(VmValue::string(word_wrap_str(&text, width)))
}

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
        #[expect(
            clippy::string_slice,
            reason = "leading_len is the byte length of the whole-char whitespace prefix"
        )]
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

#[expect(clippy::string_slice, reason = "end comes from floor_char_boundary")]
fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    // Walk back to a char boundary so multibyte content stays intact.
    let end = a.floor_char_boundary(end);
    &a[..end]
}

/// Greedy word-wrap that breaks on ASCII whitespace and preserves the
/// original line structure (existing newlines split paragraphs). Words
/// longer than `width` are emitted on their own line rather than split
/// mid-token. `width` is a terminal-column budget, so a word's cost is its
/// display width — CJK and emoji are two columns each, combining marks zero —
/// not its grapheme count.
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
            let word_len = UnicodeWidthStr::width(word);
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
    fn dedent_preserves_line_structure() {
        for (name, input, expected) in [
            (
                "minimum common prefix",
                "    line one\n      line two\n    line three\n",
                "line one\n  line two\nline three\n",
            ),
            ("blank lines", "  one\n\n  two\n", "one\n\ntwo\n"),
            ("no common prefix", "  foo\nbar\n", "  foo\nbar\n"),
        ] {
            assert_eq!(dedent_str(input), expected, "{name}");
        }
    }

    #[test]
    fn word_wrap_long_word_keeps_word_intact() {
        let result = word_wrap_str("aa supercalifragilistic bb", 5);
        assert_eq!(result, "aa\nsupercalifragilistic\nbb");
    }

    #[test]
    fn word_wrap_combining_marks_do_not_add_width() {
        // "e" + U+0301 (combining acute) renders in one column, so "café"
        // spelled with a combining mark is four columns wide and fits a
        // five-column line beside nothing — but two of them do not.
        let cafe = "cafe\u{0301}";
        let result = word_wrap_str(&format!("{cafe} {cafe}"), 8);
        assert_eq!(result, format!("{cafe}\n{cafe}"));
    }

    #[test]
    fn index_of_is_char_indexed_with_from_and_miss() {
        use crate::value::VmValue;
        let s = |v: &str| VmValue::string(v);
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

    #[test]
    fn substring_builtin_uses_start_end_semantics() {
        use crate::value::VmValue;
        let s = |v: &str| VmValue::string(v);
        let call = |args: Vec<VmValue>| -> String {
            let mut out = String::new();
            match super::substring_impl(&args, &mut out).unwrap() {
                VmValue::String(text) => text.to_string(),
                other => panic!("expected string, got {other:?}"),
            }
        };
        // Second arg is an END index, not a length — matches `.substring`,
        // `s[a:b]`, `list.slice`, and `bytes_slice`.
        assert_eq!(
            call(vec![s("hello world"), VmValue::Int(0), VmValue::Int(5)]),
            "hello"
        );
        assert_eq!(
            call(vec![s("hello world"), VmValue::Int(6), VmValue::Int(9)]),
            "wor"
        );
        // Omitted end runs to the end of the string.
        assert_eq!(call(vec![s("hello world"), VmValue::Int(6)]), "world");
        // end < start (after clamping) yields the empty string; negatives clamp to 0.
        assert_eq!(
            call(vec![s("hello world"), VmValue::Int(6), VmValue::Int(3)]),
            ""
        );
        assert_eq!(
            call(vec![s("hello world"), VmValue::Int(2), VmValue::Int(-3)]),
            ""
        );
        assert_eq!(
            call(vec![s("hello world"), VmValue::Int(-3), VmValue::Int(5)]),
            "hello"
        );
        // Char-indexed, not byte-indexed: the 2-byte 'é' counts as one char.
        assert_eq!(
            call(vec![s("café latte"), VmValue::Int(0), VmValue::Int(4)]),
            "café"
        );
    }
}
