use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::helpers::vm_add_role_message;

/// Register conversation management builtins.
pub(crate) fn register_conversation_builtins(vm: &mut Vm) {
    vm.register_builtin("conversation", |_args, _out| {
        // Returns a list (messages array) -- can be passed to llm_call via options.messages
        Ok(VmValue::List(Rc::new(Vec::new())))
    });

    vm.register_builtin("add_message", |args, _out| {
        let messages = match args.first() {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "add_message: first argument must be a message list",
                ))));
            }
        };
        let role = args.get(1).map(|a| a.display()).unwrap_or_default();
        let content = args.get(2).map(|a| a.display()).unwrap_or_default();

        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from(role.as_str())));
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(content.as_str())),
        );

        let mut new_messages = messages;
        new_messages.push(VmValue::Dict(Rc::new(msg)));
        Ok(VmValue::List(Rc::new(new_messages)))
    });

    vm.register_builtin("add_user", |args, _out| vm_add_role_message(args, "user"));

    vm.register_builtin("add_assistant", |args, _out| {
        vm_add_role_message(args, "assistant")
    });

    vm.register_builtin("add_system", |args, _out| {
        vm_add_role_message(args, "system")
    });

    vm.register_builtin("add_tool_result", |args, _out| {
        let messages = match args.first() {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "add_tool_result: first argument must be a message list",
                ))));
            }
        };
        let tool_use_id = args.get(1).map(|a| a.display()).unwrap_or_default();
        let result_content = args.get(2).map(|a| a.display()).unwrap_or_default();

        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from("tool_result")));
        msg.insert(
            "tool_use_id".to_string(),
            VmValue::String(Rc::from(tool_use_id.as_str())),
        );
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(result_content.as_str())),
        );

        let mut new_messages = messages;
        new_messages.push(VmValue::Dict(Rc::new(msg)));
        Ok(VmValue::List(Rc::new(new_messages)))
    });
}
