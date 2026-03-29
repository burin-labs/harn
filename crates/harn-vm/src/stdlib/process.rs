use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static VM_SOURCE_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Set the source directory for the current thread (called by VM on file execution).
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = Some(dir.to_path_buf()));
}

/// Reset thread-local process state (for test isolation).
pub(crate) fn reset_process_state() {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = None);
}

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

    // --- System attributes for prompt building ---

    vm.register_builtin("username", |_args, _out| {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        Ok(VmValue::String(Rc::from(user)))
    });

    vm.register_builtin("hostname", |_args, _out| {
        let name = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .or_else(|_| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .ok_or(std::env::VarError::NotPresent)
            })
            .unwrap_or_default();
        Ok(VmValue::String(Rc::from(name)))
    });

    vm.register_builtin("platform", |_args, _out| {
        let os = if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            std::env::consts::OS
        };
        Ok(VmValue::String(Rc::from(os)))
    });

    vm.register_builtin("arch", |_args, _out| {
        Ok(VmValue::String(Rc::from(std::env::consts::ARCH)))
    });

    vm.register_builtin("home_dir", |_args, _out| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        Ok(VmValue::String(Rc::from(home)))
    });

    vm.register_builtin("pid", |_args, _out| {
        Ok(VmValue::Int(std::process::id() as i64))
    });

    // --- Path / directory introspection ---

    vm.register_builtin("date_iso", |_args, _out| {
        use crate::stdlib::datetime::vm_civil_from_timestamp;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let millis = now.subsec_millis();
        let (y, m, d, hour, minute, second, _) = vm_civil_from_timestamp(total_secs);
        Ok(VmValue::String(Rc::from(format!(
            "{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
        ))))
    });

    vm.register_builtin("cwd", |_args, _out| {
        let dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(VmValue::String(Rc::from(dir)))
    });
}

/// Find the project root by walking up from a base directory looking for harn.toml.
pub(crate) fn find_project_root(base: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = base.to_path_buf();
    loop {
        if dir.join("harn.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Register builtins that depend on source directory context.
pub(crate) fn register_path_builtins(vm: &mut Vm) {
    vm.register_builtin("source_dir", |_args, _out| {
        let dir = VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
        match dir {
            Some(d) => Ok(VmValue::String(Rc::from(d.to_string_lossy().to_string()))),
            None => {
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(VmValue::String(Rc::from(cwd)))
            }
        }
    });

    vm.register_builtin("project_root", |_args, _out| {
        let base = VM_SOURCE_DIR
            .with(|sd| sd.borrow().clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        match find_project_root(&base) {
            Some(root) => Ok(VmValue::String(Rc::from(
                root.to_string_lossy().to_string(),
            ))),
            None => Ok(VmValue::Nil),
        }
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
