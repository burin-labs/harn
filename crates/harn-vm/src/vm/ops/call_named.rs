use crate::value::{VmError, VmValue};
use crate::BuiltinId;

use super::super::Vm;

impl Vm {
    pub(in crate::vm) async fn call_named_value(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if self.try_call_special_name(name, &args).await? {
            return Ok(());
        }
        if let Some(closure) = self.resolve_named_closure(name) {
            self.call_user_closure(closure, args).await?;
        } else if let Some(closure) =
            crate::vm::tool_callable::resolve_named_single_harn_tool_handler(self, name)?
        {
            self.call_user_closure(closure, args).await?;
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

        if Self::is_special_name(name) {
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(stack_truncate_to);
            return self.call_named_value(name, args, direct_id).await;
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
