use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_dict_method_sync(
        map: &Rc<BTreeMap<String, VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if matches!(map.get("_namespace"), Some(VmValue::String(_)))
            && map.get(method).is_some_and(Self::is_callable_value)
        {
            return None;
        }

        let result = match method {
            "keys" => Ok(VmValue::List(Rc::new(
                map.keys()
                    .map(|k| VmValue::String(Rc::from(k.as_str())))
                    .collect(),
            ))),
            "values" => Ok(VmValue::List(Rc::new(map.values().cloned().collect()))),
            "entries" => Ok(VmValue::List(Rc::new(Self::dict_entries(map)))),
            "count" => Ok(VmValue::Int(map.len() as i64)),
            "has" => Ok(VmValue::Bool(map.contains_key(
                &args.first().map(|a| a.display()).unwrap_or_default(),
            ))),
            "merge" => {
                if let Some(VmValue::Dict(other)) = args.first() {
                    if map.is_empty() {
                        return Some(Ok(VmValue::Dict(Rc::clone(other))));
                    }
                    if other.is_empty() {
                        return Some(Ok(VmValue::Dict(Rc::clone(map))));
                    }
                    let mut result = (**map).clone();
                    result.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Dict(Rc::clone(map)))
                }
            }
            "map_values" | "rekey" | "map_keys" | "filter" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Nil)
            }
            "remove" => {
                let key = args.first().map(|a| a.display()).unwrap_or_default();
                let mut result = (**map).clone();
                result.remove(&key);
                Ok(VmValue::Dict(Rc::new(result)))
            }
            "get" => {
                let key = args.first().map(|a| a.display()).unwrap_or_default();
                let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                Ok(map.get(&key).cloned().unwrap_or(default))
            }
            "to_dict" => Ok(VmValue::Dict(Rc::clone(map))),
            "to_list" => Ok(VmValue::List(Rc::new(Self::dict_entries(map)))),
            _ => {
                if map.get(method).is_some_and(Self::is_callable_value) {
                    return None;
                }
                Err(VmError::Runtime(format!("dict has no method `{method}`")))
            }
        };
        Some(result)
    }

    pub(super) async fn call_dict_method(
        &mut self,
        map: &Rc<BTreeMap<String, VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(result) = Self::call_dict_method_sync(map, method, args) {
            return result;
        }

        if matches!(map.get("_namespace"), Some(VmValue::String(_))) {
            if let Some(callable) = map.get(method).filter(|v| Self::is_callable_value(v)) {
                return self.call_callable_value(callable, args).await;
            }
        }

        match method {
            "map_values" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut result = BTreeMap::new();
                for (k, v) in map.iter() {
                    let mapped = self.call_callable_value(callable, &[v.clone()]).await?;
                    result.insert(k.clone(), mapped);
                }
                Ok(VmValue::Dict(Rc::new(result)))
            }
            "rekey" | "map_keys" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut result = BTreeMap::new();
                for (k, v) in map.iter() {
                    let new_key = self
                        .call_callable_value(callable, &[VmValue::String(Rc::from(k.as_str()))])
                        .await?;
                    let new_key_str = new_key.display();
                    result.insert(new_key_str, v.clone());
                }
                Ok(VmValue::Dict(Rc::new(result)))
            }
            "filter" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut result = BTreeMap::new();
                for (k, v) in map.iter() {
                    let keep = self.call_callable_value(callable, &[v.clone()]).await?;
                    if keep.is_truthy() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(VmValue::Dict(Rc::new(result)))
            }
            _ => {
                if let Some(callable) = map.get(method).filter(|v| Self::is_callable_value(v)) {
                    self.call_callable_value(callable, args).await
                } else {
                    Err(VmError::Runtime(format!("dict has no method `{method}`")))
                }
            }
        }
    }

    fn dict_entries(map: &BTreeMap<String, VmValue>) -> Vec<VmValue> {
        map.iter()
            .map(|(k, v)| {
                VmValue::Dict(Rc::new(BTreeMap::from([
                    ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                    ("value".to_string(), v.clone()),
                ])))
            })
            .collect()
    }
}
