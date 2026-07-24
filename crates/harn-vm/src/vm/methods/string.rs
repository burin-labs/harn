use crate::value::{string_char_count, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_string_method(
        s: &arcstr::ArcStr,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "count" => Ok(VmValue::Int(string_char_count(s) as i64)),
            "empty" => Ok(VmValue::Bool(s.is_empty())),
            "contains" | "includes" => Ok(VmValue::Bool(
                s.contains(&*args.first().map(|a| a.as_str_cow()).unwrap_or_default()),
            )),
            "replace" if args.len() >= 2 => Ok(VmValue::String(arcstr::ArcStr::from(
                s.replace(&*args[0].as_str_cow(), &args[1].as_str_cow()),
            ))),
            "split" => {
                let sep = args
                    .first()
                    .map(|a| a.as_str_cow())
                    .unwrap_or(std::borrow::Cow::Borrowed(","));
                Ok(VmValue::List(std::sync::Arc::new(
                    s.split(&*sep)
                        .map(|p| VmValue::String(arcstr::ArcStr::from(p)))
                        .collect(),
                )))
            }
            "trim" => Ok(VmValue::String(arcstr::ArcStr::from(s.trim()))),
            "starts_with" => Ok(VmValue::Bool(
                s.starts_with(&*args.first().map(|a| a.as_str_cow()).unwrap_or_default()),
            )),
            "ends_with" => Ok(VmValue::Bool(
                s.ends_with(&*args.first().map(|a| a.as_str_cow()).unwrap_or_default()),
            )),
            "lowercase" => Ok(VmValue::String(arcstr::ArcStr::from(s.to_lowercase()))),
            "uppercase" => Ok(VmValue::String(arcstr::ArcStr::from(s.to_uppercase()))),
            "substring" => Ok(VmValue::String(arcstr::ArcStr::from(
                crate::stdlib::strings::char_substring(
                    s,
                    args.first().and_then(|a| a.as_int()).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_int()),
                ),
            ))),
            // `slice` is a list method, but harness authors (and JS/Python
            // muscle memory) routinely call it on strings. Aliasing it to a
            // char-based, negative-index-aware substring — mirroring
            // `list.slice` semantics exactly — structurally removes the entire
            // "string has no method `slice`" crash class instead of chasing
            // each call site. End index defaults to the char length.
            "slice" => {
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let start_raw = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let start = if start_raw < 0 {
                    (len + start_raw).max(0) as usize
                } else {
                    start_raw.min(len) as usize
                };
                let end = if args.len() > 1 {
                    let end_raw = args[1].as_int().unwrap_or(len);
                    if end_raw < 0 {
                        (len + end_raw).max(0) as usize
                    } else {
                        end_raw.min(len) as usize
                    }
                } else {
                    len as usize
                };
                let end = end.max(start);
                Ok(VmValue::String(arcstr::ArcStr::from(
                    chars[start..end].iter().collect::<String>(),
                )))
            }
            "index_of" => {
                let needle = args.first().map(|a| a.as_str_cow()).unwrap_or_default();
                let idx = s
                    .find(&*needle)
                    .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                Ok(VmValue::Int(idx.unwrap_or(-1)))
            }
            "chars" => Ok(VmValue::chars_list(s)),
            "repeat" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(1);
                let repeated = crate::limits::checked_repeat(s, n.max(0) as usize)?;
                Ok(VmValue::String(arcstr::ArcStr::from(repeated)))
            }
            "reversed" | "reverse" => Ok(VmValue::String(arcstr::ArcStr::from(
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
                let current_len = string_char_count(s);
                if current_len >= width {
                    Ok(VmValue::String(s.clone()))
                } else {
                    // Cap script-controlled pad widths so a huge `width` errors
                    // cleanly instead of allocating gigabytes.
                    let padding = crate::limits::checked_repeat(
                        pad_char.encode_utf8(&mut [0u8; 4]),
                        width - current_len,
                    )?;
                    if left {
                        Ok(VmValue::String(arcstr::ArcStr::from(format!(
                            "{padding}{s}"
                        ))))
                    } else {
                        Ok(VmValue::String(arcstr::ArcStr::from(format!(
                            "{s}{padding}"
                        ))))
                    }
                }
            }
            "trim_start" => Ok(VmValue::String(arcstr::ArcStr::from(s.trim_start()))),
            "trim_end" => Ok(VmValue::String(arcstr::ArcStr::from(s.trim_end()))),
            "lines" => Ok(VmValue::List(std::sync::Arc::new(
                s.lines()
                    .map(|l| VmValue::String(arcstr::ArcStr::from(l)))
                    .collect(),
            ))),
            "char_at" => {
                let idx = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let Ok(idx) = usize::try_from(idx) else {
                    return Ok(VmValue::Nil);
                };
                Ok(s.chars()
                    .nth(idx)
                    .map(VmValue::char_value)
                    .unwrap_or(VmValue::Nil))
            }
            "last_index_of" | "rfind" => {
                let needle = args.first().map(|a| a.as_str_cow()).unwrap_or_default();
                let idx = s
                    .rfind(&*needle)
                    .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                Ok(VmValue::Int(idx.unwrap_or(-1)))
            }
            "lower" | "to_lower" => Ok(VmValue::String(arcstr::ArcStr::from(s.to_lowercase()))),
            "upper" | "to_upper" => Ok(VmValue::String(arcstr::ArcStr::from(s.to_uppercase()))),
            "len" => Ok(VmValue::Int(string_char_count(s) as i64)),
            _ => Err(VmError::Runtime(format!("string has no method `{method}`"))),
        }
    }
}
