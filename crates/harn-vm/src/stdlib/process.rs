use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_process_builtins(vm: &mut Vm) {
    vm.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        match std::env::var(&name) {
            Ok(val) => Ok(VmValue::String(Rc::from(val))),
            Err(_) => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("timestamp", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(VmValue::Float(secs))
    });

    vm.register_builtin("exit", |args, _out| {
        let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        std::process::exit(code as i32);
    });

    vm.register_builtin("exec", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "exec: command is required",
            ))));
        }
        let cmd = args[0].display();
        let cmd_args: Vec<String> = args[1..].iter().map(|a| a.display()).collect();
        let output = std::process::Command::new(&cmd)
            .args(&cmd_args)
            .output()
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("exec failed: {e}")))))?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("shell", |args, _out| {
        let cmd = args.first().map(|a| a.display()).unwrap_or_default();
        if cmd.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "shell: command string is required",
            ))));
        }
        let shell = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let flag = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };
        let output = std::process::Command::new(shell)
            .arg(flag)
            .arg(&cmd)
            .output()
            .map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("shell failed: {e}"))))
            })?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("elapsed", |_args, _out| {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        Ok(VmValue::Int(start.elapsed().as_millis() as i64))
    });
}

fn vm_output_to_value(output: std::process::Output) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert(
        "stdout".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stdout).to_string().as_str(),
        )),
    );
    result.insert(
        "stderr".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stderr).to_string().as_str(),
        )),
    );
    result.insert(
        "status".to_string(),
        VmValue::Int(output.status.code().unwrap_or(-1) as i64),
    );
    result.insert(
        "success".to_string(),
        VmValue::Bool(output.status.success()),
    );
    VmValue::Dict(Rc::new(result))
}
