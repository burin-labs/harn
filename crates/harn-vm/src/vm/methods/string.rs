use std::sync::Arc;

use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_string_method(
        s: &Arc<str>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "count" => Ok(VmValue::Int(s.chars().count() as i64)),
            "empty" => Ok(VmValue::Bool(s.is_empty())),
            "contains" => Ok(VmValue::Bool(
                s.contains(&*args.first().map(|a| a.display()).unwrap_or_default()),
            )),
            "replace" if args.len() >= 2 => Ok(VmValue::String(std::sync::Arc::from(
                s.replace(&args[0].display(), &args[1].display()),
            ))),
            "split" => {
                let sep = args.first().map(|a| a.display()).unwrap_or(",".into());
                Ok(VmValue::List(std::sync::Arc::new(
                    s.split(&*sep)
                        .map(|p| VmValue::String(std::sync::Arc::from(p)))
                        .collect(),
                )))
            }
            "trim" => Ok(VmValue::String(std::sync::Arc::from(s.trim()))),
            "starts_with" => Ok(VmValue::Bool(
                s.starts_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
            )),
            "ends_with" => Ok(VmValue::Bool(
                s.ends_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
            )),
            "lowercase" => Ok(VmValue::String(std::sync::Arc::from(s.to_lowercase()))),
            "uppercase" => Ok(VmValue::String(std::sync::Arc::from(s.to_uppercase()))),
            "substring" => {
                let len = s.chars().count() as i64;
                let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(len);
                let start = start.clamp(0, len) as usize;
                let end = end.clamp(0, len) as usize;
                let end = end.max(start);
                let substr: String = s.chars().skip(start).take(end - start).collect();
                Ok(VmValue::String(std::sync::Arc::from(substr)))
            }
            "index_of" => {
                let needle = args.first().map(|a| a.display()).unwrap_or_default();
                let idx = s
                    .find(&needle)
                    .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                Ok(VmValue::Int(idx.unwrap_or(-1)))
            }
            "chars" => Ok(VmValue::List(std::sync::Arc::new(
                s.chars()
                    .map(|c| VmValue::String(std::sync::Arc::from(c.to_string())))
                    .collect(),
            ))),
            "repeat" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(1);
                Ok(VmValue::String(std::sync::Arc::from(
                    s.repeat(n.max(0) as usize),
                )))
            }
            "reverse" => Ok(VmValue::String(std::sync::Arc::from(
                s.chars().rev().collect::<String>(),
            ))),
            "pad_left" | "pad_right" => {
                let left = method == "pad_left";
                let width = args.first().and_then(|a| a.as_int()).unwrap_or(0) as usize;
                let pad_char = args
                    .get(1)
                    .map(|a| a.display())
                    .and_then(|s| s.chars().next())
                    .unwrap_or(' ');
                let current_len = s.chars().count();
                if current_len >= width {
                    Ok(VmValue::String(Arc::clone(s)))
                } else {
                    let padding: String =
                        std::iter::repeat_n(pad_char, width - current_len).collect();
                    if left {
                        Ok(VmValue::String(std::sync::Arc::from(format!(
                            "{padding}{s}"
                        ))))
                    } else {
                        Ok(VmValue::String(std::sync::Arc::from(format!(
                            "{s}{padding}"
                        ))))
                    }
                }
            }
            "trim_start" => Ok(VmValue::String(std::sync::Arc::from(s.trim_start()))),
            "trim_end" => Ok(VmValue::String(std::sync::Arc::from(s.trim_end()))),
            "lines" => Ok(VmValue::List(std::sync::Arc::new(
                s.lines()
                    .map(|l| VmValue::String(std::sync::Arc::from(l)))
                    .collect(),
            ))),
            "char_at" => {
                let idx = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let chars: Vec<char> = s.chars().collect();
                if idx >= 0 && (idx as usize) < chars.len() {
                    Ok(VmValue::String(std::sync::Arc::from(
                        chars[idx as usize].to_string(),
                    )))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "last_index_of" => {
                let needle = args.first().map(|a| a.display()).unwrap_or_default();
                let idx = s
                    .rfind(&needle)
                    .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                Ok(VmValue::Int(idx.unwrap_or(-1)))
            }
            "lower" | "to_lower" => Ok(VmValue::String(std::sync::Arc::from(s.to_lowercase()))),
            "upper" | "to_upper" => Ok(VmValue::String(std::sync::Arc::from(s.to_uppercase()))),
            "len" => Ok(VmValue::Int(s.chars().count() as i64)),
            _ => Err(VmError::Runtime(format!("string has no method `{method}`"))),
        }
    }
}
