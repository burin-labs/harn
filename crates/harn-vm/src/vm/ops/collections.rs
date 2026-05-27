use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::{InlineCacheEntry, PropertyCacheTarget};
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    fn concat_display_values(parts: &[VmValue]) -> String {
        let all_strings_len = parts.iter().try_fold(0usize, |len, part| match part {
            VmValue::String(text) => Some(len + text.len()),
            _ => None,
        });
        let mut result = String::with_capacity(all_strings_len.unwrap_or(0));
        for part in parts {
            part.write_display(&mut result);
        }
        result
    }

    fn char_to_value(ch: char) -> VmValue {
        let mut buffer = [0; 4];
        VmValue::String(Rc::from(ch.encode_utf8(&mut buffer)))
    }

    fn index_from_end(index: i64) -> Option<usize> {
        index
            .checked_neg()?
            .checked_sub(1)
            .and_then(|offset| usize::try_from(offset).ok())
    }

    fn string_index(s: &str, index: i64) -> VmValue {
        if s.is_ascii() {
            let pos = if index < 0 {
                Self::index_from_end(index)
                    .and_then(|offset| offset.checked_add(1))
                    .and_then(|distance| s.len().checked_sub(distance))
            } else {
                usize::try_from(index).ok()
            };
            return pos
                .and_then(|pos| s.as_bytes().get(pos))
                .map(|byte| Self::char_to_value(*byte as char))
                .unwrap_or(VmValue::Nil);
        }

        if index < 0 {
            let Some(index) = Self::index_from_end(index) else {
                return VmValue::Nil;
            };
            return s
                .chars()
                .rev()
                .nth(index)
                .map(Self::char_to_value)
                .unwrap_or(VmValue::Nil);
        }
        s.chars()
            .nth(match usize::try_from(index) {
                Ok(index) => index,
                Err(_) => return VmValue::Nil,
            })
            .map(Self::char_to_value)
            .unwrap_or(VmValue::Nil)
    }

    fn try_cached_property(
        cache: Option<&(u16, PropertyCacheTarget)>,
        name_idx: u16,
        obj: &VmValue,
    ) -> Option<VmValue> {
        let (cached_name_idx, target) = cache?;
        if *cached_name_idx != name_idx {
            return None;
        }

        match (target, obj) {
            (PropertyCacheTarget::DictField(name), VmValue::Dict(map)) => {
                Some(map.get(name.as_ref()).cloned().unwrap_or(VmValue::Nil))
            }
            (
                PropertyCacheTarget::StructField { field_name, index },
                VmValue::StructInstance { layout, fields },
            ) => {
                if layout
                    .field_names()
                    .get(*index)
                    .is_some_and(|candidate| candidate.as_str() == field_name.as_ref())
                {
                    Some(
                        fields
                            .get(*index)
                            .and_then(Option::as_ref)
                            .cloned()
                            .unwrap_or(VmValue::Nil),
                    )
                } else {
                    None
                }
            }
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
            (PropertyCacheTarget::EnumVariant, VmValue::EnumVariant(enum_variant)) => {
                Some(VmValue::String(Rc::clone(&enum_variant.variant)))
            }
            (PropertyCacheTarget::EnumFields, VmValue::EnumVariant(enum_variant)) => {
                Some(VmValue::List(Rc::clone(&enum_variant.fields)))
            }
            _ => None,
        }
    }

    fn property_cache_target(obj: &VmValue, name: &str) -> Option<PropertyCacheTarget> {
        match obj {
            VmValue::Dict(_) => Some(PropertyCacheTarget::DictField(Rc::from(name))),
            VmValue::StructInstance { layout, .. } => {
                layout
                    .field_index(name)
                    .map(|index| PropertyCacheTarget::StructField {
                        field_name: Rc::from(name),
                        index,
                    })
            }
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
            VmValue::EnumVariant(_) => match name {
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
            VmValue::EnumVariant(enum_variant) => match name {
                "variant" => VmValue::String(Rc::clone(&enum_variant.variant)),
                "fields" => VmValue::List(Rc::clone(&enum_variant.fields)),
                _ => VmValue::Nil,
            },
            VmValue::StructInstance { layout, fields } => layout
                .field_index(name)
                .and_then(|index| fields.get(index))
                .and_then(Clone::clone)
                .unwrap_or(VmValue::Nil),
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
            VmValue::Harness(handle) => match handle.sub_handle(name) {
                Some(sub) => VmValue::harness(sub),
                None if optional => VmValue::Nil,
                None => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on {} — Harness exposes `stdio`, \
                         `clock`, `fs`, `env`, `random`, `net`, `system`",
                        handle.type_name(),
                    )));
                }
            },
            VmValue::Nil => {
                return Err(VmError::TypeError(format!(
                    "cannot access property `{name}` on nil — use `?.{name}` for optional access, or narrow with a `!= nil` guard before reading fields"
                )));
            }
            _ if optional => VmValue::Nil,
            _ => {
                return Err(VmError::TypeError(format!(
                    "cannot access property `{name}` on {} — only dicts, structs, lists, strings, and pairs support property access",
                    obj.type_name()
                )));
            }
        };
        Ok(result)
    }

    pub(super) fn execute_build_list(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let count = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let items = self.stack.split_off(self.stack.len().saturating_sub(count));
        self.stack.push(VmValue::List(Rc::new(items)));
    }

    pub(super) fn execute_build_dict(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let count = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let pairs = self
            .stack
            .split_off(self.stack.len().saturating_sub(count * 2));
        let mut map = BTreeMap::new();
        let mut pairs = pairs.into_iter();
        while let Some(key) = pairs.next() {
            if let Some(value) = pairs.next() {
                map.insert(key.display(), value);
            }
        }
        self.stack.push(VmValue::Dict(Rc::new(map)));
    }

    pub(super) fn execute_subscript(&mut self, optional: bool) -> Result<(), VmError> {
        let idx = self.pop()?;
        let obj = self.pop()?;
        if optional && matches!(obj, VmValue::Nil) {
            self.stack.push(VmValue::Nil);
            return Ok(());
        }
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
            (VmValue::Dict(map), VmValue::String(key)) => {
                map.get(key.as_ref()).cloned().unwrap_or(VmValue::Nil)
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
            (VmValue::String(s), VmValue::Int(i)) => Self::string_index(s, *i),
            _ => {
                return Err(VmError::TypeError(format!(
                    "cannot index into {} with {}",
                    obj.type_name(),
                    idx.type_name()
                )));
            }
        };
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn execute_slice(&mut self) -> Result<(), VmError> {
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
                )))
            }
        };
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn execute_get_property(&mut self, optional: bool) -> Result<(), VmError> {
        let (name_idx, cache_slot, cached_property) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            let cached_property = cache_slot.and_then(|slot| frame.chunk.peek_property_cache(slot));
            (name_idx, cache_slot, cached_property)
        };

        let obj = self.pop()?;
        if optional && matches!(obj, VmValue::Nil) {
            self.stack.push(VmValue::Nil);
        } else if let Some(result) =
            Self::try_cached_property(cached_property.as_ref(), name_idx, &obj)
        {
            self.stack.push(result);
        } else {
            let (result, target) = {
                let frame = self.frames.last().unwrap();
                let name = Self::const_str(&frame.chunk.constants[name_idx as usize])?;
                (
                    Self::resolve_property(&obj, name, optional)?,
                    Self::property_cache_target(&obj, name),
                )
            };

            if let (Some(slot), Some(target)) = (cache_slot, target) {
                let frame = self.frames.last().unwrap();
                frame
                    .chunk
                    .set_inline_cache_entry(slot, InlineCacheEntry::Property { name_idx, target });
            }
            self.stack.push(result);
        }
        Ok(())
    }

    pub(super) fn execute_set_property(&mut self) -> Result<(), VmError> {
        let (chunk, prop_idx, var_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let prop_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Rc::clone(&frame.chunk), prop_idx, var_idx)
        };
        let prop_name = Self::const_str(&chunk.constants[prop_idx])?;
        let var_name = Self::const_str(&chunk.constants[var_idx])?;
        let new_value = self.pop()?;
        if let Some(obj) = self
            .active_local_slot_value(var_name)
            .or_else(|| self.env.get(var_name))
        {
            let assign_value = |vm: &mut Self, value: VmValue| -> Result<(), VmError> {
                if !vm.assign_active_local_slot(var_name, value.clone(), false)? {
                    vm.env.assign(var_name, value)?;
                }
                Ok(())
            };
            match obj {
                VmValue::Dict(map) => {
                    let mut new_map = (*map).clone();
                    new_map.insert(prop_name.to_string(), new_value);
                    assign_value(self, VmValue::Dict(Rc::new(new_map)))?;
                }
                VmValue::StructInstance { .. } => {
                    let new_obj = obj
                        .struct_instance_with_property(prop_name, new_value)
                        .expect("struct instance matched above");
                    assign_value(self, new_obj)?;
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot set property `{prop_name}` on {}",
                        obj.type_name()
                    )));
                }
            }
        }
        Ok(())
    }

    pub(super) fn execute_set_subscript(&mut self) -> Result<(), VmError> {
        let (chunk, var_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Rc::clone(&frame.chunk), var_idx)
        };
        let var_name = Self::const_str(&chunk.constants[var_idx])?;
        let index = self.pop()?;
        let new_value = self.pop()?;

        // Fast path: when the binding is an active local slot, mutate the
        // contained dict/list in place via `Rc::make_mut`. This skips the
        // defensive `VmValue::clone` + collection clone the env-fallback
        // path has to pay, which is the per-iteration cost behind builder
        // loops like `out[k] = v` and `aliases[id] = …`.
        if let Some(slot_idx) = self.active_local_slot_index(var_name) {
            let frame = self.frames.last_mut().unwrap();
            if !frame.chunk.local_slots[slot_idx].mutable {
                return Err(VmError::ImmutableAssignment(var_name.to_string()));
            }
            let slot = &mut frame.local_slots[slot_idx];
            match &mut slot.value {
                VmValue::List(items) => {
                    if let Some(i) = index.as_int() {
                        let len = items.len();
                        let idx = if i < 0 {
                            (len as i64 + i).max(0) as usize
                        } else {
                            i as usize
                        };
                        if idx >= len {
                            return Err(VmError::Runtime(format!(
                                "Index {i} out of bounds for list of length {len}",
                            )));
                        }
                        Rc::make_mut(items)[idx] = new_value;
                        slot.synced = false;
                    }
                    return Ok(());
                }
                VmValue::Dict(map) => {
                    let key = index.display();
                    Rc::make_mut(map).insert(key, new_value);
                    slot.synced = false;
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }

        // Fallback: variable lives in `env` (e.g. captured by a closure or
        // declared in an outer scope). The env path still has to rebind
        // because `env.get` returns by value, but `Rc::try_unwrap` keeps
        // the no-other-references case allocation-free.
        if let Some(obj) = self.env.get(var_name) {
            match obj {
                VmValue::List(items) => {
                    if let Some(i) = index.as_int() {
                        let mut new_items =
                            Rc::try_unwrap(items).unwrap_or_else(|items| (*items).clone());
                        let idx = if i < 0 {
                            (new_items.len() as i64 + i).max(0) as usize
                        } else {
                            i as usize
                        };
                        if idx >= new_items.len() {
                            return Err(VmError::Runtime(format!(
                                "Index {} out of bounds for list of length {}",
                                i,
                                new_items.len()
                            )));
                        }
                        new_items[idx] = new_value;
                        self.env
                            .assign(var_name, VmValue::List(Rc::new(new_items)))?;
                    }
                }
                VmValue::Dict(map) => {
                    let key = index.display();
                    let mut new_map = Rc::try_unwrap(map).unwrap_or_else(|map| (*map).clone());
                    new_map.insert(key, new_value);
                    self.env.assign(var_name, VmValue::Dict(Rc::new(new_map)))?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn execute_concat(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let count = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let start = self.stack.len().saturating_sub(count);
        let result = Self::concat_display_values(&self.stack[start..]);
        self.stack.truncate(start);
        self.stack.push(VmValue::String(Rc::from(result)));
    }

    pub(super) fn execute_build_enum(&mut self) -> Result<(), VmError> {
        let (chunk, enum_idx, variant_idx, field_count) = {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let field_count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Rc::clone(&frame.chunk), enum_idx, variant_idx, field_count)
        };
        let enum_name = chunk
            .constant_string_rc(enum_idx)
            .ok_or_else(|| VmError::TypeError("expected string constant".into()))?;
        let variant = chunk
            .constant_string_rc(variant_idx)
            .ok_or_else(|| VmError::TypeError("expected string constant".into()))?;
        let fields = self
            .stack
            .split_off(self.stack.len().saturating_sub(field_count));
        self.stack
            .push(VmValue::enum_variant(enum_name, variant, fields));
        Ok(())
    }

    pub(super) fn execute_match_enum(&mut self) -> Result<(), VmError> {
        let (chunk, enum_idx, variant_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Rc::clone(&frame.chunk), enum_idx, variant_idx)
        };
        let enum_name = Self::const_str(&chunk.constants[enum_idx])?;
        let variant_name = Self::const_str(&chunk.constants[variant_idx])?;
        let val = self.pop()?;
        let matches = match &val {
            VmValue::EnumVariant(enum_variant) => enum_variant.is_variant(enum_name, variant_name),
            _ => false,
        };
        self.stack.push(val);
        self.stack.push(VmValue::Bool(matches));
        Ok(())
    }
}
