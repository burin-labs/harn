use harn_vm::VmValue;
use serde_json::json;

use super::state::{Debugger, PathSegment};
use crate::protocol::*;

fn vm_type_name(val: &VmValue) -> &'static str {
    val.type_name()
}

impl Debugger {
    pub(crate) fn alloc_var_ref(&mut self, children: Vec<(String, VmValue)>) -> i64 {
        let id = self.next_var_ref;
        self.next_var_ref += 1;
        self.var_refs.insert(id, children);
        id
    }

    pub(crate) fn make_variable(&mut self, name: String, val: &VmValue) -> Variable {
        let (var_ref, display) = match val {
            VmValue::List(items) => {
                let children: Vec<(String, VmValue)> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("[{i}]"), v.clone()))
                    .collect();
                let display = format!("list<{}>", items.len());
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::Dict(map) => {
                let children: Vec<(String, VmValue)> =
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let display = format!("dict<{}>", map.len());
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::StructInstance {
                struct_name,
                fields,
            } => {
                let children: Vec<(String, VmValue)> =
                    fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let display = struct_name.clone();
                if children.is_empty() {
                    (0, display)
                } else {
                    (self.alloc_var_ref(children), display)
                }
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    (0, format!("{enum_name}.{variant}"))
                } else {
                    let children: Vec<(String, VmValue)> = fields
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (format!("field_{i}"), v.clone()))
                        .collect();
                    let display = format!("{enum_name}.{variant}(...)");
                    (self.alloc_var_ref(children), display)
                }
            }
            other => (0, other.display()),
        };
        Variable {
            name,
            value: display,
            var_type: vm_type_name(val).to_string(),
            variables_reference: var_ref,
        }
    }

    pub(crate) fn handle_scopes(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let scopes = vec![Scope {
            name: "Locals".to_string(),
            variables_reference: 1,
            expensive: false,
        }];

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "scopes",
            Some(json!({ "scopes": scopes })),
        )]
    }

    pub(crate) fn handle_variables(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let ref_id = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("variablesReference"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1);

        // Ref IDs >= 100 index `self.var_refs` (children of composite values).
        if ref_id >= 100 {
            if let Some(children) = self.var_refs.get(&ref_id).cloned() {
                let vars: Vec<Variable> = children
                    .iter()
                    .map(|(name, val)| self.make_variable(name.clone(), val))
                    .collect();
                let seq = self.next_seq();
                return vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "variables",
                    Some(json!({ "variables": vars })),
                )];
            }
        }

        // Fallback: scope 1 is the locals map.
        let variable_list: Vec<(String, VmValue)> = self.variables.clone().into_iter().collect();
        let vars: Vec<Variable> = variable_list
            .iter()
            .map(|(name, val)| self.make_variable(name.clone(), val))
            .collect();

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "variables",
            Some(json!({ "variables": vars })),
        )]
    }

    pub(crate) fn handle_evaluate(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let expression = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("expression"))
            .and_then(|e| e.as_str())
            .unwrap_or("");

        // DAP context is one of "watch", "repl", "hover", "clipboard"; we ignore it.
        let _context = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("context"))
            .and_then(|c| c.as_str())
            .unwrap_or("watch");

        match self.resolve_expression(expression) {
            Some(val) => {
                let variable = self.make_variable(expression.to_string(), &val);
                let seq = self.next_seq();
                vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "evaluate",
                    Some(json!({
                        "result": variable.value,
                        "type": variable.var_type,
                        "variablesReference": variable.variables_reference,
                    })),
                )]
            }
            None => {
                let seq = self.next_seq();
                vec![DapResponse {
                    seq,
                    msg_type: "response".to_string(),
                    request_seq: Some(msg.seq),
                    success: Some(false),
                    command: Some("evaluate".to_string()),
                    message: Some(format!(
                        "Cannot evaluate '{expression}': only variable lookups and dot-access \
                         property paths are supported in the debugger"
                    )),
                    body: None,
                    event: None,
                }]
            }
        }
    }

    /// Resolve an expression string against the current variable state.
    /// Supports: variable names ("x"), dot-access ("x.foo.bar"),
    /// subscript access ("x[0]", "x[\"key\"]"), len(x), type_of(x).
    fn resolve_expression(&self, expression: &str) -> Option<VmValue> {
        let expr = expression.trim();

        if let Some(inner) = expr.strip_prefix("len(").and_then(|s| s.strip_suffix(')')) {
            let val = self.resolve_expression(inner)?;
            return match &val {
                VmValue::String(s) => Some(VmValue::Int(s.len() as i64)),
                VmValue::List(l) => Some(VmValue::Int(l.len() as i64)),
                VmValue::Dict(d) => Some(VmValue::Int(d.len() as i64)),
                _ => None,
            };
        }
        if let Some(inner) = expr
            .strip_prefix("type_of(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let val = self.resolve_expression(inner)?;
            let type_name = match &val {
                VmValue::Int(_) => "int",
                VmValue::Float(_) => "float",
                VmValue::String(_) => "string",
                VmValue::Bool(_) => "bool",
                VmValue::Nil => "nil",
                VmValue::List(_) => "list",
                VmValue::Dict(_) => "dict",
                _ => "unknown",
            };
            return Some(VmValue::String(std::rc::Rc::from(type_name)));
        }

        // Tokenize into a path of `Field(name)` and `Index(n)` segments.
        let mut segments = Vec::new();
        let mut chars = expr.chars().peekable();
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return None;
        }
        segments.push(PathSegment::Field(name));

        while let Some(&c) = chars.peek() {
            match c {
                '.' => {
                    chars.next();
                    let mut field = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            field.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if field.is_empty() {
                        return None;
                    }
                    segments.push(PathSegment::Field(field));
                }
                '[' => {
                    chars.next();
                    let mut idx = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ']' {
                            chars.next();
                            break;
                        }
                        idx.push(c);
                        chars.next();
                    }
                    let idx = idx.trim().trim_matches('"').trim_matches('\'');
                    if let Ok(n) = idx.parse::<i64>() {
                        segments.push(PathSegment::Index(n));
                    } else {
                        segments.push(PathSegment::Field(idx.to_string()));
                    }
                }
                _ => return None,
            }
        }

        let root_name = match &segments[0] {
            PathSegment::Field(n) => n.as_str(),
            _ => return None,
        };
        let mut current = self.variables.get(root_name)?.clone();

        for seg in &segments[1..] {
            current = match seg {
                PathSegment::Field(f) => match &current {
                    VmValue::Dict(map) => map.get(f.as_str())?.clone(),
                    VmValue::StructInstance { fields, .. } => fields.get(f.as_str())?.clone(),
                    _ => return None,
                },
                PathSegment::Index(i) => match &current {
                    VmValue::List(list) => {
                        let idx = if *i < 0 {
                            (list.len() as i64 + i) as usize
                        } else {
                            *i as usize
                        };
                        list.get(idx)?.clone()
                    }
                    VmValue::Dict(map) => map.get(&i.to_string())?.clone(),
                    _ => return None,
                },
            };
        }

        Some(current)
    }
}
