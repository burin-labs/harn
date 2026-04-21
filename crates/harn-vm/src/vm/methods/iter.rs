use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::value::{compare_values, values_equal, VmError, VmValue};
use crate::vm::iter::{drain, iter_from_value, next_handle, VmIter};

impl crate::vm::Vm {
    pub(super) async fn call_iter_method(
        &mut self,
        handle: &Rc<RefCell<VmIter>>,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let handle = Rc::clone(handle);
        match method {
            "map" => {
                let f = args
                    .first()
                    .filter(|v| Self::is_callable_value(v))
                    .cloned()
                    .ok_or_else(|| VmError::TypeError("iter.map: expected callable".to_string()))?;
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
                        VmError::TypeError("iter.flat_map: expected callable".to_string())
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
                        VmError::TypeError("iter.take_while: expected callable".to_string())
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
                        VmError::TypeError("iter.skip_while: expected callable".to_string())
                    })?;
                Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::SkipWhile {
                    inner: handle,
                    p,
                    primed: false,
                }))))
            }
            "zip" => {
                let other = args.first().cloned().ok_or_else(|| {
                    VmError::TypeError("iter.zip: expected iterable argument".to_string())
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
            "enumerate" => Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Enumerate {
                inner: handle,
                i: 0,
            })))),
            "chain" => {
                let other = args.first().cloned().ok_or_else(|| {
                    VmError::TypeError("iter.chain: expected iterable argument".to_string())
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
                let items = drain(&handle, self).await?;
                Ok(VmValue::List(Rc::new(items)))
            }
            "to_set" => {
                let items = drain(&handle, self).await?;
                let mut out: Vec<VmValue> = Vec::new();
                for v in items {
                    if !out.iter().any(|x| values_equal(x, &v)) {
                        out.push(v);
                    }
                }
                Ok(VmValue::Set(Rc::new(out)))
            }
            "to_dict" => {
                let items = drain(&handle, self).await?;
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
                    let v = next_handle(&handle, self).await?;
                    if v.is_none() {
                        break;
                    }
                    n += 1;
                }
                Ok(VmValue::Int(n))
            }
            "sum" => {
                let items = drain(&handle, self).await?;
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
                let items = drain(&handle, self).await?;
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
                let items = drain(&handle, self).await?;
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
                    let item = next_handle(&handle, self).await?;
                    match item {
                        None => return Ok(acc),
                        Some(v) => {
                            acc = self.call_callable_value(&f, &[acc, v]).await?;
                        }
                    }
                }
            }
            "first" => {
                let v = next_handle(&handle, self).await?;
                Ok(v.unwrap_or(VmValue::Nil))
            }
            "last" => {
                let mut last = VmValue::Nil;
                loop {
                    let v = next_handle(&handle, self).await?;
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
                    .ok_or_else(|| VmError::TypeError("iter.any: expected callable".to_string()))?;
                loop {
                    let item = next_handle(&handle, self).await?;
                    match item {
                        None => return Ok(VmValue::Bool(false)),
                        Some(v) => {
                            let r = self.call_callable_value(&p, &[v]).await?;
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
                    .ok_or_else(|| VmError::TypeError("iter.all: expected callable".to_string()))?;
                loop {
                    let item = next_handle(&handle, self).await?;
                    match item {
                        None => return Ok(VmValue::Bool(true)),
                        Some(v) => {
                            let r = self.call_callable_value(&p, &[v]).await?;
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
                    let item = next_handle(&handle, self).await?;
                    match item {
                        None => return Ok(VmValue::Nil),
                        Some(v) => {
                            let r = self.call_callable_value(&p, &[v.clone()]).await?;
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
                        VmError::TypeError("iter.for_each: expected callable".to_string())
                    })?;
                loop {
                    let item = next_handle(&handle, self).await?;
                    match item {
                        None => return Ok(VmValue::Nil),
                        Some(v) => {
                            self.call_callable_value(&f, &[v]).await?;
                        }
                    }
                }
            }
            _ => Ok(VmValue::Nil),
        }
    }
}
