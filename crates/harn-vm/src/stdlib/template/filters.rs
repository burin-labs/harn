//! Native filter evaluation over the shared prompt syntax contract.

pub use harn_prompt_syntax::filters::{lookup, Filter, FilterId, FILTERS};

use crate::value::{string_char_count, VmValue};

use super::error::TemplateError;
use super::render::{display_value, truthy};

pub(super) fn apply_filter(
    name: &str,
    v: &VmValue,
    args: &[VmValue],
    line: usize,
    col: usize,
) -> Result<VmValue, TemplateError> {
    let Some(filter) = lookup(name) else {
        return Err(TemplateError::new(
            line,
            col,
            format!("unknown filter `{name}`"),
        ));
    };
    if args.len() < filter.required || args.len() > filter.params.len() {
        return Err(TemplateError::new(
            line,
            col,
            format!("filter `{name}` got wrong number of arguments"),
        ));
    }
    let result = match filter.id {
        FilterId::Upper => Ok(str_value(display_value(v).to_uppercase())),
        FilterId::Lower => Ok(str_value(display_value(v).to_lowercase())),
        FilterId::Trim => Ok(str_value(display_value(v).trim().to_string())),
        FilterId::Capitalize => Ok(str_value(capitalize(&display_value(v)))),
        FilterId::Title => Ok(str_value(title_case(&display_value(v)))),
        FilterId::Length => length(v),
        FilterId::First => Ok(first(v)),
        FilterId::Last => Ok(last(v)),
        FilterId::Reverse => Ok(reverse(v)),
        FilterId::Join => join(v, &args[0]),
        FilterId::Default => Ok(if truthy(v) {
            v.clone()
        } else {
            args[0].clone()
        }),
        FilterId::Json => json(v, args.first().map(truthy).unwrap_or(false)),
        FilterId::Indent => indent(v, &args[0], args.get(1).map(truthy).unwrap_or(false)),
        FilterId::Lines => Ok(lines(v)),
        FilterId::EscapeMd => Ok(str_value(escape_md(&display_value(v)))),
        FilterId::Replace => Ok(str_value(
            display_value(v).replace(&display_value(&args[0]), &display_value(&args[1])),
        )),
    };
    result.map_err(|message| TemplateError::new(line, col, message))
}

fn str_value(s: String) -> VmValue {
    VmValue::string(s)
}

fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    if let Some(c) = chars.next() {
        out.extend(c.to_uppercase());
    }
    for c in chars {
        out.extend(c.to_lowercase());
    }
    out
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_start = true;
    for c in s.chars() {
        if c.is_whitespace() {
            at_start = true;
            out.push(c);
        } else if at_start {
            out.extend(c.to_uppercase());
            at_start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn length(v: &VmValue) -> Result<VmValue, String> {
    let n: i64 = match v {
        VmValue::String(s) => string_char_count(s) as i64,
        VmValue::List(items) => items.len() as i64,
        VmValue::Set(items) => items.len() as i64,
        VmValue::Dict(d) => d.len() as i64,
        VmValue::Range(r) => r.len(),
        VmValue::Nil => 0,
        other => return Err(format!("`length` not defined for {}", other.type_name())),
    };
    Ok(VmValue::Int(n))
}

fn first(v: &VmValue) -> VmValue {
    match v {
        VmValue::List(items) => items.first().cloned().unwrap_or(VmValue::Nil),
        VmValue::Set(set) => set.items().first().cloned().unwrap_or(VmValue::Nil),
        VmValue::String(s) => s
            .chars()
            .next()
            .map(|c| str_value(c.to_string()))
            .unwrap_or(VmValue::Nil),
        _ => VmValue::Nil,
    }
}

fn last(v: &VmValue) -> VmValue {
    match v {
        VmValue::List(items) => items.last().cloned().unwrap_or(VmValue::Nil),
        VmValue::Set(set) => set.items().last().cloned().unwrap_or(VmValue::Nil),
        VmValue::String(s) => s
            .chars()
            .last()
            .map(|c| str_value(c.to_string()))
            .unwrap_or(VmValue::Nil),
        _ => VmValue::Nil,
    }
}

fn reverse(v: &VmValue) -> VmValue {
    match v {
        VmValue::List(items) => {
            let mut out: Vec<VmValue> = items.as_ref().clone();
            out.reverse();
            VmValue::list(out)
        }
        VmValue::String(s) => str_value(s.chars().rev().collect::<String>()),
        _ => v.clone(),
    }
}

fn join(v: &VmValue, separator: &VmValue) -> Result<VmValue, String> {
    let sep = display_value(separator);
    let parts: Vec<String> = match v {
        VmValue::List(items) => items.iter().map(display_value).collect(),
        VmValue::Set(items) => items.iter().map(display_value).collect(),
        VmValue::String(s) => return Ok(VmValue::String(s.clone())),
        _ => return Err(format!("`join` requires a list (got {})", v.type_name())),
    };
    Ok(str_value(parts.join(&sep)))
}

fn json(v: &VmValue, pretty: bool) -> Result<VmValue, String> {
    let jv = crate::llm::helpers::vm_value_to_json(v);
    let s = if pretty {
        serde_json::to_string_pretty(&jv)
    } else {
        serde_json::to_string(&jv)
    }
    .map_err(|e| format!("json serialization: {e}"))?;
    Ok(str_value(s))
}

fn indent(v: &VmValue, width: &VmValue, indent_first: bool) -> Result<VmValue, String> {
    let VmValue::Int(n) = width else {
        return Err("`indent` requires an int width".to_string());
    };
    let n = (*n).max(0) as usize;
    let pad: String = " ".repeat(n);
    let s = display_value(v);
    let mut out = String::with_capacity(s.len() + n * 4);
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if !line.is_empty() && (i > 0 || indent_first) {
            out.push_str(&pad);
        }
        out.push_str(line);
    }
    Ok(str_value(out))
}

fn lines(v: &VmValue) -> VmValue {
    let s = display_value(v);
    let list: Vec<VmValue> = s.split('\n').map(|p| str_value(p.to_string())).collect();
    VmValue::list(list)
}

fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '<' | '>' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
