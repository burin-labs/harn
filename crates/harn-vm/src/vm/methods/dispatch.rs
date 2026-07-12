use std::future::Future;
use std::pin::Pin;

use crate::value::{VmError, VmValue};
use crate::vm::iter::iter_from_value;

impl crate::vm::Vm {
    pub(in crate::vm) fn call_method_sync(
        obj: &VmValue,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if method == "iter"
            && matches!(
                obj,
                VmValue::List(_)
                    | VmValue::Set(_)
                    | VmValue::Dict(_)
                    | VmValue::String(_)
                    | VmValue::Generator(_)
                    | VmValue::Stream(_)
                    | VmValue::Channel(_)
                    | VmValue::Iter(_)
            )
        {
            return Some(iter_from_value(obj.clone()));
        }

        match obj {
            VmValue::String(s) => Some(Self::call_string_method(s, method, args)),
            VmValue::List(items) => Self::call_list_method_sync(items, method, args),
            VmValue::Dict(map) => Self::call_dict_method_sync(map, method, args),
            VmValue::Set(items) => Self::call_set_method_sync(items, method, args),
            VmValue::Int(_) | VmValue::Float(_) => {
                Some(Self::call_number_method(obj, method, args))
            }
            VmValue::Range(r) => Self::call_range_method_sync(obj, r, method, args),
            _ => None,
        }
    }

    pub(in crate::vm) fn call_method_async<'a>(
        &'a mut self,
        obj: VmValue,
        method: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(result) = Self::call_method_sync(&obj, method, args) {
                return result;
            }

            match &obj {
                VmValue::List(items) => self.call_list_method(items, method, args).await,
                VmValue::Dict(map) => self.call_dict_method(map, method, args).await,
                VmValue::Set(items) => self.call_set_method(items, method, args).await,
                VmValue::Range(r) => self.call_range_method(&obj, r, method, args).await,
                VmValue::StructInstance(_) => {
                    self.call_struct_instance_method(&obj, method, args).await
                }
                VmValue::BuiltinRef(name) => {
                    let qualified = format!("{name}.{method}");
                    if self.builtins.contains_key(&qualified)
                        || self.async_builtins.contains_key(&qualified)
                    {
                        self.call_named_builtin(&qualified, args.to_vec()).await
                    } else {
                        Err(VmError::TypeError(format!(
                            "value of type {} has no method `{method}`",
                            obj.type_name()
                        )))
                    }
                }
                VmValue::BuiltinRefId(r) => {
                    let qualified = format!("{}.{method}", r.name);
                    if self.builtins.contains_key(&qualified)
                        || self.async_builtins.contains_key(&qualified)
                    {
                        self.call_named_builtin(&qualified, args.to_vec()).await
                    } else {
                        Err(VmError::TypeError(format!(
                            "value of type {} has no method `{method}`",
                            obj.type_name()
                        )))
                    }
                }
                VmValue::Generator(gen) => self.call_generator_method(gen, method).await,
                VmValue::Stream(stream) => self.call_stream_method(stream, method).await,
                VmValue::Iter(handle) => self.call_iter_method(handle, method, args).await,
                VmValue::Harness(handle) => {
                    let handle = handle.clone();
                    self.call_harness_method(&handle, method, args).await
                }
                other => Err(VmError::TypeError(format!(
                    "value of type {} has no method `{method}`",
                    other.type_name()
                ))),
            }
        })
    }
}

#[cfg(test)]
mod drift_guard_tests {
    //! Keep the typechecker's builtin-method registry
    //! (`harn_parser::typechecker::method_registry`) in sync with the VM
    //! dispatch here. If a method name in a registry list is *not* accepted by
    //! the corresponding VM dispatch, the typechecker would wrongly reject a
    //! call the runtime supports — a false positive. These tests fail when
    //! that drift is introduced (e.g. a VM method is renamed without updating
    //! the registry).
    //!
    //! A `None` from a `*_sync` dispatcher means "handled by the async path",
    //! which is still a valid method, so only an explicit `has no method`
    //! error counts as rejection.
    use crate::value::VmValue;
    use harn_parser::typechecker::method_registry as reg;
    use std::sync::Arc;

    fn sample_args() -> Vec<VmValue> {
        // Enough positional args (2 strings) to satisfy guarded arms such as
        // `"replace" if args.len() >= 2`.
        vec![VmValue::string("a"), VmValue::string("b"), VmValue::Int(1)]
    }

    fn is_no_method_rejection(result: Option<Result<VmValue, crate::value::VmError>>) -> bool {
        matches!(result, Some(Err(e)) if format!("{e:?}").contains("has no method"))
    }

    #[test]
    fn string_registry_matches_vm_dispatch() {
        let s = arcstr::ArcStr::from("hello");
        let args = sample_args();
        for &name in reg::STRING_METHODS {
            if name == "iter" {
                continue; // dispatched in call_method_sync, not call_string_method
            }
            let res = crate::vm::Vm::call_string_method(&s, name, &args);
            assert!(
                !format!("{res:?}").contains("has no method"),
                "STRING_METHODS lists `{name}` but the VM rejects it"
            );
        }
        // Sanity: a name absent from the registry is genuinely rejected.
        let bogus = crate::vm::Vm::call_string_method(&s, "frobnicate", &args);
        assert!(format!("{bogus:?}").contains("has no method"));
    }

    #[test]
    fn list_registry_matches_vm_dispatch() {
        let items: Arc<Vec<VmValue>> = Arc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
        let args = sample_args();
        for &name in reg::LIST_METHODS {
            if name == "iter" {
                continue;
            }
            let res = crate::vm::Vm::call_list_method_sync(&items, name, &args);
            assert!(
                !is_no_method_rejection(res),
                "LIST_METHODS lists `{name}` but the VM rejects it"
            );
        }
    }

    #[test]
    fn set_registry_matches_vm_dispatch() {
        let VmValue::Set(set) = VmValue::set([VmValue::Int(1), VmValue::Int(2)]) else {
            unreachable!("VmValue::set builds a Set");
        };
        let args = vec![VmValue::set([VmValue::Int(3)]), VmValue::string("a")];
        for &name in reg::SET_METHODS {
            if name == "iter" {
                continue;
            }
            let res = crate::vm::Vm::call_set_method_sync(&set, name, &args);
            assert!(
                !is_no_method_rejection(res),
                "SET_METHODS lists `{name}` but the VM rejects it"
            );
        }
    }

    #[test]
    fn dict_registry_matches_vm_dispatch() {
        let VmValue::Dict(map) = VmValue::dict([("k", VmValue::Int(1))]) else {
            unreachable!("VmValue::dict builds a Dict");
        };
        let args = sample_args();
        for &name in reg::DICT_METHODS {
            if name == "iter" {
                continue;
            }
            let res = crate::vm::Vm::call_dict_method_sync(&map, name, &args);
            assert!(
                !is_no_method_rejection(res),
                "DICT_METHODS lists `{name}` but the VM rejects it"
            );
        }
    }
}
