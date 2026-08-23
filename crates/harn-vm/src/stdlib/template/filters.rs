//! The prompt-template filter registry.
//!
//! [`FILTERS`] is the only list of filters that exists. `apply_filter`
//! dispatches through it and tooling (editor completion, hover) reads
//! it, so a filter cannot be offered to an author and then rejected by
//! the engine, and a filter cannot be implemented without becoming
//! discoverable.
//!
//! Arity checking and error positioning happen here, once, so each
//! filter body only has to say what went wrong.

use crate::value::{string_char_count, VmValue};

use super::error::TemplateError;
use super::render::{display_value, truthy};

/// A filter body. Arity has already been checked against the filter's
/// declared parameters; the returned message is positioned by the
/// caller.
type FilterBody = fn(&VmValue, &[VmValue]) -> Result<VmValue, String>;

/// One filter's contract: what authors write after `|`, which arguments
/// follow the `:`, and what it does.
pub struct Filter {
    /// The name written after `|`.
    pub name: &'static str,
    /// Names of the arguments that follow `:`, in order. Anything past
    /// `required` is optional.
    pub params: &'static [&'static str],
    /// How many of `params` must be supplied.
    pub required: usize,
    /// One line describing what the filter does.
    pub summary: &'static str,
    body: FilterBody,
}

impl Filter {
    /// How the filter is written, e.g. `join: separator` or
    /// `indent: width[, indent_first]`.
    pub fn signature(&self) -> String {
        if self.params.is_empty() {
            return self.name.to_string();
        }
        let mut out = format!("{}: ", self.name);
        for (index, param) in self.params.iter().enumerate() {
            // The bracket opens before the separator, so what is left
            // when the optional arguments are dropped still reads as a
            // valid call.
            if index == self.required {
                out.push('[');
            }
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(param);
        }
        if self.required < self.params.len() {
            out.push(']');
        }
        out
    }
}

/// Every filter the engine can apply.
pub static FILTERS: &[Filter] = &[
    Filter {
        name: "upper",
        params: &[],
        required: 0,
        summary: "Uppercase the value.",
        body: |v, _| Ok(str_value(display_value(v).to_uppercase())),
    },
    Filter {
        name: "lower",
        params: &[],
        required: 0,
        summary: "Lowercase the value.",
        body: |v, _| Ok(str_value(display_value(v).to_lowercase())),
    },
    Filter {
        name: "trim",
        params: &[],
        required: 0,
        summary: "Strip leading and trailing whitespace.",
        body: |v, _| Ok(str_value(display_value(v).trim().to_string())),
    },
    Filter {
        name: "capitalize",
        params: &[],
        required: 0,
        summary: "Uppercase the first character and lowercase the rest.",
        body: |v, _| Ok(str_value(capitalize(&display_value(v)))),
    },
    Filter {
        name: "title",
        params: &[],
        required: 0,
        summary: "Uppercase the first character of every word.",
        body: |v, _| Ok(str_value(title_case(&display_value(v)))),
    },
    Filter {
        name: "length",
        params: &[],
        required: 0,
        summary: "Number of characters, items, or entries.",
        body: |v, _| length(v),
    },
    Filter {
        name: "first",
        params: &[],
        required: 0,
        summary: "First item of a list or set, or first character of a string.",
        body: |v, _| Ok(first(v)),
    },
    Filter {
        name: "last",
        params: &[],
        required: 0,
        summary: "Last item of a list or set, or last character of a string.",
        body: |v, _| Ok(last(v)),
    },
    Filter {
        name: "reverse",
        params: &[],
        required: 0,
        summary: "Reverse a list or string. Other values pass through unchanged.",
        body: |v, _| Ok(reverse(v)),
    },
    Filter {
        name: "join",
        params: &["separator"],
        required: 1,
        summary: "Join a list or set into a string with the given separator.",
        body: |v, args| join(v, &args[0]),
    },
    Filter {
        name: "default",
        params: &["fallback"],
        required: 1,
        summary: "Substitute the fallback when the value is falsy.",
        body: |v, args| {
            Ok(if truthy(v) {
                v.clone()
            } else {
                args[0].clone()
            })
        },
    },
    Filter {
        name: "json",
        params: &["pretty"],
        required: 0,
        summary: "Serialize the value as JSON, optionally pretty-printed.",
        body: |v, args| json(v, args.first().map(truthy).unwrap_or(false)),
    },
    Filter {
        name: "indent",
        params: &["width", "indent_first"],
        required: 1,
        summary: "Indent every line by `width` spaces, skipping the first line unless \
                  `indent_first` is true.",
        body: |v, args| indent(v, &args[0], args.get(1).map(truthy).unwrap_or(false)),
    },
    Filter {
        name: "lines",
        params: &[],
        required: 0,
        summary: "Split the value into a list of lines.",
        body: |v, _| Ok(lines(v)),
    },
    Filter {
        name: "escape_md",
        params: &[],
        required: 0,
        summary: "Backslash-escape Markdown punctuation.",
        body: |v, _| Ok(str_value(escape_md(&display_value(v)))),
    },
    Filter {
        name: "replace",
        params: &["from", "to"],
        required: 2,
        summary: "Replace every occurrence of `from` with `to`.",
        body: |v, args| {
            let s = display_value(v);
            let from = display_value(&args[0]);
            let to = display_value(&args[1]);
            Ok(str_value(s.replace(&from, &to)))
        },
    },
];

