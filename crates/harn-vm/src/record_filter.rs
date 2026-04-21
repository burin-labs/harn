use std::rc::Rc;

use serde_json::Value as JsonValue;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmClosure, VmError};
use crate::vm::Vm;
use crate::Chunk;

const FILTER_FN_NAME: &str = "__harn_record_filter";

pub struct CompiledRecordFilter {
    vm: Vm,
    chunk: Chunk,
    closure: Option<Rc<VmClosure>>,
    normalized_expr: String,
}

impl CompiledRecordFilter {
    pub fn compile(expr: &str) -> Result<Self, String> {
        let normalized_expr = normalize_record_filter_expression(expr)?;
        let source = format!(
            r#"
fn {FILTER_FN_NAME}(record) {{
  let event = record.event
  let binding = record.binding
  let attempt = record.attempt
  let outcome = record.outcome
  let audit = record.audit
  return ({normalized_expr})
}}
"#
        );
        Ok(Self {
            vm: Vm::new(),
            chunk: crate::compile_source(&source)?,
            closure: None,
            normalized_expr,
        })
    }

    pub fn normalized_expr(&self) -> &str {
        &self.normalized_expr
    }

    pub async fn matches(&mut self, record: &JsonValue) -> Result<bool, VmError> {
        if self.closure.is_none() {
            self.vm.execute(&self.chunk).await?;
            self.closure = self.vm.resolve_named_closure(FILTER_FN_NAME);
        }
        let closure = self.closure.as_ref().ok_or_else(|| {
            VmError::Runtime("record filter closure was not installed".to_string())
        })?;
        let result = self
            .vm
            .call_closure_pub(closure, &[json_to_vm_value(record)])
            .await?;
        Ok(result.is_truthy())
    }
}

pub fn normalize_record_filter_expression(expr: &str) -> Result<String, String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err("filter expression cannot be empty".to_string());
    }

    let mut out = String::with_capacity(trimmed.len());
    let chars: Vec<char> = trimmed.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '\'' => {
                let (string, next) = parse_single_quoted_string(&chars, index)?;
                out.push_str(&string);
                index = next;
            }
            '"' => {
                let (string, next) = copy_double_quoted_string(&chars, index)?;
                out.push_str(&string);
                index = next;
            }
            ch if is_identifier_start(ch) => {
                let start = index;
                index += 1;
                while index < chars.len() && is_identifier_continue(chars[index]) {
                    index += 1;
                }
                let token: String = chars[start..index].iter().collect();
                if token.eq_ignore_ascii_case("and") {
                    out.push_str("&&");
                } else if token.eq_ignore_ascii_case("or") {
                    out.push_str("||");
                } else if token.eq_ignore_ascii_case("not") {
                    out.push('!');
                } else {
                    out.push_str(&token);
                }
            }
            ch => {
                out.push(ch);
                index += 1;
            }
        }
    }

    Ok(out)
}

fn parse_single_quoted_string(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut out = String::from("\"");
    let mut index = start + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => {
                index += 1;
                let escaped = chars.get(index).copied().ok_or_else(|| {
                    "unterminated escape in single-quoted filter string".to_string()
                })?;
                match escaped {
                    '\'' => out.push('\''),
                    '"' => {
                        out.push('\\');
                        out.push('"');
                    }
                    '\\' => {
                        out.push('\\');
                        out.push('\\');
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                index += 1;
            }
            '\'' => {
                out.push('"');
                return Ok((out, index + 1));
            }
            '"' => {
                out.push('\\');
                out.push('"');
                index += 1;
            }
            ch => {
                out.push(ch);
                index += 1;
            }
        }
    }
    Err("unterminated single-quoted filter string".to_string())
}

fn copy_double_quoted_string(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut out = String::from("\"");
    let mut index = start + 1;
    while index < chars.len() {
        let ch = chars[index];
        out.push(ch);
        index += 1;
        if ch == '\\' {
            let escaped = chars
                .get(index)
                .copied()
                .ok_or_else(|| "unterminated escape in filter string".to_string())?;
            out.push(escaped);
            index += 1;
            continue;
        }
        if ch == '"' {
            return Ok((out, index));
        }
    }
    Err("unterminated double-quoted filter string".to_string())
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{normalize_record_filter_expression, CompiledRecordFilter};

    #[test]
    fn normalize_sqlish_tokens_into_harn_expression() {
        let normalized = normalize_record_filter_expression(
            "event.payload.tenant == 'acme' AND NOT attempt.failed_at",
        )
        .expect("normalize filter");
        assert_eq!(
            normalized,
            "event.payload.tenant == \"acme\" && ! attempt.failed_at"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compiled_filter_matches_record_bindings() {
        let mut filter = CompiledRecordFilter::compile(
            "event.payload.tenant == 'acme' AND attempt.handler == 'handlers::risky'",
        )
        .expect("compile filter");
        assert_eq!(
            filter.normalized_expr(),
            "event.payload.tenant == \"acme\" && attempt.handler == \"handlers::risky\""
        );
        let matched = filter
            .matches(&json!({
                "event": {
                    "payload": { "tenant": "acme" }
                },
                "binding": {},
                "attempt": {
                    "handler": "handlers::risky"
                },
                "outcome": {},
                "audit": {}
            }))
            .await
            .expect("evaluate filter");
        assert!(matched);
    }
}
