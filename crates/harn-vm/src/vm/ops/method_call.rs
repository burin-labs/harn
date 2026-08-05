use std::sync::Arc;

use crate::chunk::{InlineCacheEntry, MethodCacheTarget};
use crate::value::{string_char_count, values_equal, VmError, VmValue};

impl super::super::Vm {
    fn try_cached_method(
        cache: Option<(u16, usize, MethodCacheTarget)>,
        name_idx: u16,
        argc: usize,
        obj: &VmValue,
        args: &[VmValue],
    ) -> Option<VmValue> {
        let (cached_name_idx, cached_argc, target) = cache?;
        if cached_name_idx != name_idx || cached_argc != argc {
            return None;
        }

        match (target, obj) {
            (MethodCacheTarget::ListCount, VmValue::List(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (MethodCacheTarget::ListEmpty, VmValue::List(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            (MethodCacheTarget::ListContains, VmValue::List(items)) => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Some(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
            }
            (MethodCacheTarget::StringCount, VmValue::String(s)) => {
                Some(VmValue::Int(string_char_count(s) as i64))
            }
            (MethodCacheTarget::StringEmpty, VmValue::String(s)) => {
                Some(VmValue::Bool(s.is_empty()))
            }
            (MethodCacheTarget::StringContains, VmValue::String(s)) => Some(VmValue::Bool(
                s.contains(&*args.first().map(|arg| arg.display()).unwrap_or_default()),
            )),
            (MethodCacheTarget::DictCount, VmValue::Dict(map)) => {
                Some(VmValue::Int(map.len() as i64))
            }
            (MethodCacheTarget::DictHas, VmValue::Dict(map)) => {
                let key = args.first().map(|arg| arg.display()).unwrap_or_default();
                Some(VmValue::Bool(map.contains_key(key.as_str())))
            }
            (MethodCacheTarget::RangeCount | MethodCacheTarget::RangeLen, VmValue::Range(r)) => {
                Some(VmValue::Int(r.len()))
            }
            (MethodCacheTarget::RangeEmpty, VmValue::Range(r)) => Some(VmValue::Bool(r.is_empty())),
            (MethodCacheTarget::RangeFirst, VmValue::Range(r)) => {
                Some(r.first().map(VmValue::Int).unwrap_or(VmValue::Nil))
            }
            (MethodCacheTarget::RangeLast, VmValue::Range(r)) => {
                Some(r.last().map(VmValue::Int).unwrap_or(VmValue::Nil))
            }
            (MethodCacheTarget::SetCount | MethodCacheTarget::SetLen, VmValue::Set(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (MethodCacheTarget::SetEmpty, VmValue::Set(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            (MethodCacheTarget::SetContains, VmValue::Set(items)) => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Some(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
            }
            _ => None,
        }
    }

    fn try_cached_harness_method(
        cache: Option<(u16, usize, MethodCacheTarget)>,
        name_idx: u16,
        argc: usize,
        obj: &VmValue,
    ) -> Option<Arc<crate::harness::VmHarness>> {
        let (cached_name_idx, cached_argc, target) = cache?;
        if cached_name_idx != name_idx || cached_argc != argc {
            return None;
        }
        let MethodCacheTarget::Harness(kind) = target else {
            return None;
        };
        let VmValue::Harness(handle) = obj else {
            return None;
        };
        if handle.kind() == kind {
            Some(Arc::clone(handle))
        } else {
            None
        }
    }

    fn try_harness_method_sync_fast(
        output: &mut String,
        runtime_effects: &mut crate::orchestration::RuntimeEffectState,
        obj: &VmValue,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let VmValue::Harness(handle) = obj else {
            return None;
        };
        Self::call_harness_method_sync_fast(output, runtime_effects, handle, method, args)
    }

    fn method_cache_target(obj: &VmValue, method: &str, argc: usize) -> Option<MethodCacheTarget> {
        match obj {
            VmValue::Harness(handle) => Self::harness_sync_method_cache_target(handle, method),
            VmValue::List(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::ListCount),
                ("empty", 0) => Some(MethodCacheTarget::ListEmpty),
                ("contains" | "includes", 1) => Some(MethodCacheTarget::ListContains),
                _ => None,
            },
            VmValue::String(_) => match (method, argc) {
                ("count" | "len", 0) => Some(MethodCacheTarget::StringCount),
                ("empty", 0) => Some(MethodCacheTarget::StringEmpty),
                ("contains" | "includes", 1) => Some(MethodCacheTarget::StringContains),
                _ => None,
            },
            VmValue::Dict(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::DictCount),
                ("has", 1) => Some(MethodCacheTarget::DictHas),
                _ => None,
            },
            VmValue::Range(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::RangeCount),
                ("len", 0) => Some(MethodCacheTarget::RangeLen),
                ("empty", 0) => Some(MethodCacheTarget::RangeEmpty),
                ("first", 0) => Some(MethodCacheTarget::RangeFirst),
                ("last", 0) => Some(MethodCacheTarget::RangeLast),
                _ => None,
            },
            VmValue::Set(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::SetCount),
                ("len", 0) => Some(MethodCacheTarget::SetLen),
                ("empty", 0) => Some(MethodCacheTarget::SetEmpty),
                ("contains" | "includes", 1) => Some(MethodCacheTarget::SetContains),
                _ => None,
            },
            _ => None,
        }
    }

    fn harness_sync_method_cache_target(
        handle: &crate::harness::VmHarness,
        method: &str,
    ) -> Option<MethodCacheTarget> {
        let cacheable = match handle.kind() {
            crate::harness::HarnessKind::Stdio => matches!(
                method,
                "print" | "println" | "eprint" | "eprintln" | "read_line" | "prompt"
            ),
            crate::harness::HarnessKind::Term => {
                matches!(method, "width" | "height" | "is_tty" | "read_password")
            }
            crate::harness::HarnessKind::Clock => {
                matches!(
                    method,
                    "now_ms" | "timestamp" | "monotonic_ms" | "elapsed" | "date_iso"
                )
            }
            crate::harness::HarnessKind::Env => matches!(method, "get" | "get_or"),
            crate::harness::HarnessKind::Random => matches!(
                method,
                "f64" | "u64" | "range" | "choice" | "shuffle" | "uuid" | "uuid_v7"
            ),
            crate::harness::HarnessKind::Tenant => matches!(method, "id" | "try_id"),
            crate::harness::HarnessKind::Auth => matches!(
                method,
                "is_authenticated"
                    | "subject"
                    | "try_subject"
                    | "scheme"
                    | "try_scheme"
                    | "kind"
                    | "scopes"
                    | "has_scope"
            ),
            crate::harness::HarnessKind::Root
            | crate::harness::HarnessKind::Fs
            | crate::harness::HarnessKind::Net
            | crate::harness::HarnessKind::Process
            | crate::harness::HarnessKind::System
            | crate::harness::HarnessKind::Secrets
            | crate::harness::HarnessKind::Llm
            | crate::harness::HarnessKind::Obs
            | crate::harness::HarnessKind::Verdict => false,
            _ => false,
        };
        cacheable.then_some(MethodCacheTarget::Harness(handle.kind()))
    }

    pub(super) async fn execute_method_call(&mut self, optional: bool) -> Result<(), VmError> {
        let (chunk, cache_site, name_idx, argc) = {
            let frame = self.frames.last_mut().unwrap();
            let chunk = Arc::clone(&frame.chunk);
            let cache_site = frame.inline_cache_site_for_previous_op();
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            (chunk, cache_site, name_idx, argc)
        };
        let cached_method = cache_site
            .slot
            .and_then(|slot| self.peek_method_cache_by_index(cache_site.cache_set, slot));
        let args_start = self.stack_arg_start(argc)?;
        let obj_idx = args_start
            .checked_sub(1)
            .ok_or_else(|| VmError::Runtime("method receiver stack underflow".to_string()))?;
        let obj = self
            .stack
            .get(obj_idx)
            .cloned()
            .ok_or_else(|| VmError::Runtime("method receiver stack underflow".to_string()))?;
        if optional && matches!(obj, VmValue::Nil) {
            self.stack.truncate(obj_idx);
            self.stack.push(VmValue::Nil);
        } else if let Some(result) = Self::try_cached_method(
            cached_method,
            name_idx,
            argc,
            &obj,
            &self.stack[args_start..],
        ) {
            self.stack.truncate(obj_idx);
            self.stack.push(result);
        } else if let Some(handle) =
            Self::try_cached_harness_method(cached_method, name_idx, argc, &obj)
        {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(obj_idx);
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::call_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    &handle,
                    method,
                    &args,
                )
            };
            let result = if let Some(result) = sync_result {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            self.stack.push(result);
        } else {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let cache_target = Self::method_cache_target(&obj, method, argc);
            let args = &self.stack[args_start..];
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::try_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    &obj,
                    method,
                    args,
                )
            };
            let result = if let Some(result) = sync_result {
                self.stack.truncate(obj_idx);
                result?
            } else if let Some(result) = Self::call_method_sync(&obj, method, args) {
                self.stack.truncate(obj_idx);
                result?
            } else {
                let args = self.take_stack_args_from(args_start)?;
                self.stack.truncate(obj_idx);
                self.call_method_async(obj, method, &args).await?
            };
            if let (Some(slot), Some(target)) = (cache_site.slot, cache_target) {
                self.set_inline_cache_entry_by_index(
                    cache_site.cache_set,
                    cache_site.slot_count,
                    slot,
                    InlineCacheEntry::Method {
                        name_idx,
                        argc,
                        target,
                    },
                );
            }
            self.stack.push(result);
        }
        Ok(())
    }

    /// Completes method calls that do not need to suspend. Returning `None`
    /// leaves the frame and operand stack untouched for the async fallback.
    pub(super) fn execute_method_call_sync(
        &mut self,
        optional: bool,
    ) -> Option<Result<(), VmError>> {
        let (chunk, cache_site, name_idx, argc) = {
            let frame = self.frames.last().unwrap();
            let chunk = Arc::clone(&frame.chunk);
            let cache_site = frame.inline_cache_site_for_previous_op();
            let name_idx = frame.chunk.read_u16(frame.ip);
            let argc = frame.chunk.code[frame.ip + 2] as usize;
            (chunk, cache_site, name_idx, argc)
        };
        let cached_method = cache_site
            .slot
            .and_then(|slot| self.peek_method_cache_by_index(cache_site.cache_set, slot));

        let args_start = match self.stack.len().checked_sub(argc) {
            Some(args_start) => args_start,
            None => return Some(Err(VmError::StackUnderflow)),
        };
        let obj_idx = match args_start.checked_sub(1) {
            Some(obj_idx) => obj_idx,
            None => return Some(Err(VmError::StackUnderflow)),
        };
        let obj = &self.stack[obj_idx];

        if optional && matches!(obj, VmValue::Nil) {
            return Some(self.finish_method_call_sync(
                cache_site.cache_set,
                cache_site.slot_count,
                argc,
                Ok(VmValue::Nil),
                name_idx,
                None,
                None,
            ));
        }

        if let Some(result) = Self::try_cached_method(
            cached_method,
            name_idx,
            argc,
            obj,
            &self.stack[args_start..],
        ) {
            return Some(self.finish_method_call_sync(
                cache_site.cache_set,
                cache_site.slot_count,
                argc,
                Ok(result),
                name_idx,
                None,
                None,
            ));
        }

        if let Some(handle) = Self::try_cached_harness_method(cached_method, name_idx, argc, obj) {
            let method = match Self::const_str(&chunk.constants[name_idx as usize]) {
                Ok(method) => method,
                Err(err) => {
                    return Some(self.finish_method_call_sync(
                        cache_site.cache_set,
                        cache_site.slot_count,
                        argc,
                        Err(err),
                        name_idx,
                        None,
                        None,
                    ))
                }
            };
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::call_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    &handle,
                    method,
                    &self.stack[args_start..],
                )
            };
            if let Some(result) = sync_result {
                return Some(self.finish_method_call_sync(
                    cache_site.cache_set,
                    cache_site.slot_count,
                    argc,
                    result,
                    name_idx,
                    None,
                    None,
                ));
            }
        }