/// The filter named `name`, if the engine has one.
pub fn lookup(name: &str) -> Option<&'static Filter> {
    FILTERS.iter().find(|filter| filter.name == name)
}

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
    (filter.body)(v, args).map_err(|message| TemplateError::new(line, col, message))
}

fn str_value(s: String) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
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
            VmValue::List(std::sync::Arc::new(out))
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
    VmValue::List(std::sync::Arc::new(list))
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

#[cfg(test)]
mod tests {
    use super::{apply_filter, lookup, FILTERS};
    use crate::value::VmValue;

    #[test]
    fn filter_names_are_unique() {
        let mut names: Vec<&str> = FILTERS.iter().map(|filter| filter.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate filter name in the table");
    }

    #[test]
    fn required_arguments_are_a_prefix_of_the_parameter_list() {
        for filter in FILTERS {
            assert!(
                filter.required <= filter.params.len(),
                "`{}` requires more arguments than it declares",
                filter.name
            );
        }
    }

    #[test]
    fn signatures_mark_optional_arguments() {
        assert_eq!(lookup("upper").unwrap().signature(), "upper");
        assert_eq!(lookup("join").unwrap().signature(), "join: separator");
        assert_eq!(lookup("replace").unwrap().signature(), "replace: from, to");
        assert_eq!(lookup("json").unwrap().signature(), "json: [pretty]");
        assert_eq!(
            lookup("indent").unwrap().signature(),
            "indent: width[, indent_first]"
        );
    }

    #[test]
    fn unknown_names_are_not_filters() {
        assert!(lookup("endif").is_none());
        assert!(lookup("").is_none());
    }

    /// `apply_filter` dispatches through the table rather than a parallel
    /// `match`, so "declared" and "implemented" are the same set by
    /// construction. This pins that: every declared filter is reachable,
    /// and a name that is not declared is rejected as unknown.
    #[test]
    fn dispatch_covers_exactly_the_declared_filters() {
        for filter in FILTERS {
            let args = vec![VmValue::Nil; filter.required];
            // A filter may still reject `nil` on its merits; what must
            // never happen is the engine not knowing the name at all.
            if let Err(error) = apply_filter(filter.name, &VmValue::Nil, &args, 1, 1) {
                assert!(
                    !error.kind.contains("unknown filter"),
                    "`{}` is declared but not dispatched",
                    filter.name
                );
            }
        }

        let error = apply_filter("uppercase", &VmValue::Nil, &[], 1, 1)
            .expect_err("an undeclared name is not a filter");
        assert!(error.kind.contains("unknown filter"), "got {}", error.kind);
    }
}
