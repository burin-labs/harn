use std::sync::Arc;

use crate::value::{VmError, VmSet, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_set_method_sync(
        set: &Arc<VmSet>,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let result = match method {
            "count" | "len" => Ok(VmValue::Int(set.len() as i64)),
            "empty" => Ok(VmValue::Bool(set.is_empty())),
            "contains" | "includes" => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Ok(VmValue::Bool(set.contains(needle)))
            }
            "adding" | "add" => {
                let mut new_set = (**set).clone();
                new_set.insert(args.first().cloned().unwrap_or(VmValue::Nil));
                Ok(VmValue::set_value(new_set))
            }
            "removing" | "remove" | "delete" => {
                let mut new_set = (**set).clone();
                new_set.remove(args.first().unwrap_or(&VmValue::Nil));
                Ok(VmValue::set_value(new_set))
            }
            "union" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::set_value(set.union(other))),
                _ => Ok(VmValue::Set(Arc::clone(set))),
            },
            "intersect" | "intersection" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::set_value(set.intersect(other))),
                _ => Ok(VmValue::set_value(VmSet::new())),
            },
            "difference" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::set_value(set.difference(other))),
                _ => Ok(VmValue::Set(Arc::clone(set))),
            },
            "symmetric_difference" => match args.first() {
                Some(VmValue::Set(other)) => {
                    Ok(VmValue::set_value(set.symmetric_difference(other)))
                }
                _ => Ok(VmValue::Set(Arc::clone(set))),
            },
            "is_subset" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::Bool(set.is_subset(other))),
                _ => Ok(VmValue::Bool(false)),
            },
            "is_superset" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::Bool(set.is_superset(other))),
                _ => Ok(VmValue::Bool(false)),
            },
            "is_disjoint" => match args.first() {
                Some(VmValue::Set(other)) => Ok(VmValue::Bool(set.is_disjoint(other))),
                _ => Ok(VmValue::Bool(true)),
            },
            "to_list" => Ok(VmValue::List(set.shared_items())),
            "to_set" => Ok(VmValue::Set(Arc::clone(set))),
            "map" | "filter" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Nil)
            }
            "any" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Bool(false))
            }
            "all" | "every" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Bool(true))
            }
            _ => Err(VmError::Runtime(format!("set has no method `{method}`"))),
        };
        Some(result)
    }

    pub(super) async fn call_set_method(
        &mut self,
        set: &Arc<VmSet>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(result) = Self::call_set_method_sync(set, method, args) {
            return result;
        }

        match method {
            "map" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut result = VmSet::new();
                for item in set.iter() {
                    let mapped = self.call_callable_one(callable, item).await?;
                    result.insert(mapped);
                }
                Ok(VmValue::set_value(result))
            }
            "filter" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut result = VmSet::new();
                for item in set.iter() {
                    let keep = self.call_callable_one(callable, item).await?;
                    if keep.is_truthy() {
                        result.insert(item.clone());
                    }
                }
                Ok(VmValue::set_value(result))
            }
            "any" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Bool(false));
                };
                for item in set.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        return Ok(VmValue::Bool(true));
                    }
                }
                Ok(VmValue::Bool(false))
            }
            "all" | "every" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Bool(true));
                };
                for item in set.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if !result.is_truthy() {
                        return Ok(VmValue::Bool(false));
                    }
                }
                Ok(VmValue::Bool(true))
            }
            _ => Err(VmError::Runtime(format!("set has no method `{method}`"))),
        }
    }
}
