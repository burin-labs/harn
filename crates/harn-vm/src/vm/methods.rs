use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use std::cell::RefCell;

use crate::chunk::CompiledFunction;
use crate::value::{compare_values, values_equal, VmError, VmValue};
use crate::vm::iter::{drain, iter_from_value, next_handle, VmIter};

impl super::Vm {
    pub(super) fn call_method<'a>(
        &'a mut self,
        obj: VmValue,
        method: &'a str,
        args: &'a [VmValue],
        functions: &'a [CompiledFunction],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            // Universal `.iter()` lift for any iterable source into VmValue::Iter.
            // Applied before type-specific method dispatch so every iterable
            // source gets the explicit lift.
            if method == "iter"
                && matches!(
                    &obj,
                    VmValue::List(_)
                        | VmValue::Set(_)
                        | VmValue::Dict(_)
                        | VmValue::String(_)
                        | VmValue::Generator(_)
                        | VmValue::Channel(_)
                        | VmValue::Iter(_)
                )
            {
                return iter_from_value(obj);
            }
            match &obj {
                VmValue::String(s) => match method {
                    "count" => Ok(VmValue::Int(s.chars().count() as i64)),
                    "empty" => Ok(VmValue::Bool(s.is_empty())),
                    "contains" => Ok(VmValue::Bool(
                        s.contains(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "replace" if args.len() >= 2 => Ok(VmValue::String(Rc::from(
                        s.replace(&args[0].display(), &args[1].display()),
                    ))),
                    "split" => {
                        let sep = args.first().map(|a| a.display()).unwrap_or(",".into());
                        Ok(VmValue::List(Rc::new(
                            s.split(&*sep)
                                .map(|p| VmValue::String(Rc::from(p)))
                                .collect(),
                        )))
                    }
                    "trim" => Ok(VmValue::String(Rc::from(s.trim()))),
                    "starts_with" => Ok(VmValue::Bool(
                        s.starts_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "ends_with" => Ok(VmValue::Bool(
                        s.ends_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "lowercase" => Ok(VmValue::String(Rc::from(s.to_lowercase()))),
                    "uppercase" => Ok(VmValue::String(Rc::from(s.to_uppercase()))),
                    "substring" => {
                        let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                        let len = s.chars().count() as i64;
                        let start = start.max(0).min(len) as usize;
                        let end =
                            args.get(1).and_then(|a| a.as_int()).unwrap_or(len).min(len) as usize;
                        let end = end.max(start);
                        let substr: String = s.chars().skip(start).take(end - start).collect();
                        Ok(VmValue::String(Rc::from(substr)))
                    }
                    "index_of" => {
                        let needle = args.first().map(|a| a.display()).unwrap_or_default();
                        // Return char offset, not byte offset
                        let idx = s
                            .find(&needle)
                            .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                        Ok(VmValue::Int(idx.unwrap_or(-1)))
                    }
                    "chars" => Ok(VmValue::List(Rc::new(
                        s.chars()
                            .map(|c| VmValue::String(Rc::from(c.to_string())))
                            .collect(),
                    ))),
                    "repeat" => {
                        let n = args.first().and_then(|a| a.as_int()).unwrap_or(1);
                        Ok(VmValue::String(Rc::from(s.repeat(n.max(0) as usize))))
                    }
                    "reverse" => Ok(VmValue::String(Rc::from(
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
                        let current_len = s.chars().count();
                        if current_len >= width {
                            Ok(VmValue::String(Rc::clone(s)))
                        } else {
                            let padding: String =
                                std::iter::repeat_n(pad_char, width - current_len).collect();
                            if left {
                                Ok(VmValue::String(Rc::from(format!("{padding}{s}"))))
                            } else {
                                Ok(VmValue::String(Rc::from(format!("{s}{padding}"))))
                            }
                        }
                    }
                    "trim_start" => Ok(VmValue::String(Rc::from(s.trim_start()))),
                    "trim_end" => Ok(VmValue::String(Rc::from(s.trim_end()))),
                    "lines" => Ok(VmValue::List(Rc::new(
                        s.lines().map(|l| VmValue::String(Rc::from(l))).collect(),
                    ))),
                    "char_at" => {
                        let idx = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                        let chars: Vec<char> = s.chars().collect();
                        if idx >= 0 && (idx as usize) < chars.len() {
                            Ok(VmValue::String(Rc::from(chars[idx as usize].to_string())))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "last_index_of" => {
                        let needle = args.first().map(|a| a.display()).unwrap_or_default();
                        let idx = s
                            .rfind(&needle)
                            .map(|byte_pos| s[..byte_pos].chars().count() as i64);
                        Ok(VmValue::Int(idx.unwrap_or(-1)))
                    }
                    "lower" | "to_lower" => {
                        Ok(VmValue::String(Rc::from(s.to_lowercase().as_str())))
                    }
                    "upper" | "to_upper" => {
                        Ok(VmValue::String(Rc::from(s.to_uppercase().as_str())))
                    }
                    "len" => Ok(VmValue::Int(s.chars().count() as i64)),
                    _ => Ok(VmValue::Nil),
                },
                VmValue::List(items) => match method {
                    "count" => Ok(VmValue::Int(items.len() as i64)),
                    "empty" => Ok(VmValue::Bool(items.is_empty())),
                    "map" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut results = Vec::with_capacity(items.len());
                            for item in items.iter() {
                                results.push(
                                    self.call_callable_value(callable, &[item.clone()], functions)
                                        .await?,
                                );
                            }
                            Ok(VmValue::List(Rc::new(results)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "filter" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut results = Vec::with_capacity(items.len());
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                                    .call_callable_value(callable, &[acc, item.clone()], functions)
                                    .await?;
                            }
                            return Ok(acc);
                        }
                        Ok(VmValue::Nil)
                    }
                    "find" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
                                if result.is_truthy() {
                                    return Ok(item.clone());
                                }
                            }
                        }
                        Ok(VmValue::Nil)
                    }
                    "any" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut results = Vec::with_capacity(items.len());
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut keyed: Vec<(VmValue, VmValue)> = Vec::new();
                            for item in items.iter() {
                                let key = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                    // --- Ruby-inspired additions ---
                    "none" | "none?" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
                                if result.is_truthy() {
                                    return Ok(VmValue::Bool(false));
                                }
                            }
                            Ok(VmValue::Bool(true))
                        } else {
                            // No predicate: check if list is empty
                            Ok(VmValue::Bool(items.is_empty()))
                        }
                    }
                    "find_index" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for (i, item) in items.iter().enumerate() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut truthy = Vec::new();
                            let mut falsy = Vec::new();
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut groups: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
                            for item in items.iter() {
                                let key = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        let size =
                            args.first().and_then(|a| a.as_int()).unwrap_or(1).max(1) as usize;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut best = items[0].clone();
                            let mut best_key = self
                                .call_callable_value(callable, &[best.clone()], functions)
                                .await?;
                            for item in &items[1..] {
                                let key = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut best = items[0].clone();
                            let mut best_key = self
                                .call_callable_value(callable, &[best.clone()], functions)
                                .await?;
                            for item in &items[1..] {
                                let key = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        let size =
                            args.first().and_then(|a| a.as_int()).unwrap_or(2).max(1) as usize;
                        if size > items.len() {
                            return Ok(VmValue::List(Rc::new(Vec::new())));
                        }
                        let windows: Vec<VmValue> = items
                            .windows(size)
                            .map(|w| VmValue::List(Rc::new(w.to_vec())))
                            .collect();
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
                },
                VmValue::Dict(map) => match method {
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut result = BTreeMap::new();
                            for (k, v) in map.iter() {
                                let mapped = self
                                    .call_callable_value(callable, &[v.clone()], functions)
                                    .await?;
                                result.insert(k.clone(), mapped);
                            }
                            Ok(VmValue::Dict(Rc::new(result)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "rekey" | "map_keys" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut result = BTreeMap::new();
                            for (k, v) in map.iter() {
                                let new_key = self
                                    .call_callable_value(
                                        callable,
                                        &[VmValue::String(Rc::from(k.as_str()))],
                                        functions,
                                    )
                                    .await?;
                                let new_key_str = new_key.display();
                                // Last write wins on key collision
                                result.insert(new_key_str, v.clone());
                            }
                            Ok(VmValue::Dict(Rc::new(result)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "filter" => {
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut result = BTreeMap::new();
                            for (k, v) in map.iter() {
                                let keep = self
                                    .call_callable_value(callable, &[v.clone()], functions)
                                    .await?;
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
                        if let Some(callable) =
                            map.get(method).filter(|v| Self::is_callable_value(v))
                        {
                            self.call_callable_value(callable, args, functions).await
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                },
                VmValue::Set(items) => match method {
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut result = Vec::new();
                            for item in items.iter() {
                                let mapped = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            let mut result = Vec::new();
                            for item in items.iter() {
                                let keep = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                        if let Some(callable) = args.first().filter(|v| Self::is_callable_value(v))
                        {
                            for item in items.iter() {
                                let result = self
                                    .call_callable_value(callable, &[item.clone()], functions)
                                    .await?;
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
                },
                VmValue::Range(r) => match method {
                    // O(1) core methods — no materialization.
                    "len" | "count" => Ok(VmValue::Int(r.len())),
                    "empty" => Ok(VmValue::Bool(r.is_empty())),
                    "contains" => {
                        let needle = args.first().unwrap_or(&VmValue::Nil);
                        let result = match needle {
                            VmValue::Int(n) => r.contains(*n),
                            _ => false,
                        };
                        Ok(VmValue::Bool(result))
                    }
                    "first" => Ok(r.first().map(VmValue::Int).unwrap_or(VmValue::Nil)),
                    "last" => Ok(r.last().map(VmValue::Int).unwrap_or(VmValue::Nil)),
                    "to_string" => Ok(VmValue::String(Rc::from(obj.display()))),
                    // Everything else routes through the unified lazy iter
                    // protocol: lift the Range into a VmValue::Iter (which
                    // preserves laziness via VmIter::Range) and delegate.
                    // `.map/.filter/.take/...` stay lazy; sinks like
                    // `.to_list/.sum/.reduce` materialize only on demand.
                    _ => {
                        let lifted = iter_from_value(obj.clone())?;
                        self.call_method(lifted, method, args, functions).await
                    }
                },
                VmValue::StructInstance { struct_name, .. } => {
                    // Look up __impl_TypeName in env for impl block methods
                    let impl_key = format!("__impl_{}", struct_name);
                    if let Some(VmValue::Dict(impl_dict)) = self.env.get(&impl_key) {
                        if let Some(VmValue::Closure(closure)) = impl_dict.get(method) {
                            // Call method with self (the struct) as first argument
                            let mut full_args = vec![obj.clone()];
                            full_args.extend_from_slice(args);
                            return self.call_closure(closure, &full_args, functions).await;
                        }
                    }
                    Ok(VmValue::Nil)
                }
                VmValue::Generator(gen) => match method {
                    "next" => {
                        if gen.done.get() {
                            let mut dict = BTreeMap::new();
                            dict.insert("value".to_string(), VmValue::Nil);
                            dict.insert("done".to_string(), VmValue::Bool(true));
                            Ok(VmValue::Dict(Rc::new(dict)))
                        } else {
                            let rx = gen.receiver.clone();
                            let mut guard = rx.lock().await;
                            match guard.recv().await {
                                Some(val) => {
                                    let mut dict = BTreeMap::new();
                                    dict.insert("done".to_string(), VmValue::Bool(false));
                                    dict.insert("value".to_string(), val);
                                    Ok(VmValue::Dict(Rc::new(dict)))
                                }
                                None => {
                                    gen.done.set(true);
                                    let mut dict = BTreeMap::new();
                                    dict.insert("value".to_string(), VmValue::Nil);
                                    dict.insert("done".to_string(), VmValue::Bool(true));
                                    Ok(VmValue::Dict(Rc::new(dict)))
                                }
                            }
                        }
                    }
                    _ => Ok(VmValue::Nil),
                },
                VmValue::Iter(handle) => {
                    let handle = Rc::clone(handle);
                    match method {
                        "map" => {
                            let f = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError("iter.map: expected callable".to_string())
                                })?;
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Map {
                                inner: handle,
                                f,
                            }))))
                        }
                        "filter" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError("iter.filter: expected callable".to_string())
                                })?;
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Filter {
                                inner: handle,
                                p,
                            }))))
                        }
                        "flat_map" => {
                            let f = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError(
                                        "iter.flat_map: expected callable".to_string(),
                                    )
                                })?;
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::FlatMap {
                                inner: handle,
                                f,
                                cur: None,
                            }))))
                        }
                        "take" => {
                            let n = match args.first() {
                                Some(VmValue::Int(i)) if *i >= 0 => *i as usize,
                                _ => {
                                    return Err(VmError::TypeError(
                                        "iter.take: expected non-negative int".to_string(),
                                    ))
                                }
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Take {
                                inner: handle,
                                remaining: n,
                            }))))
                        }
                        "skip" => {
                            let n = match args.first() {
                                Some(VmValue::Int(i)) if *i >= 0 => *i as usize,
                                _ => {
                                    return Err(VmError::TypeError(
                                        "iter.skip: expected non-negative int".to_string(),
                                    ))
                                }
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Skip {
                                inner: handle,
                                remaining: n,
                            }))))
                        }
                        "take_while" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError(
                                        "iter.take_while: expected callable".to_string(),
                                    )
                                })?;
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::TakeWhile {
                                inner: handle,
                                p,
                                done: false,
                            }))))
                        }
                        "skip_while" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError(
                                        "iter.skip_while: expected callable".to_string(),
                                    )
                                })?;
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::SkipWhile {
                                inner: handle,
                                p,
                                primed: false,
                            }))))
                        }
                        "zip" => {
                            let other = args.first().cloned().ok_or_else(|| {
                                VmError::TypeError(
                                    "iter.zip: expected iterable argument".to_string(),
                                )
                            })?;
                            let other_iter = iter_from_value(other)?;
                            let b_handle = match other_iter {
                                VmValue::Iter(h) => h,
                                _ => unreachable!("iter_from_value returns Iter"),
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Zip {
                                a: handle,
                                b: b_handle,
                            }))))
                        }
                        "enumerate" => {
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Enumerate {
                                inner: handle,
                                i: 0,
                            }))))
                        }
                        "chain" => {
                            let other = args.first().cloned().ok_or_else(|| {
                                VmError::TypeError(
                                    "iter.chain: expected iterable argument".to_string(),
                                )
                            })?;
                            let other_iter = iter_from_value(other)?;
                            let b_handle = match other_iter {
                                VmValue::Iter(h) => h,
                                _ => unreachable!("iter_from_value returns Iter"),
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Chain {
                                a: handle,
                                b: b_handle,
                                on_a: true,
                            }))))
                        }
                        "chunks" => {
                            let n = match args.first() {
                                Some(VmValue::Int(i)) if *i > 0 => *i as usize,
                                _ => {
                                    return Err(VmError::TypeError(
                                        "iter.chunks: chunk size must be positive".to_string(),
                                    ))
                                }
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Chunks {
                                inner: handle,
                                n,
                            }))))
                        }
                        "windows" => {
                            let n = match args.first() {
                                Some(VmValue::Int(i)) if *i > 0 => *i as usize,
                                _ => {
                                    return Err(VmError::TypeError(
                                        "iter.windows: window size must be positive".to_string(),
                                    ))
                                }
                            };
                            Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Windows {
                                inner: handle,
                                n,
                                buf: VecDeque::new(),
                            }))))
                        }
                        "to_list" => {
                            let items = drain(&handle, self, functions).await?;
                            Ok(VmValue::List(Rc::new(items)))
                        }
                        "to_set" => {
                            let items = drain(&handle, self, functions).await?;
                            let mut out: Vec<VmValue> = Vec::new();
                            for v in items {
                                if !out.iter().any(|x| values_equal(x, &v)) {
                                    out.push(v);
                                }
                            }
                            Ok(VmValue::Set(Rc::new(out)))
                        }
                        "to_dict" => {
                            let items = drain(&handle, self, functions).await?;
                            let mut map = BTreeMap::new();
                            for v in items {
                                match v {
                                    VmValue::Pair(pair) => {
                                        let (k, val) = (*pair).clone();
                                        let key = match k {
                                            VmValue::String(s) => s.to_string(),
                                            other => {
                                                return Err(VmError::TypeError(format!(
                                                    "iter.to_dict: expected string key, got {}",
                                                    other.type_name()
                                                )))
                                            }
                                        };
                                        map.insert(key, val);
                                    }
                                    other => {
                                        return Err(VmError::TypeError(format!(
                                            "iter.to_dict: expected pair, got {}",
                                            other.type_name()
                                        )))
                                    }
                                }
                            }
                            Ok(VmValue::Dict(Rc::new(map)))
                        }
                        "count" => {
                            let mut n: i64 = 0;
                            loop {
                                let v = next_handle(&handle, self, functions).await?;
                                if v.is_none() {
                                    break;
                                }
                                n += 1;
                            }
                            Ok(VmValue::Int(n))
                        }
                        "sum" => {
                            let items = drain(&handle, self, functions).await?;
                            let mut has_float = false;
                            let mut int_acc: i64 = 0;
                            let mut float_acc: f64 = 0.0;
                            for v in &items {
                                match v {
                                    VmValue::Int(i) => {
                                        int_acc += i;
                                        float_acc += *i as f64;
                                    }
                                    VmValue::Float(f) => {
                                        has_float = true;
                                        float_acc += f;
                                    }
                                    other => {
                                        return Err(VmError::TypeError(format!(
                                            "iter.sum: expected number, got {}",
                                            other.type_name()
                                        )))
                                    }
                                }
                            }
                            if has_float {
                                Ok(VmValue::Float(float_acc))
                            } else {
                                Ok(VmValue::Int(int_acc))
                            }
                        }
                        "min" => {
                            let items = drain(&handle, self, functions).await?;
                            let mut best: Option<VmValue> = None;
                            for v in items {
                                best = Some(match best {
                                    None => v,
                                    Some(cur) => {
                                        if compare_values(&v, &cur) < 0 {
                                            v
                                        } else {
                                            cur
                                        }
                                    }
                                });
                            }
                            Ok(best.unwrap_or(VmValue::Nil))
                        }
                        "max" => {
                            let items = drain(&handle, self, functions).await?;
                            let mut best: Option<VmValue> = None;
                            for v in items {
                                best = Some(match best {
                                    None => v,
                                    Some(cur) => {
                                        if compare_values(&v, &cur) > 0 {
                                            v
                                        } else {
                                            cur
                                        }
                                    }
                                });
                            }
                            Ok(best.unwrap_or(VmValue::Nil))
                        }
                        "reduce" => {
                            if args.len() < 2 {
                                return Err(VmError::TypeError(
                                    "iter.reduce: expected (init, fn)".to_string(),
                                ));
                            }
                            let mut acc = args[0].clone();
                            let f = args[1].clone();
                            if !Self::is_callable_value(&f) {
                                return Err(VmError::TypeError(
                                    "iter.reduce: second arg must be callable".to_string(),
                                ));
                            }
                            loop {
                                let item = next_handle(&handle, self, functions).await?;
                                match item {
                                    None => return Ok(acc),
                                    Some(v) => {
                                        acc = self
                                            .call_callable_value(&f, &[acc, v], functions)
                                            .await?;
                                    }
                                }
                            }
                        }
                        "first" => {
                            let v = next_handle(&handle, self, functions).await?;
                            Ok(v.unwrap_or(VmValue::Nil))
                        }
                        "last" => {
                            let mut last = VmValue::Nil;
                            loop {
                                let v = next_handle(&handle, self, functions).await?;
                                match v {
                                    Some(v) => last = v,
                                    None => return Ok(last),
                                }
                            }
                        }
                        "any" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError("iter.any: expected callable".to_string())
                                })?;
                            loop {
                                let item = next_handle(&handle, self, functions).await?;
                                match item {
                                    None => return Ok(VmValue::Bool(false)),
                                    Some(v) => {
                                        let r =
                                            self.call_callable_value(&p, &[v], functions).await?;
                                        if r.is_truthy() {
                                            return Ok(VmValue::Bool(true));
                                        }
                                    }
                                }
                            }
                        }
                        "all" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError("iter.all: expected callable".to_string())
                                })?;
                            loop {
                                let item = next_handle(&handle, self, functions).await?;
                                match item {
                                    None => return Ok(VmValue::Bool(true)),
                                    Some(v) => {
                                        let r =
                                            self.call_callable_value(&p, &[v], functions).await?;
                                        if !r.is_truthy() {
                                            return Ok(VmValue::Bool(false));
                                        }
                                    }
                                }
                            }
                        }
                        "find" => {
                            let p = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError("iter.find: expected callable".to_string())
                                })?;
                            loop {
                                let item = next_handle(&handle, self, functions).await?;
                                match item {
                                    None => return Ok(VmValue::Nil),
                                    Some(v) => {
                                        let r = self
                                            .call_callable_value(&p, &[v.clone()], functions)
                                            .await?;
                                        if r.is_truthy() {
                                            return Ok(v);
                                        }
                                    }
                                }
                            }
                        }
                        "for_each" => {
                            let f = args
                                .first()
                                .filter(|v| Self::is_callable_value(v))
                                .cloned()
                                .ok_or_else(|| {
                                    VmError::TypeError(
                                        "iter.for_each: expected callable".to_string(),
                                    )
                                })?;
                            loop {
                                let item = next_handle(&handle, self, functions).await?;
                                match item {
                                    None => return Ok(VmValue::Nil),
                                    Some(v) => {
                                        self.call_callable_value(&f, &[v], functions).await?;
                                    }
                                }
                            }
                        }
                        _ => Ok(VmValue::Nil),
                    }
                }
                _ => Ok(VmValue::Nil),
            }
        })
    }
}
