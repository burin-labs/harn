//! Swift-style `mutating` builtin methods (`xs.push(v)`, `xs.pop()`,
//! `s.add(v)`, `d.remove(k)`).
//!
//! Values stay immutable: a mutating call computes the receiver's next value
//! and its result, and the compiled call site writes that value back to the
//! receiver's place. The method registry
//! (`harn_parser::builtin_methods`) names which methods mutate.

use std::sync::Arc;

use crate::value::{compare_values, VmError, VmValue};

/// The receiver's next value and the call's result.
pub struct Mutation {
    pub receiver: VmValue,
    pub result: VmValue,
}

impl crate::vm::Vm {
    /// Apply a mutating builtin method to `receiver`. `None` means the
    /// receiver is not a builtin value with that mutating method (a struct
    /// whose impl defines `push`, a dict carrying a callable `remove`, a
    /// module namespace), so the caller performs an ordinary method call.
    pub async fn apply_mutating_method(
        &mut self,
        receiver: &VmValue,
        method: &str,
        args: &[VmValue],
    ) -> Result<Option<Mutation>, VmError> {
        match receiver {
            VmValue::List(items) => self.mutate_list(items, method, args).await,
            VmValue::Set(set) => Ok(Self::mutate_set(set, method, args)),
            VmValue::Dict(map) => {
                if matches!(map.get("_namespace"), Some(VmValue::String(_)))
                    || map.get(method).is_some_and(Self::is_callable_value)
                {
                    return Ok(None);
                }
                Ok(Self::mutate_dict(map, method, args))
            }
            _ => Ok(None),
        }
    }

    async fn mutate_list(
        &mut self,
        items: &Arc<Vec<VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<Option<Mutation>, VmError> {
        let unchanged = |result| Mutation {
            receiver: VmValue::List(Arc::clone(items)),
            result,
        };
        let changed = |next: Vec<VmValue>, result| Mutation {
            receiver: VmValue::List(Arc::new(next)),
            result,
        };
        let mutation = match method {
            "push" => {
                let mut next = items.to_vec();
                next.push(args.first().cloned().unwrap_or(VmValue::Nil));
                changed(next, VmValue::Nil)
            }
            "pop" => {
                if items.is_empty() {
                    unchanged(VmValue::Nil)
                } else {
                    let mut next = items.to_vec();
                    let last = next.pop().unwrap_or(VmValue::Nil);
                    changed(next, last)
                }
            }
            "insert" => {
                let index = match args.first() {
                    Some(VmValue::Int(index)) => *index,
                    other => {
                        return Err(VmError::TypeError(format!(
                            "list.insert: index must be an int, got {}",
                            other.map_or("nothing", VmValue::type_name)
                        )))
                    }
                };
                let len = items.len() as i64;
                let at = if index < 0 { len + index } else { index }.clamp(0, len) as usize;
                let mut next = items.to_vec();
                next.insert(at, args.get(1).cloned().unwrap_or(VmValue::Nil));
                changed(next, VmValue::Nil)
            }
            "extend" => {
                let mut next = items.to_vec();
                match args.first() {
                    Some(VmValue::List(more)) => next.extend(more.iter().cloned()),
                    Some(VmValue::Set(more)) => next.extend(more.iter().cloned()),
                    other => {
                        return Err(VmError::TypeError(format!(
                            "list.extend: expected a list, got {}",
                            other.map_or("nothing", VmValue::type_name)
                        )))
                    }
                }
                changed(next, VmValue::Nil)
            }
            "clear" => changed(Vec::new(), VmValue::Nil),
            "sort" => {
                if !args.is_empty() {
                    return Err(VmError::TypeError(
                        "list.sort takes no arguments; use sort_by(key) to sort by a key"
                            .to_string(),
                    ));
                }
                let mut next = items.to_vec();
                next.sort_by(|a, b| compare_values(a, b).cmp(&0));
                changed(next, VmValue::Nil)
            }
            "sort_by" => {
                let Some(key) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Err(VmError::TypeError(
                        "list.sort_by: expected a key function".to_string(),
                    ));
                };
                let mut keyed = Vec::with_capacity(items.len());
                for item in items.iter() {
                    keyed.push((self.call_callable_one(key, item).await?, item.clone()));
                }
                keyed.sort_by(|(a, _), (b, _)| compare_values(a, b).cmp(&0));
                changed(
                    keyed.into_iter().map(|(_, item)| item).collect(),
                    VmValue::Nil,
                )
            }
            "reverse" => {
                let mut next = items.to_vec();
                next.reverse();
                changed(next, VmValue::Nil)
            }
            _ => return Ok(None),
        };
        Ok(Some(mutation))
    }

    fn mutate_set(
        set: &Arc<crate::value::VmSet>,
        method: &str,
        args: &[VmValue],
    ) -> Option<Mutation> {
        let item = args.first().cloned().unwrap_or(VmValue::Nil);
        let mut next = (**set).clone();
        let result = match method {
            "add" => {
                next.insert(item);
                VmValue::Nil
            }
            "remove" => VmValue::Bool(next.remove(&item)),
            "clear" => {
                next = crate::value::VmSet::new();
                VmValue::Nil
            }
            _ => return None,
        };
        Some(Mutation {
            receiver: VmValue::set_value(next),
            result,
        })
    }

    fn mutate_dict(
        map: &Arc<crate::value::DictMap>,
        method: &str,
        args: &[VmValue],
    ) -> Option<Mutation> {
        let mut next = (**map).clone();
        let result = match method {
            "remove" => {
                let key = args.first().map(|a| a.display()).unwrap_or_default();
                next.remove(key.as_str()).unwrap_or(VmValue::Nil)
            }
            "clear" => {
                next = crate::value::DictMap::new();
                VmValue::Nil
            }
            _ => return None,
        };
        Some(Mutation {
            receiver: VmValue::dict_map(next),
            result,
        })
    }
}
