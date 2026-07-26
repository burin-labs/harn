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
    let bad_arity = || {
        TemplateError::new(
            line,
            col,
            format!("filter `{name}` got wrong number of arguments"),
        )
    };
    let need = |n: usize, args: &[VmValue]| -> Result<(), TemplateError> {
        if args.len() == n {
            Ok(())
        } else {
            Err(bad_arity())
        }
    };
    let str_of = |v: &VmValue| -> String { display_value(v) };
    match name {
        "upper" => {
            need(0, args)?;
            Ok(VmValue::String(arcstr::ArcStr::from(
                str_of(v).to_uppercase(),
            )))
        }
        "lower" => {
            need(0, args)?;
            Ok(VmValue::String(arcstr::ArcStr::from(
                str_of(v).to_lowercase(),
            )))
        }
        "trim" => {
            need(0, args)?;
            Ok(VmValue::String(arcstr::ArcStr::from(str_of(v).trim())))
        }
        "capitalize" => {
            need(0, args)?;
            let s = str_of(v);
            let mut out = String::with_capacity(s.len());
            let mut chars = s.chars();
            if let Some(c) = chars.next() {
                out.extend(c.to_uppercase());
            }
            for c in chars {
                out.extend(c.to_lowercase());
            }
            Ok(VmValue::String(arcstr::ArcStr::from(out)))
        }
        "title" => {
            need(0, args)?;
            let s = str_of(v);
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
            Ok(VmValue::String(arcstr::ArcStr::from(out)))
        }
        "length" => {
            need(0, args)?;
            let n: i64 = match v {
                VmValue::String(s) => string_char_count(s) as i64,
                VmValue::List(items) => items.len() as i64,
                VmValue::Set(items) => items.len() as i64,
                VmValue::Dict(d) => d.len() as i64,
                VmValue::Range(r) => r.len(),
                VmValue::Nil => 0,
                other => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        format!("`length` not defined for {}", other.type_name()),
                    ));
                }
            };
            Ok(VmValue::Int(n))
        }
        "first" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => items.first().cloned().unwrap_or(VmValue::Nil),
                VmValue::Set(set) => set.items().first().cloned().unwrap_or(VmValue::Nil),
                VmValue::String(s) => s
                    .chars()
                    .next()
                    .map(|c| VmValue::String(arcstr::ArcStr::from(c.to_string())))
                    .unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            })
        }
        "last" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => items.last().cloned().unwrap_or(VmValue::Nil),
                VmValue::Set(set) => set.items().last().cloned().unwrap_or(VmValue::Nil),
                VmValue::String(s) => s
                    .chars()
                    .last()
                    .map(|c| VmValue::String(arcstr::ArcStr::from(c.to_string())))
                    .unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            })
        }
        "reverse" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => {
                    let mut out: Vec<VmValue> = items.as_ref().clone();
                    out.reverse();
                    VmValue::List(std::sync::Arc::new(out))
                }
                VmValue::String(s) => {
                    VmValue::String(arcstr::ArcStr::from(s.chars().rev().collect::<String>()))
                }
                _ => v.clone(),
            })
        }
        "join" => {
            need(1, args)?;
            let sep = str_of(&args[0]);
            let parts: Vec<String> = match v {
                VmValue::List(items) => items.iter().map(str_of).collect(),
                VmValue::Set(items) => items.iter().map(str_of).collect(),
                VmValue::String(s) => return Ok(VmValue::String(s.clone())),
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        format!("`join` requires a list (got {})", v.type_name()),
                    ));
                }
            };
            Ok(VmValue::String(arcstr::ArcStr::from(parts.join(&sep))))
        }
        "default" => {
            need(1, args)?;
            if truthy(v) {
                Ok(v.clone())
            } else {
                Ok(args[0].clone())
            }
        }
        "json" => {
            if args.len() > 1 {
                return Err(bad_arity());
            }
            let pretty = args.first().map(truthy).unwrap_or(false);
            let jv = crate::llm::helpers::vm_value_to_json(v);
            let s = if pretty {
                serde_json::to_string_pretty(&jv)
            } else {
                serde_json::to_string(&jv)
            }
            .map_err(|e| TemplateError::new(line, col, format!("json serialization: {e}")))?;
            Ok(VmValue::String(arcstr::ArcStr::from(s)))
        }
        "indent" => {
            if args.is_empty() || args.len() > 2 {
                return Err(bad_arity());
            }
            let n = match &args[0] {
                VmValue::Int(n) => (*n).max(0) as usize,
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        "`indent` requires an integer width",
                    ));
                }
            };
            let indent_first = args.get(1).map(truthy).unwrap_or(false);
            let pad: String = " ".repeat(n);
            let s = str_of(v);
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
            Ok(VmValue::String(arcstr::ArcStr::from(out)))
        }
        "lines" => {
            need(0, args)?;
            let s = str_of(v);
            let list: Vec<VmValue> = s
                .split('\n')
                .map(|p| VmValue::String(arcstr::ArcStr::from(p)))
                .collect();
            Ok(VmValue::List(std::sync::Arc::new(list)))
        }
        "escape_md" => {
            need(0, args)?;
            let s = str_of(v);
            let mut out = String::with_capacity(s.len() + 8);
            for c in s.chars() {
                match c {
                    '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+'
                    | '-' | '.' | '!' | '|' | '<' | '>' => {
                        out.push('\\');
                        out.push(c);
                    }
                    _ => out.push(c),
                }
            }
            Ok(VmValue::String(arcstr::ArcStr::from(out)))
        }
        "replace" => {
            need(2, args)?;
            let s = str_of(v);
            let from = str_of(&args[0]);
            let to = str_of(&args[1]);
            Ok(VmValue::String(arcstr::ArcStr::from(s.replace(&from, &to))))
        }
        other => Err(TemplateError::new(
            line,
            col,
            format!("unknown filter `{other}`"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::vocabulary::FILTERS;

    /// The dispatch above and the declared filter vocabulary must name the same
    /// set. Editors highlight from the declaration, so a filter implemented
    /// here but never declared would go un-highlighted, and a declared filter
    /// with no arm would highlight as valid while failing at render time.
    ///
    /// Reading this file's own source is what makes the check exact: the arms
    /// are data the compiler otherwise erases, and enumerating them any other
    /// way would just be a third copy of the list.
    #[test]
    fn declared_filters_match_the_dispatch_arms() {
        let source = include_str!("filters.rs");
        let implemented: Vec<&str> = source
            .lines()
            .filter_map(|line| line.strip_prefix("        \"")?.strip_suffix("\" => {"))
            .collect();

        assert!(
            !implemented.is_empty(),
            "no filter arms found — the arm layout changed, so this guard is no \
             longer checking anything. Update the scan in this test."
        );

        let implemented_set: std::collections::BTreeSet<&str> =
            implemented.iter().copied().collect();
        let declared_set: std::collections::BTreeSet<&str> = FILTERS.iter().copied().collect();

        assert_eq!(
            implemented_set, declared_set,
            "filter dispatch and `vocabulary::FILTERS` disagree.\n\
             Add the filter to both, then run `make gen-prompt-grammar`."
        );
    }
}
