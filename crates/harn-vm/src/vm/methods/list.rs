use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{compare_values, values_equal, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_list_method(
        &mut self,
        items: &Rc<Vec<VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "count" => Ok(VmValue::Int(items.len() as i64)),
            "empty" => Ok(VmValue::Bool(items.is_empty())),
            "map" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut results = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        results.push(self.call_callable_value(callable, &[item.clone()]).await?);
                    }
                    Ok(VmValue::List(Rc::new(results)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "filter" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut results = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            results.push(item.clone());
                        }
                    }
                    Ok(VmValue::List(Rc::new(results)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "reduce" => {
                if args.len() >= 2 && Self::is_callable_value(&args[1]) {
                    let callable = &args[1].clone();
                    let mut acc = args[0].clone();
                    for item in items.iter() {
                        acc = self
                            .call_callable_value(callable, &[acc, item.clone()])
                            .await?;
                    }
                    return Ok(acc);
                }
                Ok(VmValue::Nil)
            }
            "find" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            return Ok(item.clone());
                        }
                    }
                }
                Ok(VmValue::Nil)
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
            "all" | "every" | "all?" => {
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
            "flat_map" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut results = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if let VmValue::List(inner) = result {
                            results.extend(inner.iter().cloned());
                        } else {
                            results.push(result);
                        }
                    }
                    Ok(VmValue::List(Rc::new(results)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "sort" => {
                let mut sorted: Vec<VmValue> = items.iter().cloned().collect();
                sorted.sort_by(|a, b| compare_values(a, b).cmp(&0));
                Ok(VmValue::List(Rc::new(sorted)))
            }
            "sort_by" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut keyed: Vec<(VmValue, VmValue)> = Vec::new();
                    for item in items.iter() {
                        let key = self.call_callable_value(callable, &[item.clone()]).await?;
                        keyed.push((item.clone(), key));
                    }
                    keyed.sort_by(|(_, ka), (_, kb)| compare_values(ka, kb).cmp(&0));
                    Ok(VmValue::List(Rc::new(
                        keyed.into_iter().map(|(v, _)| v).collect(),
                    )))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "reverse" => {
                let mut rev: Vec<VmValue> = items.iter().cloned().collect();
                rev.reverse();
                Ok(VmValue::List(Rc::new(rev)))
            }
            "join" => {
                let sep = if args.is_empty() {
                    String::new()
                } else {
                    args[0].display()
                };
                let joined: String = items
                    .iter()
                    .map(|v| v.display())
                    .collect::<Vec<_>>()
                    .join(&sep);
                Ok(VmValue::String(Rc::from(joined)))
            }
            "contains" => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Ok(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
            }
            "index_of" => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                let idx = items.iter().position(|v| values_equal(v, needle));
                Ok(VmValue::Int(idx.map(|i| i as i64).unwrap_or(-1)))
            }
            "enumerate" => {
                let result: Vec<VmValue> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        VmValue::Dict(Rc::new(BTreeMap::from([
                            ("index".to_string(), VmValue::Int(i as i64)),
                            ("value".to_string(), v.clone()),
                        ])))
                    })
                    .collect();
                Ok(VmValue::List(Rc::new(result)))
            }
            "zip" => {
                if let Some(VmValue::List(other)) = args.first() {
                    let result: Vec<VmValue> = items
                        .iter()
                        .zip(other.iter())
                        .map(|(a, b)| VmValue::List(Rc::new(vec![a.clone(), b.clone()])))
                        .collect();
                    Ok(VmValue::List(Rc::new(result)))
                } else {
                    Ok(VmValue::List(Rc::new(Vec::new())))
                }
            }
            "slice" => {
                let len = items.len() as i64;
                let start_raw = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                let start = if start_raw < 0 {
                    (len + start_raw).max(0) as usize
                } else {
                    (start_raw.min(len)) as usize
                };
                let end = if args.len() > 1 {
                    let end_raw = args[1].as_int().unwrap_or(len);
                    if end_raw < 0 {
                        (len + end_raw).max(0) as usize
                    } else {
                        (end_raw.min(len)) as usize
                    }
                } else {
                    len as usize
                };
                let end = end.max(start);
                Ok(VmValue::List(Rc::new(items[start..end].to_vec())))
            }
            "unique" => {
                let mut seen = std::collections::HashSet::with_capacity(items.len());
                let mut result = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let key = crate::value::value_structural_hash_key(item);
                    if seen.insert(key) {
                        result.push(item.clone());
                    }
                }
                Ok(VmValue::List(Rc::new(result)))
            }
            "take" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                Ok(VmValue::List(Rc::new(
                    items.iter().take(n).cloned().collect(),
                )))
            }
            "skip" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                Ok(VmValue::List(Rc::new(
                    items.iter().skip(n).cloned().collect(),
                )))
            }
            "sum" => {
                let mut int_sum: i64 = 0;
                let mut has_float = false;
                let mut float_sum: f64 = 0.0;
                for item in items.iter() {
                    match item {
                        VmValue::Int(n) => {
                            int_sum = int_sum.wrapping_add(*n);
                            float_sum += *n as f64;
                        }
                        VmValue::Float(n) => {
                            has_float = true;
                            float_sum += n;
                        }
                        _ => {}
                    }
                }
                if has_float {
                    Ok(VmValue::Float(float_sum))
                } else {
                    Ok(VmValue::Int(int_sum))
                }
            }
            "min" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                let mut min_val = items[0].clone();
                for item in &items[1..] {
                    if compare_values(item, &min_val) < 0 {
                        min_val = item.clone();
                    }
                }
                Ok(min_val)
            }
            "max" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                let mut max_val = items[0].clone();
                for item in &items[1..] {
                    if compare_values(item, &max_val) > 0 {
                        max_val = item.clone();
                    }
                }
                Ok(max_val)
            }
            "flatten" => {
                let mut result = Vec::new();
                for item in items.iter() {
                    if let VmValue::List(inner) = item {
                        result.extend(inner.iter().cloned());
                    } else {
                        result.push(item.clone());
                    }
                }
                Ok(VmValue::List(Rc::new(result)))
            }
            "push" => {
                let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                if let Some(item) = args.first() {
                    new_list.push(item.clone());
                }
                Ok(VmValue::List(Rc::new(new_list)))
            }
            "pop" => {
                let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                new_list.pop();
                Ok(VmValue::List(Rc::new(new_list)))
            }
            "none" | "none?" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            return Ok(VmValue::Bool(false));
                        }
                    }
                    Ok(VmValue::Bool(true))
                } else {
                    Ok(VmValue::Bool(items.is_empty()))
                }
            }
            "find_index" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    for (i, item) in items.iter().enumerate() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            return Ok(VmValue::Int(i as i64));
                        }
                    }
                }
                Ok(VmValue::Int(-1))
            }
            "first" => {
                let n = args.first().and_then(|a| a.as_int());
                match n {
                    Some(count) => Ok(VmValue::List(Rc::new(
                        items.iter().take(count.max(0) as usize).cloned().collect(),
                    ))),
                    None => Ok(items.first().cloned().unwrap_or(VmValue::Nil)),
                }
            }
            "last" => {
                let n = args.first().and_then(|a| a.as_int());
                match n {
                    Some(count) => {
                        let count = count.max(0) as usize;
                        let skip = items.len().saturating_sub(count);
                        Ok(VmValue::List(Rc::new(
                            items.iter().skip(skip).cloned().collect(),
                        )))
                    }
                    None => Ok(items.last().cloned().unwrap_or(VmValue::Nil)),
                }
            }
            "partition" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut truthy = Vec::new();
                    let mut falsy = Vec::new();
                    for item in items.iter() {
                        let result = self.call_callable_value(callable, &[item.clone()]).await?;
                        if result.is_truthy() {
                            truthy.push(item.clone());
                        } else {
                            falsy.push(item.clone());
                        }
                    }
                    Ok(VmValue::List(Rc::new(vec![
                        VmValue::List(Rc::new(truthy)),
                        VmValue::List(Rc::new(falsy)),
                    ])))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "group_by" => {
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut groups: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
                    for item in items.iter() {
                        let key = self.call_callable_value(callable, &[item.clone()]).await?;
                        let key_str = key.display();
                        groups.entry(key_str).or_default().push(item.clone());
                    }
                    let result: BTreeMap<String, VmValue> = groups
                        .into_iter()
                        .map(|(k, v)| (k, VmValue::List(Rc::new(v))))
                        .collect();
                    Ok(VmValue::Dict(Rc::new(result)))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "chunk" | "each_slice" => {
                let size = args.first().and_then(|a| a.as_int()).unwrap_or(1).max(1) as usize;
                let chunks: Vec<VmValue> = items
                    .chunks(size)
                    .map(|c| VmValue::List(Rc::new(c.to_vec())))
                    .collect();
                Ok(VmValue::List(Rc::new(chunks)))
            }
            "min_by" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut best = items[0].clone();
                    let mut best_key = self.call_callable_value(callable, &[best.clone()]).await?;
                    for item in &items[1..] {
                        let key = self.call_callable_value(callable, &[item.clone()]).await?;
                        if compare_values(&key, &best_key) < 0 {
                            best = item.clone();
                            best_key = key;
                        }
                    }
                    Ok(best)
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "max_by" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) {
                    let mut best = items[0].clone();
                    let mut best_key = self.call_callable_value(callable, &[best.clone()]).await?;
                    for item in &items[1..] {
                        let key = self.call_callable_value(callable, &[item.clone()]).await?;
                        if compare_values(&key, &best_key) > 0 {
                            best = item.clone();
                            best_key = key;
                        }
                    }
                    Ok(best)
                } else {
                    Ok(VmValue::Nil)
                }
            }
            "compact" => {
                let result: Vec<VmValue> = items
                    .iter()
                    .filter(|v| !matches!(v, VmValue::Nil))
                    .cloned()
                    .collect();
                Ok(VmValue::List(Rc::new(result)))
            }
            "window" | "each_cons" | "sliding_window" => {
                let size = args.first().and_then(|a| a.as_int()).unwrap_or(2).max(1) as usize;
                let step = args.get(1).and_then(|a| a.as_int()).unwrap_or(1).max(1) as usize;
                if size > items.len() {
                    return Ok(VmValue::List(Rc::new(Vec::new())));
                }
                let mut windows = Vec::new();
                let mut start = 0;
                while start + size <= items.len() {
                    windows.push(VmValue::List(Rc::new(items[start..start + size].to_vec())));
                    start += step;
                }
                Ok(VmValue::List(Rc::new(windows)))
            }
            "tally" => {
                let mut counts: BTreeMap<String, VmValue> = BTreeMap::new();
                for item in items.iter() {
                    let key = item.display();
                    let current = counts.get(&key).and_then(|v| v.as_int()).unwrap_or(0);
                    counts.insert(key, VmValue::Int(current + 1));
                }
                Ok(VmValue::Dict(Rc::new(counts)))
            }
            _ => Ok(VmValue::Nil),
        }
    }
}
