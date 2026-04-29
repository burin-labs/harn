use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_dict_method(
        &mut self,
        map: &Rc<BTreeMap<String, VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if matches!(map.get("_namespace"), Some(VmValue::String(name)) if name.as_ref() == "stream")
        {
            if let Some(callable) = map.get(method).filter(|v| Self::is_callable_value(v)) {
                return self.call_callable_value(callable, args).await;
            }
        }
        match method {
            "keys" => Ok(VmValue::List(Rc::new(
                map.keys()
                    .map(|k| VmValue::String(Rc::from(k.as_str())))
                    .collect(),
            ))),
            "values" => Ok(VmValue::List(Rc::new(map.values().cloned().collect()))),
            "entries" => Ok(VmValue::List(Rc::new(
                map.iter()
                    .map(|(k, v)| {
                        VmValue::Dict(Rc::new(BTreeMap::from([
                            ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                            ("value".to_string(), v.clone()),
                        ])))
                    })
                    .collect(),
            ))),
            "count" => Ok(VmValue::Int(map.len() as i64)),
            "has" => Ok(VmValue::Bool(map.contains_key(
                &args.first().map(|a| a.display()).unwrap_or_default(),
            ))),
            "merge" => {
                if let Some(VmValue::Dict(other)) = args.first() {
                    let mut result = (**map).clone();
                    result.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Dict(Rc::clone(map)))
                }
            }
            "map_values" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut result = BTreeMap::new();
                    for (k, v) in map.iter() {
                        let mapped = self.call_callable_value(callable, &[v.clone()]).await?;
                        result.insert(k.clone(), mapped);
                    }
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "rekey" | "map_keys" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut result = BTreeMap::new();
                    for (k, v) in map.iter() {
                        let new_key = self
                            .call_callable_value(callable, &[VmValue::String(Rc::from(k.as_str()))])
                            .await?;
                        let new_key_str = new_key.display();
                        result.insert(new_key_str, v.clone());
                    }
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "filter" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut result = BTreeMap::new();
                    for (k, v) in map.iter() {
                        let keep = self.call_callable_value(callable, &[v.clone()]).await?;
                        if keep.is_truthy() {
                            result.insert(k.clone(), v.clone());
                        }
                    }
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
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
            _ => {
                if let Some(callable) = map.get(method).filter(|v| Self::is_callable_value(v)) {
                    self.call_callable_value(callable, args).await
                } else {
                    Ok(VmValue::Nil)
                }
            }
        }
    }
}
