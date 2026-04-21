use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_set_method(
        &mut self,
        items: &Rc<Vec<VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "count" | "len" => Ok(VmValue::Int(items.len() as i64)),
            "empty" => Ok(VmValue::Bool(items.is_empty())),
            "contains" => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Ok(VmValue::Bool(items.iter().any(|x| values_equal(x, needle))))
            }
            "add" => {
                let val = args.first().cloned().unwrap_or(VmValue::Nil);
                let mut new_items = items.to_vec();
                if !new_items.iter().any(|x| values_equal(x, &val)) {
                    new_items.push(val);
                }
                Ok(VmValue::Set(Rc::new(new_items)))
            }
            "remove" | "delete" => {
                let val = args.first().unwrap_or(&VmValue::Nil);
                let new_items: Vec<VmValue> = items
                    .iter()
                    .filter(|x| !values_equal(x, val))
                    .cloned()
                    .collect();
                Ok(VmValue::Set(Rc::new(new_items)))
            }
            "union" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    let mut result = items.to_vec();
                    for v in other.iter() {
                        if !result.iter().any(|x| values_equal(x, v)) {
                            result.push(v.clone());
                        }
                    }
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Set(Rc::clone(items)))
                }
            }
            "intersect" | "intersection" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    let result: Vec<VmValue> = items
                        .iter()
                        .filter(|x| other.iter().any(|y| values_equal(x, y)))
                        .cloned()
                        .collect();
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Set(Rc::new(Vec::new())))
                }
            }
            "difference" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    let result: Vec<VmValue> = items
                        .iter()
                        .filter(|x| !other.iter().any(|y| values_equal(x, y)))
                        .cloned()
                        .collect();
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Set(Rc::clone(items)))
                }
            }
            "symmetric_difference" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    let mut result: Vec<VmValue> = items
                        .iter()
                        .filter(|x| !other.iter().any(|y| values_equal(x, y)))
                        .cloned()
                        .collect();
                    for v in other.iter() {
                        if !items.iter().any(|x| values_equal(x, v)) {
                            result.push(v.clone());
                        }
                    }
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Set(Rc::clone(items)))
                }
            }
            "is_subset" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    Ok(VmValue::Bool(
                        items
                            .iter()
                            .all(|x| other.iter().any(|y| values_equal(x, y))),
                    ))
                } else {
                    Ok(VmValue::Bool(false))
                }
            }
            "is_superset" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    Ok(VmValue::Bool(
                        other
                            .iter()
                            .all(|x| items.iter().any(|y| values_equal(x, y))),
                    ))
                } else {
                    Ok(VmValue::Bool(false))
                }
            }
            "is_disjoint" => {
                if let Some(VmValue::Set(other)) = args.first() {
                    Ok(VmValue::Bool(
                        !items
                            .iter()
                            .any(|x| other.iter().any(|y| values_equal(x, y))),
                    ))
                } else {
                    Ok(VmValue::Bool(true))
                }
            }
            "to_list" => Ok(VmValue::List(Rc::new(items.to_vec()))),
            "map" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut result = Vec::new();
                    for item in items.iter() {
                        let mapped = self.call_callable_value(callable, &[item.clone()]).await?;
                        if !result.iter().any(|x| values_equal(x, &mapped)) {
                            result.push(mapped);
                        }
                    }
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "filter" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut result = Vec::new();
                    for item in items.iter() {
                        let keep = self.call_callable_value(callable, &[item.clone()]).await?;
                        if keep.is_truthy() {
                            result.push(item.clone());
                        }
                    }
                    Ok(VmValue::Set(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "any" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            return Ok(VmValue::Bool(true));
                        }
                    }
                    Ok(VmValue::Bool(false))
                } else {
                    Ok(VmValue::Bool(false))
                }
            }
            "all" | "every" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if !result.is_truthy() {
                            return Ok(VmValue::Bool(false));
                        }
                    }
                    Ok(VmValue::Bool(true))
                } else {
                    Ok(VmValue::Bool(true))
                }
            }
            _ => Ok(VmValue::Nil),
        }
    }
}