        let (result, cache_target) = {
            let method = match Self::const_str(&chunk.constants[name_idx as usize]) {
                Ok(method) => method,
                Err(err) => {
                    return Some(self.finish_method_call_sync(
                        cache_site.cache_set,
                        cache_site.slot_count,
                        argc,
                        Err(err),
                        name_idx,
                        None,
                        None,
                    ))
                }
            };
            let args = &self.stack[args_start..];
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::try_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    obj,
                    method,
                    args,
                )
            };
            let result = if let Some(result) = sync_result {
                result
            } else {
                Self::call_method_sync(obj, method, args)?
            };
            let cache_target = Self::method_cache_target(obj, method, argc);
            (result, cache_target)
        };

        Some(self.finish_method_call_sync(
            cache_site.cache_set,
            cache_site.slot_count,
            argc,
            result,
            name_idx,
            cache_site.slot,
            cache_target,
        ))
    }

    fn finish_method_call_sync(
        &mut self,
        cache_set: usize,
        slot_count: usize,
        argc: usize,
        result: Result<VmValue, VmError>,
        name_idx: u16,
        cache_slot: Option<usize>,
        cache_target: Option<MethodCacheTarget>,
    ) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        frame.ip += 3;

        let obj_idx = self
            .stack
            .len()
            .checked_sub(argc + 1)
            .ok_or(VmError::StackUnderflow)?;
        self.stack.truncate(obj_idx);

        let result = result?;
        if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
            self.set_inline_cache_entry_by_index(
                cache_set,
                slot_count,
                slot,
                InlineCacheEntry::Method {
                    name_idx,
                    argc,
                    target,
                },
            );
        }
        self.stack.push(result);
        Ok(())
    }

    pub(super) async fn execute_method_call_spread(&mut self) -> Result<(), VmError> {
        let (chunk, cache_site, name_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let chunk = Arc::clone(&frame.chunk);
            let cache_site = frame.inline_cache_site_for_previous_op();
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            (chunk, cache_site, name_idx)
        };
        let cached_method = cache_site
            .slot
            .and_then(|slot| self.peek_method_cache_by_index(cache_site.cache_set, slot));
        let args_val = self.pop()?;
        let obj = self.pop()?;
        let args = match args_val {
            VmValue::List(items) => (*items).clone(),
            _ => {
                return Err(VmError::TypeError(
                    "spread method call requires list arguments".into(),
                ))
            }
        };
        if let Some(result) =
            Self::try_cached_method(cached_method, name_idx, args.len(), &obj, &args)
        {
            self.stack.push(result);
        } else if let Some(handle) =
            Self::try_cached_harness_method(cached_method, name_idx, args.len(), &obj)
        {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::call_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    &handle,
                    method,
                    &args,
                )
            };
            let result = if let Some(result) = sync_result {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            self.stack.push(result);
        } else {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let cache_target = Self::method_cache_target(&obj, method, args.len());
            let sync_result = {
                let _interrupt = self.sync_builtin_interrupt_guard();
                Self::try_harness_method_sync_fast(
                    &mut self.output,
                    &mut self.runtime_effects,
                    &obj,
                    method,
                    &args,
                )
            };
            let result = if let Some(result) = sync_result {
                result?
            } else if let Some(result) = Self::call_method_sync(&obj, method, &args) {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            if let (Some(slot), Some(target)) = (cache_site.slot, cache_target) {
                self.set_inline_cache_entry_by_index(
                    cache_site.cache_set,
                    cache_site.slot_count,
                    slot,
                    InlineCacheEntry::Method {
                        name_idx,
                        argc: args.len(),
                        target,
                    },
                );
            }
            self.stack.push(result);
        }
        Ok(())
    }
}
