use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::{InlineCacheEntry, Op, PropertyCacheTarget};
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    fn try_cached_property(
        cache: &InlineCacheEntry,
        name_idx: u16,
        obj: &VmValue,
    ) -> Option<VmValue> {
        let InlineCacheEntry::Property {
            name_idx: cached_name_idx,
            target,
        } = cache
        else {
            return None;
        };
        if *cached_name_idx != name_idx {
            return None;
        }

        match (target, obj) {
            (PropertyCacheTarget::ListCount, VmValue::List(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (PropertyCacheTarget::ListEmpty, VmValue::List(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            (PropertyCacheTarget::ListFirst, VmValue::List(items)) => {
                Some(items.first().cloned().unwrap_or(VmValue::Nil))
            }
            (PropertyCacheTarget::ListLast, VmValue::List(items)) => {
                Some(items.last().cloned().unwrap_or(VmValue::Nil))
            }
            (PropertyCacheTarget::StringCount, VmValue::String(s)) => {
                Some(VmValue::Int(s.chars().count() as i64))
            }
            (PropertyCacheTarget::StringEmpty, VmValue::String(s)) => {
                Some(VmValue::Bool(s.is_empty()))
            }
            (PropertyCacheTarget::PairFirst, VmValue::Pair(p)) => Some(p.0.clone()),
            (PropertyCacheTarget::PairSecond, VmValue::Pair(p)) => Some(p.1.clone()),
            (PropertyCacheTarget::EnumVariant, VmValue::EnumVariant { variant, .. }) => {
                Some(VmValue::String(Rc::from(variant.as_str())))
            }
            (PropertyCacheTarget::EnumFields, VmValue::EnumVariant { fields, .. }) => {
                Some(VmValue::List(Rc::new(fields.clone())))
            }
            _ => None,
        }
    }

    fn property_cache_target(obj: &VmValue, name: &str) -> Option<PropertyCacheTarget> {
        match obj {
            VmValue::List(_) => match name {
                "count" => Some(PropertyCacheTarget::ListCount),
                "empty" => Some(PropertyCacheTarget::ListEmpty),
                "first" => Some(PropertyCacheTarget::ListFirst),
                "last" => Some(PropertyCacheTarget::ListLast),
                _ => None,
            },
            VmValue::String(_) => match name {
                "count" => Some(PropertyCacheTarget::StringCount),
                "empty" => Some(PropertyCacheTarget::StringEmpty),
                _ => None,
            },
            VmValue::Pair(_) => match name {
                "first" => Some(PropertyCacheTarget::PairFirst),
                "second" => Some(PropertyCacheTarget::PairSecond),
                _ => None,
            },
            VmValue::EnumVariant { .. } => match name {
                "variant" => Some(PropertyCacheTarget::EnumVariant),
                "fields" => Some(PropertyCacheTarget::EnumFields),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_property(obj: &VmValue, name: &str, optional: bool) -> Result<VmValue, VmError> {
        let result = match obj {
            VmValue::Nil if optional => VmValue::Nil,
            VmValue::Dict(map) => map.get(name).cloned().unwrap_or(VmValue::Nil),
            VmValue::List(items) => match name {
                "count" => VmValue::Int(items.len() as i64),
                "empty" => VmValue::Bool(items.is_empty()),
                "first" => items.first().cloned().unwrap_or(VmValue::Nil),
                "last" => items.last().cloned().unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            },
            VmValue::String(s) => match name {
                "count" => VmValue::Int(s.chars().count() as i64),
                "empty" => VmValue::Bool(s.is_empty()),
                _ => VmValue::Nil,
            },
            VmValue::EnumVariant {
                variant, fields, ..
            } => match name {
                "variant" => VmValue::String(Rc::from(variant.as_str())),
                "fields" => VmValue::List(Rc::new(fields.clone())),
                _ => VmValue::Nil,
            },
            VmValue::StructInstance { fields, .. } => {
                fields.get(name).cloned().unwrap_or(VmValue::Nil)
            }
            VmValue::Pair(p) => match name {
                "first" => p.0.clone(),
                "second" => p.1.clone(),
                _ if optional => VmValue::Nil,
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on pair (expected `first` or `second`)"
                    )));
                }
            },
            VmValue::Nil => {
                return Err(VmError::TypeError(format!(
                    "cannot access property `{name}` on nil"
                )));
            }
            _ if optional => VmValue::Nil,
            _ => {
                return Err(VmError::TypeError(format!(
                    "cannot access property `{name}` on {}",
                    obj.type_name()
                )));
            }
        };
        Ok(result)
    }

    pub(super) fn try_execute_collections_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::BuildList as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let items = self.stack.split_off(self.stack.len().saturating_sub(count));
            self.stack.push(VmValue::List(Rc::new(items)));
        } else if op == Op::BuildDict as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let pairs = self
                .stack
                .split_off(self.stack.len().saturating_sub(count * 2));
            let mut map = BTreeMap::new();
            for pair in pairs.chunks(2) {
                if pair.len() == 2 {
                    let key = pair[0].display();
                    map.insert(key, pair[1].clone());
                }
            }
            self.stack.push(VmValue::Dict(Rc::new(map)));
        } else if op == Op::Subscript as u8 {
            let idx = self.pop()?;
            let obj = self.pop()?;
            let result = match (&obj, &idx) {
                (VmValue::List(items), VmValue::Int(i)) => {
                    if *i < 0 {
                        let pos = items.len() as i64 + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            items.get(pos as usize).cloned().unwrap_or(VmValue::Nil)
                        }
                    } else {
                        items.get(*i as usize).cloned().unwrap_or(VmValue::Nil)
                    }
                }
                (VmValue::Dict(map), _) => map.get(&idx.display()).cloned().unwrap_or(VmValue::Nil),
                (VmValue::Range(r), VmValue::Int(i)) => {
                    let len = r.len();
                    let pos = if *i < 0 { len + *i } else { *i };
                    match r.get(pos) {
                        Some(v) => VmValue::Int(v),
                        None => {
                            return Err(VmError::Runtime(format!(
                                "range index out of range: index {i} for range of length {len}",
                            )));
                        }
                    }
                }
                (VmValue::String(s), VmValue::Int(i)) => {
                    if *i < 0 {
                        let count = s.chars().count() as i64;
                        let pos = count + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            s.chars()
                                .nth(pos as usize)
                                .map(|c| VmValue::String(Rc::from(c.to_string())))
                                .unwrap_or(VmValue::Nil)
                        }
                    } else {
                        s.chars()
                            .nth(*i as usize)
                            .map(|c| VmValue::String(Rc::from(c.to_string())))
                            .unwrap_or(VmValue::Nil)
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot index into {} with {}",
                        obj.type_name(),
                        idx.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::Slice as u8 {
            let end_val = self.pop()?;
            let start_val = self.pop()?;
            let obj = self.pop()?;

            let result = match &obj {
                VmValue::List(items) => {
                    let len = items.len() as i64;
                    let start = match &start_val {
                        VmValue::Nil => 0i64,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice start must be an integer, got {}",
                                start_val.type_name()
                            )));
                        }
                    };
                    let end = match &end_val {
                        VmValue::Nil => len,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice end must be an integer, got {}",
                                end_val.type_name()
                            )));
                        }
                    };
                    if start >= end {
                        VmValue::List(Rc::new(vec![]))
                    } else {
                        let sliced: Vec<VmValue> = items[start as usize..end as usize].to_vec();
                        VmValue::List(Rc::new(sliced))
                    }
                }
                VmValue::String(s) => {
                    let char_count = s.chars().count() as i64;
                    let start = match &start_val {
                        VmValue::Nil => 0i64,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (char_count + *i).max(0)
                            } else {
                                (*i).min(char_count)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice start must be an integer, got {}",
                                start_val.type_name()
                            )));
                        }
                    };
                    let end = match &end_val {
                        VmValue::Nil => char_count,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (char_count + *i).max(0)
                            } else {
                                (*i).min(char_count)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice end must be an integer, got {}",
                                end_val.type_name()
                            )));
                        }
                    };
                    if start >= end {
                        VmValue::String(Rc::from(""))
                    } else {
                        let start_idx = start as usize;
                        let end_idx = end as usize;
                        let byte_start = s
                            .char_indices()
                            .nth(start_idx)
                            .map(|(b, _)| b)
                            .unwrap_or(s.len());
                        let byte_end = s
                            .char_indices()
                            .nth(end_idx)
                            .map(|(b, _)| b)
                            .unwrap_or(s.len());
                        VmValue::String(Rc::from(&s[byte_start..byte_end]))
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot slice {}",
                        obj.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::GetProperty as u8 || op == Op::GetPropertyOpt as u8 {
            let optional = op == Op::GetPropertyOpt as u8;
            let (name_idx, cache_slot, cache_entry) = {
                let frame = self.frames.last_mut().unwrap();
                let op_offset = frame.ip.saturating_sub(1);
                let name_idx = frame.chunk.read_u16(frame.ip);
                frame.ip += 2;
                let cache_slot = frame.chunk.inline_cache_slot(op_offset);
                let cache_entry = cache_slot
                    .map(|slot| frame.chunk.inline_cache_entry(slot))
                    .unwrap_or(InlineCacheEntry::Empty);
                (name_idx, cache_slot, cache_entry)
            };

            let obj = self.pop()?;
            if optional && matches!(obj, VmValue::Nil) {
                self.stack.push(VmValue::Nil);
            } else if let Some(result) = Self::try_cached_property(&cache_entry, name_idx, &obj) {
                self.stack.push(result);
            } else {
                let name = {
                    let frame = self.frames.last().unwrap();
                    Self::const_string(&frame.chunk.constants[name_idx as usize])?
                };
                let result = Self::resolve_property(&obj, &name, optional)?;

                if let (Some(slot), Some(target)) =
                    (cache_slot, Self::property_cache_target(&obj, &name))
                {
                    let frame = self.frames.last().unwrap();
                    frame.chunk.set_inline_cache_entry(
                        slot,
                        InlineCacheEntry::Property { name_idx, target },
                    );
                }
                self.stack.push(result);
            }
        } else if op == Op::SetProperty as u8 {
            let frame = self.frames.last_mut().unwrap();
            let prop_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let prop_name = Self::const_string(&frame.chunk.constants[prop_idx])?;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::Dict(map) => {
                        let mut new_map = (*map).clone();
                        new_map.insert(prop_name, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    VmValue::StructInstance {
                        struct_name,
                        fields,
                    } => {
                        let mut new_fields = fields.clone();
                        new_fields.insert(prop_name, new_value);
                        self.env.assign(
                            &var_name,
                            VmValue::StructInstance {
                                struct_name,
                                fields: new_fields,
                            },
                        )?;
                    }
                    _ => {
                        return Err(VmError::TypeError(format!(
                            "cannot set property `{prop_name}` on {}",
                            obj.type_name()
                        )));
                    }
                }
            }
        } else if op == Op::SetSubscript as u8 {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let index = self.pop()?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::List(items) => {
                        if let Some(i) = index.as_int() {
                            let mut new_items = (*items).clone();
                            let idx = if i < 0 {
                                (new_items.len() as i64 + i).max(0) as usize
                            } else {
                                i as usize
                            };
                            if idx < new_items.len() {
                                new_items[idx] = new_value;
                                self.env
                                    .assign(&var_name, VmValue::List(Rc::new(new_items)))?;
                            } else {
                                return Err(VmError::Runtime(format!(
                                    "Index {} out of bounds for list of length {}",
                                    i,
                                    items.len()
                                )));
                            }
                        }
                    }
                    VmValue::Dict(map) => {
                        let key = index.display();
                        let mut new_map = (*map).clone();
                        new_map.insert(key, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    _ => {}
                }
            }
        } else if op == Op::Concat as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let parts = self.stack.split_off(self.stack.len().saturating_sub(count));
            let result: String = parts.iter().map(|p| p.display()).collect();
            self.stack.push(VmValue::String(Rc::from(result)));
        } else if op == Op::BuildEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let field_count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let fields = self
                .stack
                .split_off(self.stack.len().saturating_sub(field_count));
            self.stack.push(VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            });
        } else if op == Op::MatchEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant_name = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let val = self.pop()?;
            let matches = match &val {
                VmValue::EnumVariant {
                    enum_name: en,
                    variant: vn,
                    ..
                } => *en == enum_name && *vn == variant_name,
                _ => false,
            };
            self.stack.push(val);
            self.stack.push(VmValue::Bool(matches));
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
