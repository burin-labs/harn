use crate::value::{VmError, VmValue};
use crate::BuiltinId;

use super::super::Vm;

impl Vm {
    pub(in crate::vm) async fn call_exact_value(
        &mut self,
        callable: VmValue,
        args: Vec<VmValue>,
    ) -> Result<(), VmError> {
        match callable {
            VmValue::Closure(closure) => self.call_user_closure(closure, args).await?,
            VmValue::Dict(registry) => {
                let closure =
                    crate::vm::tool_callable::require_single_harn_tool_handler(&registry, || {
                        format!("Cannot call {}", VmValue::Dict(registry.clone()).display())
                    })?;
                self.call_user_closure(closure, args).await?;
            }
            VmValue::BuiltinRef(name) => {
                if !self.try_call_special_name(&name, &args).await? {
                    let result = self.call_named_builtin(&name, args).await?;
                    self.stack.push(result);
                }
            }
            VmValue::BuiltinRefId(reference) => {
                if !self.try_call_special_name(&reference.name, &args).await? {
                    let result = self
                        .call_builtin_id_or_name(reference.id, &reference.name, args)
                        .await?;
                    self.stack.push(result);
                }
            }
            other => {
                return Err(VmError::TypeError(format!(
                    "Cannot call {}",
                    other.display()
                )))
            }
        }
        Ok(())
    }

    pub(in crate::vm) async fn call_exact_value_from_stack_args(
        &mut self,
        callable: VmValue,
        args_start: usize,
        stack_truncate_to: usize,
    ) -> Result<(), VmError> {
        match callable {
            VmValue::Closure(closure) => {
                self.call_user_closure_from_stack_args(closure, args_start, stack_truncate_to)
                    .await?;
            }
            VmValue::Dict(registry) => {
                let closure = match crate::vm::tool_callable::require_single_harn_tool_handler(
                    &registry,
                    || format!("Cannot call {}", VmValue::Dict(registry.clone()).display()),
                ) {
                    Ok(closure) => closure,
                    Err(error) => {
                        self.stack.truncate(stack_truncate_to);
                        return Err(error);
                    }
                };
                self.call_user_closure_from_stack_args(closure, args_start, stack_truncate_to)
                    .await?;
            }
            VmValue::BuiltinRef(name) => {
                if let Some(result) =
                    self.try_call_sync_builtin_id_or_name_from_stack_args(None, &name, args_start)
                {
                    self.stack.truncate(stack_truncate_to);
                    self.stack.push(result?);
                } else {
                    let args = self.take_stack_args_from(args_start)?;
                    self.stack.truncate(stack_truncate_to);
                    if !self.try_call_special_name(&name, &args).await? {
                        let result = self.call_named_builtin(&name, args).await?;
                        self.stack.push(result);
                    }
                }
            }
            VmValue::BuiltinRefId(reference) => {
                if let Some(result) = self.try_call_sync_builtin_id_or_name_from_stack_args(
                    Some(reference.id),
                    &reference.name,
                    args_start,
                ) {
                    self.stack.truncate(stack_truncate_to);
                    self.stack.push(result?);
                } else {
                    let args = self.take_stack_args_from(args_start)?;
                    self.stack.truncate(stack_truncate_to);
                    if !self.try_call_special_name(&reference.name, &args).await? {
                        let result = self
                            .call_builtin_id_or_name(reference.id, &reference.name, args)
                            .await?;
                        self.stack.push(result);
                    }
                }
            }
            other => {
                let message = format!("Cannot call {}", other.display());
                self.stack.truncate(stack_truncate_to);
                return Err(VmError::TypeError(message));
            }
        }
        Ok(())
    }

    pub(in crate::vm) async fn call_named_value(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if let Some(callable) = self.resolve_lexical_named_value(name) {
            self.call_exact_value(callable, args).await?;
        } else if let Some(closure) = self.resolve_named_closure(name) {
            self.call_user_closure(closure, args).await?;
        } else if let Some(closure) =
            crate::vm::tool_callable::resolve_named_single_harn_tool_handler(self, name)?
        {
            self.call_user_closure(closure, args).await?;
        } else if self.try_call_special_name(name, &args).await? {
            return Ok(());
        } else {
            let result = if let Some(id) = direct_id {
                self.call_builtin_id_or_name(id, name, args).await?
            } else {
                self.call_named_builtin(name, args).await?
            };
            self.stack.push(result);
        }
        Ok(())
    }

    pub(in crate::vm) async fn call_named_value_from_stack_args(
        &mut self,
        name: &str,
        args_start: usize,
        stack_truncate_to: usize,
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if stack_truncate_to > args_start || args_start > self.stack.len() {
            return Err(VmError::Runtime(
                "invalid call argument stack range".to_string(),
            ));
        }

        if let Some(callable) = self.resolve_lexical_named_value(name) {
            self.call_exact_value_from_stack_args(callable, args_start, stack_truncate_to)
                .await?;
            return Ok(());
        }

        if let Some(closure) = self.resolve_named_closure(name) {
            self.call_user_closure_from_stack_args(closure, args_start, stack_truncate_to)
                .await?;
            return Ok(());
        }

        match crate::vm::tool_callable::resolve_named_single_harn_tool_handler(self, name) {
            Ok(Some(closure)) => {
                self.call_user_closure_from_stack_args(closure, args_start, stack_truncate_to)
                    .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                self.stack.truncate(stack_truncate_to);
                return Err(error);
            }
        }

        if Self::is_special_name(name) {
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(stack_truncate_to);
            return self.call_named_value(name, args, direct_id).await;
        }

        if let Some(result) =
            self.try_call_sync_builtin_id_or_name_from_stack_args(direct_id, name, args_start)
        {
            self.stack.truncate(stack_truncate_to);
            self.stack.push(result?);
            return Ok(());
        }

        let args = self.take_stack_args_from(args_start)?;
        self.stack.truncate(stack_truncate_to);
        let result = if let Some(id) = direct_id {
            self.call_builtin_id_or_name(id, name, args).await?
        } else {
            self.call_named_builtin(name, args).await?
        };
        self.stack.push(result);
        Ok(())
    }
}
