use std::collections::BTreeMap;
use std::sync::Arc;

use crate::value::{compare_values, values_equal, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_list_method_sync(
        items: &Arc<Vec<VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let result = match method {
            "count" => Ok(VmValue::Int(items.len() as i64)),
            "empty" => Ok(VmValue::Bool(items.is_empty())),
            "map" | "filter" | "find" | "flat_map" | "sort_by" | "partition" | "group_by"
            | "take_while" | "drop_while" | "count_by" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Nil)
            }
            "reduce" => {
                if args.len() >= 2 && Self::is_callable_value(&args[1]) {
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
            "all" | "every" | "all?" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Bool(true))
            }
            "sort" => {
                let mut sorted: Vec<VmValue> = items.iter().cloned().collect();
                sorted.sort_by(|a, b| compare_values(a, b).cmp(&0));
                Ok(VmValue::List(std::sync::Arc::new(sorted)))
            }
            "reverse" => {
                let mut rev: Vec<VmValue> = items.iter().cloned().collect();
                rev.reverse();
                Ok(VmValue::List(std::sync::Arc::new(rev)))
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
                Ok(VmValue::String(std::sync::Arc::from(joined)))
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
                        VmValue::dict(BTreeMap::from([
                            ("index".to_string(), VmValue::Int(i as i64)),
                            ("value".to_string(), v.clone()),
                        ]))
                    })
                    .collect();
                Ok(VmValue::List(std::sync::Arc::new(result)))
            }
            "zip" => {
                if let Some(VmValue::List(other)) = args.first() {
                    let result: Vec<VmValue> = items
                        .iter()
                        .zip(other.iter())
                        .map(|(a, b)| {
                            VmValue::List(std::sync::Arc::new(vec![a.clone(), b.clone()]))
                        })
                        .collect();
                    Ok(VmValue::List(std::sync::Arc::new(result)))
                } else {
                    Ok(VmValue::List(std::sync::Arc::new(Vec::new())))
                }
            }
            "slice" => {
                let len = items.len() as i64;
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
                Ok(VmValue::List(std::sync::Arc::new(
                    items[start..end].to_vec(),
                )))
            }
            "unique" => Ok(VmValue::List(std::sync::Arc::new(
                crate::value::dedup_values(items.iter()),
            ))),
            "take" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                Ok(VmValue::List(std::sync::Arc::new(
                    items.iter().take(n).cloned().collect(),
                )))
            }
            "skip" => {
                let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                Ok(VmValue::List(std::sync::Arc::new(
                    items.iter().skip(n).cloned().collect(),
                )))
            }
            "sum" => {
                let mut int_sum: i64 = 0;
                let mut overflowed = false;
                let mut has_float = false;
                let mut float_sum: f64 = 0.0;
                let mut has_decimal = false;
                let mut decimal_sum = rust_decimal::Decimal::ZERO;
                let mut decimal_overflow = false;
                for item in items.iter() {
                    match item {
                        VmValue::Int(n) => {
                            // Promote to float on i64 overflow rather than
                            // silently wrapping (matching `abs`/`pow`); the
                            // float accumulator below is the fallback value.
                            match int_sum.checked_add(*n) {
                                Some(sum) => int_sum = sum,
                                None => overflowed = true,
                            }
                            float_sum += *n as f64;
                            // Also fold into the decimal accumulator so an
                            // int+decimal list promotes ints exactly. Only used
                            // when the list actually contains a decimal.
                            match decimal_sum.checked_add(rust_decimal::Decimal::from(*n)) {
                                Some(sum) => decimal_sum = sum,
                                None => decimal_overflow = true,
                            }
                        }
                        VmValue::Float(n) => {
                            has_float = true;
                            float_sum += n;
                        }
                        VmValue::Decimal(d) => {
                            has_decimal = true;
                            match decimal_sum.checked_add(*d) {
                                Some(sum) => decimal_sum = sum,
                                None => decimal_overflow = true,
                            }
                        }
                        _ => {}
                    }
                }
                // A decimal in the list makes the whole sum a decimal (ints
                // promote exactly); mixing with float is refused, not silently
                // dropped, mirroring the arithmetic operators.
                if has_decimal {
                    if has_float {
                        return Some(Err(VmError::TypeError(
                            "sum: cannot mix decimal and float values".to_string(),
                        )));
                    }
                    if decimal_overflow {
                        return Some(Err(VmError::Runtime(
                            "sum: decimal addition overflowed".to_string(),
                        )));
                    }
                    return Some(Ok(VmValue::Decimal(decimal_sum)));
                }
                if has_float || overflowed {
                    Ok(VmValue::Float(float_sum))
                } else {
                    Ok(VmValue::Int(int_sum))
                }
            }
            "min" => {
                if items.is_empty() {
                    return Some(Ok(VmValue::Nil));
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
                    return Some(Ok(VmValue::Nil));
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
                Ok(VmValue::List(std::sync::Arc::new(result)))
            }
            "push" => {
                let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                if let Some(item) = args.first() {
                    new_list.push(item.clone());
                }
                Ok(VmValue::List(std::sync::Arc::new(new_list)))
            }
            "pop" => {
                let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                new_list.pop();
                Ok(VmValue::List(std::sync::Arc::new(new_list)))
            }
            "none" | "none?" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Bool(items.is_empty()))
            }
            "find_index" => {
                if args.first().is_some_and(Self::is_callable_value) {
                    return None;
                }
                Ok(VmValue::Int(-1))
            }
            "first" => {
                let n = args.first().and_then(|a| a.as_int());
                match n {
                    Some(count) => Ok(VmValue::List(std::sync::Arc::new(
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
                        Ok(VmValue::List(std::sync::Arc::new(
                            items.iter().skip(skip).cloned().collect(),
                        )))
                    }
                    None => Ok(items.last().cloned().unwrap_or(VmValue::Nil)),
                }
            }
            "chunk" | "each_slice" => {
                let size = args.first().and_then(|a| a.as_int()).unwrap_or(1).max(1) as usize;
                let chunks: Vec<VmValue> = items
                    .chunks(size)
                    .map(|c| VmValue::List(std::sync::Arc::new(c.to_vec())))
                    .collect();
                Ok(VmValue::List(std::sync::Arc::new(chunks)))
            }
            "min_by" | "max_by" => {
                if items.is_empty() {
                    Ok(VmValue::Nil)
                } else if args.first().is_some_and(Self::is_callable_value) {
                    return None;
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
                Ok(VmValue::List(std::sync::Arc::new(result)))
            }
            "window" | "each_cons" | "sliding_window" => {
                let size = args.first().and_then(|a| a.as_int()).unwrap_or(2).max(1) as usize;
                let step = args.get(1).and_then(|a| a.as_int()).unwrap_or(1).max(1) as usize;
                if size > items.len() {
                    return Some(Ok(VmValue::List(std::sync::Arc::new(Vec::new()))));
                }
                let mut windows = Vec::new();
                let mut start = 0;
                while start + size <= items.len() {
                    windows.push(VmValue::List(std::sync::Arc::new(
                        items[start..start + size].to_vec(),
                    )));
                    start += step;
                }
                Ok(VmValue::List(std::sync::Arc::new(windows)))
            }
            "tally" => {
                // Strict-string discriminator: dict keys are intrinsically
                // `String`, and the previous `display()` path collapsed
                // `Int(1)` and `String("1")` into the same bucket — the
                // exact problem #2467 fixed for `count_by` / `group_by`.
                let mut counts: crate::value::DictMap = crate::value::DictMap::new();
                let mut error: Option<VmError> = None;
                for item in items.iter() {
                    let bucket = match item {
                        VmValue::String(s) => (**s).to_string(),
                        VmValue::Nil => {
                            error = Some(VmError::TypeError(
                                "tally: list contains nil; expected a list of strings (wrap with to_string(...) if you intended a scalar)"
                                    .to_string(),
                            ));
                            break;
                        }
                        other => {
                            error = Some(VmError::TypeError(format!(
                                "tally: list contains {}; expected a list of strings — wrap with to_string(...) so the bucket key is unambiguous",
                                other.type_name()
                            )));
                            break;
                        }
                    };
                    let current = counts.get(&bucket).and_then(|v| v.as_int()).unwrap_or(0);
                    counts.insert(bucket, VmValue::Int(current + 1));
                }
                if let Some(err) = error {
                    Err(err)
                } else {
                    Ok(VmValue::dict(counts))
                }
            }
            "to_list" => Ok(VmValue::List(Arc::clone(items))),
            "to_set" => Ok(VmValue::set(items.iter().cloned())),
            _ => Err(VmError::Runtime(format!("list has no method `{method}`"))),
        };
        Some(result)
    }

    pub(super) async fn call_list_method(
        &mut self,
        items: &Arc<Vec<VmValue>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(result) = Self::call_list_method_sync(items, method, args) {
            return result;
        }

        match method {
            "map" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items.iter() {
                    results.push(self.call_callable_one(callable, item).await?);
                }
                Ok(VmValue::List(std::sync::Arc::new(results)))
            }
            "filter" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        results.push(item.clone());
                    }
                }
                Ok(VmValue::List(std::sync::Arc::new(results)))
            }
            "reduce" => {
                let Some(callable) = args.get(1).filter(|v| Self::is_callable_value(v)).cloned()
                else {
                    return Ok(VmValue::Nil);
                };
                let mut acc = args.first().cloned().unwrap_or(VmValue::Nil);
                for item in items.iter() {
                    acc = self.call_callable_two(&callable, &acc, item).await?;
                }
                Ok(acc)
            }
            "find" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        return Ok(item.clone());
                    }
                }
                Ok(VmValue::Nil)
            }
            "any" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Bool(false));
                };
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        return Ok(VmValue::Bool(true));
                    }
                }
                Ok(VmValue::Bool(false))
            }
            "all" | "every" | "all?" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Bool(true));
                };
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if !result.is_truthy() {
                        return Ok(VmValue::Bool(false));
                    }
                }
                Ok(VmValue::Bool(true))
            }
            "flat_map" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if let VmValue::List(inner) = result {
                        results.extend(inner.iter().cloned());
                    } else {
                        results.push(result);
                    }
                }
                Ok(VmValue::List(std::sync::Arc::new(results)))
            }
            "sort_by" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut keyed: Vec<(VmValue, VmValue)> = Vec::new();
                for item in items.iter() {
                    let key = self.call_callable_one(callable, item).await?;
                    keyed.push((item.clone(), key));
                }
                keyed.sort_by(|(_, ka), (_, kb)| compare_values(ka, kb).cmp(&0));
                Ok(VmValue::List(std::sync::Arc::new(
                    keyed.into_iter().map(|(v, _)| v).collect(),
                )))
            }
            "none" | "none?" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Bool(items.is_empty()));
                };
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        return Ok(VmValue::Bool(false));
                    }
                }
                Ok(VmValue::Bool(true))
            }
            "find_index" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Int(-1));
                };
                for (i, item) in items.iter().enumerate() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        return Ok(VmValue::Int(i as i64));
                    }
                }
                Ok(VmValue::Int(-1))
            }
            "partition" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut truthy = Vec::new();
                let mut falsy = Vec::new();
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if result.is_truthy() {
                        truthy.push(item.clone());
                    } else {
                        falsy.push(item.clone());
                    }
                }
                Ok(VmValue::List(std::sync::Arc::new(vec![
                    VmValue::List(std::sync::Arc::new(truthy)),
                    VmValue::List(std::sync::Arc::new(falsy)),
                ])))
            }
            "group_by" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut groups: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
                for item in items.iter() {
                    let key = self.call_callable_one(callable, item).await?;
                    let key_str =
                        crate::stdlib::collections::string_discriminator(&key, "group_by")?;
                    groups.entry(key_str).or_default().push(item.clone());
                }
                let result: crate::value::DictMap = groups
                    .into_iter()
                    .map(|(k, v)| (k, VmValue::List(std::sync::Arc::new(v))))
                    .collect();
                Ok(VmValue::dict(result))
            }
            "min_by" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut best = items[0].clone();
                let mut best_key = self.call_callable_one(callable, &best).await?;
                for item in &items[1..] {
                    let key = self.call_callable_one(callable, item).await?;
                    if compare_values(&key, &best_key) < 0 {
                        best = item.clone();
                        best_key = key;
                    }
                }
                Ok(best)
            }
            "max_by" => {
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::Nil);
                };
                let mut best = items[0].clone();
                let mut best_key = self.call_callable_one(callable, &best).await?;
                for item in &items[1..] {
                    let key = self.call_callable_one(callable, item).await?;
                    if compare_values(&key, &best_key) > 0 {
                        best = item.clone();
                        best_key = key;
                    }
                }
                Ok(best)
            }
            "take_while" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::List(std::sync::Arc::new(
                        items.iter().cloned().collect(),
                    )));
                };
                let mut out = Vec::new();
                for item in items.iter() {
                    let result = self.call_callable_one(callable, item).await?;
                    if !result.is_truthy() {
                        break;
                    }
                    out.push(item.clone());
                }
                Ok(VmValue::List(std::sync::Arc::new(out)))
            }
            "drop_while" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::List(std::sync::Arc::new(
                        items.iter().cloned().collect(),
                    )));
                };
                let mut out = Vec::new();
                let mut dropping = true;
                for item in items.iter() {
                    if dropping {
                        let result = self.call_callable_one(callable, item).await?;
                        if result.is_truthy() {
                            continue;
                        }
                        dropping = false;
                    }
                    out.push(item.clone());
                }
                Ok(VmValue::List(std::sync::Arc::new(out)))
            }
            "count_by" => {
                let Some(callable) = args.first().filter(|v| Self::is_callable_value(v)) else {
                    return Ok(VmValue::dict(BTreeMap::new()));
                };
                let mut counts: BTreeMap<String, i64> = BTreeMap::new();
                for item in items.iter() {
                    let key = self.call_callable_one(callable, item).await?;
                    let bucket =
                        crate::stdlib::collections::string_discriminator(&key, "count_by")?;
                    *counts.entry(bucket).or_insert(0) += 1;
                }
                Ok(VmValue::dict(
                    counts
                        .into_iter()
                        .map(|(k, v)| (k, VmValue::Int(v)))
                        .collect::<crate::value::DictMap>(),
                ))
            }
            _ => Err(VmError::Runtime(format!("list has no method `{method}`"))),
        }
    }
}
