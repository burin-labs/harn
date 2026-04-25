use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::orchestration::RunExecutionRecord;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HARN_REPLAY_ENV: &str = "HARN_REPLAY";

thread_local! {
    pub(crate) static VM_SOURCE_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static VM_EXECUTION_CONTEXT: RefCell<Option<RunExecutionRecord>> = const { RefCell::new(None) };
}

/// Set the source directory for the current thread (called by VM on file execution).
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = Some(normalize_context_path(dir)));
}

pub(crate) fn normalize_context_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

pub fn set_thread_execution_context(context: Option<RunExecutionRecord>) {
    VM_EXECUTION_CONTEXT.with(|current| *current.borrow_mut() = context);
}

pub(crate) fn current_execution_context() -> Option<RunExecutionRecord> {
    VM_EXECUTION_CONTEXT.with(|current| current.borrow().clone())
}

/// Reset thread-local process state (for test isolation).
pub(crate) fn reset_process_state() {
    VM_SOURCE_DIR.with(|sd| *sd.borrow_mut() = None);
    VM_EXECUTION_CONTEXT.with(|current| *current.borrow_mut() = None);
}

pub fn execution_root_path() -> PathBuf {
    current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn source_root_path() -> PathBuf {
    VM_SOURCE_DIR
        .with(|sd| sd.borrow().clone())
        .or_else(|| {
            current_execution_context().and_then(|context| context.source_dir.map(PathBuf::from))
        })
        .or_else(|| current_execution_context().and_then(|context| context.cwd.map(PathBuf::from)))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn asset_root_path() -> PathBuf {
    source_root_path()
}

fn env_override(name: &str) -> Option<String> {
    (name == HARN_REPLAY_ENV && crate::triggers::dispatcher::current_dispatch_is_replay())
        .then(|| "1".to_string())
}

fn read_env_value(name: &str) -> Option<String> {
    env_override(name)
        .or_else(|| current_execution_context().and_then(|context| context.env.get(name).cloned()))
        .or_else(|| std::env::var(name).ok())
}

pub fn runtime_root_base() -> PathBuf {
    find_project_root(&execution_root_path())
        .or_else(|| find_project_root(&source_root_path()))
        .unwrap_or_else(source_root_path)
}

pub fn resolve_source_relative_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    execution_root_path().join(candidate)
}

pub fn resolve_source_asset_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    asset_root_path().join(candidate)
}

pub(crate) fn register_process_builtins(vm: &mut Vm) {
    vm.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        if let Some(value) = read_env_value(&name) {
            return Ok(VmValue::String(Rc::from(value)));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("env_or", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
        if let Some(value) = read_env_value(&name) {
            return Ok(VmValue::String(Rc::from(value)));
        }
        Ok(default)
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
        let output = exec_command(None, &cmd, &cmd_args)?;
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
        let output = exec_shell(None, shell, flag, &cmd)?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("exec_at", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "exec_at: directory and command are required",
            ))));
        }
        let dir = args[0].display();
        let cmd = args[1].display();
        let cmd_args: Vec<String> = args[2..].iter().map(|a| a.display()).collect();
        let output = exec_command(Some(dir.as_str()), &cmd, &cmd_args)?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("shell_at", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "shell_at: directory and command string are required",
            ))));
        }
        let dir = args[0].display();
        let cmd = args[1].display();
        if cmd.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "shell_at: command string is required",
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
        let output = exec_shell(Some(dir.as_str()), shell, flag, &cmd)?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("elapsed", |_args, _out| {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        Ok(VmValue::Int(start.elapsed().as_millis() as i64))
    });

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
        let dir = current_execution_context()
            .and_then(|context| context.cwd)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        Ok(VmValue::String(Rc::from(dir)))
    });

    vm.register_builtin("execution_root", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            execution_root_path().to_string_lossy().into_owned(),
        )))
    });

    vm.register_builtin("asset_root", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            asset_root_path().to_string_lossy().into_owned(),
        )))
    });

    vm.register_builtin("runtime_paths", |_args, _out| {
        let runtime_base = runtime_root_base();
        let mut paths = BTreeMap::new();
        paths.insert(
            "execution_root".to_string(),
            VmValue::String(Rc::from(
                execution_root_path().to_string_lossy().into_owned(),
            )),
        );
        paths.insert(
            "asset_root".to_string(),
            VmValue::String(Rc::from(asset_root_path().to_string_lossy().into_owned())),
        );
        paths.insert(
            "state_root".to_string(),
            VmValue::String(Rc::from(
                crate::runtime_paths::state_root(&runtime_base)
                    .to_string_lossy()
                    .into_owned(),
            )),
        );
        paths.insert(
            "run_root".to_string(),
            VmValue::String(Rc::from(
                crate::runtime_paths::run_root(&runtime_base)
                    .to_string_lossy()
                    .into_owned(),
            )),
        );
        paths.insert(
            "worktree_root".to_string(),
            VmValue::String(Rc::from(
                crate::runtime_paths::worktree_root(&runtime_base)
                    .to_string_lossy()
                    .into_owned(),
            )),
        );
        Ok(VmValue::Dict(Rc::new(paths)))
    });
}

/// Find the project root by walking up from a base directory looking for harn.toml.
pub fn find_project_root(base: &std::path::Path) -> Option<std::path::PathBuf> {
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
            Some(d) => Ok(VmValue::String(Rc::from(d.to_string_lossy().into_owned()))),
            None => {
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(VmValue::String(Rc::from(cwd)))
            }
        }
    });

    vm.register_builtin("project_root", |_args, _out| {
        let base = current_execution_context()
            .and_then(|context| context.cwd.map(PathBuf::from))
            .or_else(|| VM_SOURCE_DIR.with(|sd| sd.borrow().clone()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        match find_project_root(&base) {
            Some(root) => Ok(VmValue::String(Rc::from(
                root.to_string_lossy().into_owned(),
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

fn exec_command(
    dir: Option<&str>,
    cmd: &str,
    args: &[String],
) -> Result<std::process::Output, VmError> {
    let config = process_command_config(dir)?;
    crate::stdlib::sandbox::command_output(cmd, args, &config)
        .map_err(|error| prefix_process_error(error, "exec"))
}

fn exec_shell(
    dir: Option<&str>,
    shell: &str,
    flag: &str,
    script: &str,
) -> Result<std::process::Output, VmError> {
    let args = vec![flag.to_string(), script.to_string()];
    let config = process_command_config(dir)?;
    crate::stdlib::sandbox::command_output(shell, &args, &config)
        .map_err(|error| prefix_process_error(error, "shell"))
}

fn process_command_config(
    dir: Option<&str>,
) -> Result<crate::stdlib::sandbox::ProcessCommandConfig, VmError> {
    let mut config = crate::stdlib::sandbox::ProcessCommandConfig {
        stdin_null: true,
        ..Default::default()
    };
    if let Some(dir) = dir {
        let resolved = resolve_command_dir(dir);
        crate::stdlib::sandbox::enforce_process_cwd(&resolved)?;
        config.cwd = Some(resolved);
    } else if let Some(context) = current_execution_context() {
        if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
            crate::stdlib::sandbox::enforce_process_cwd(std::path::Path::new(&cwd))?;
            config.cwd = Some(std::path::PathBuf::from(cwd));
        }
        if !context.env.is_empty() {
            config.env.extend(context.env);
        }
    }
    if let Some(value) = env_override(HARN_REPLAY_ENV) {
        config.env.push((HARN_REPLAY_ENV.to_string(), value));
    }
    Ok(config)
}

fn prefix_process_error(error: VmError, prefix: &str) -> VmError {
    match error {
        VmError::Thrown(VmValue::String(message)) => VmError::Thrown(VmValue::String(Rc::from(
            format!("{prefix} failed: {message}"),
        ))),
        other => other,
    }
}

fn resolve_command_dir(dir: &str) -> PathBuf {
    let candidate = PathBuf::from(dir);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(cwd) = current_execution_context().and_then(|context| context.cwd) {
        return PathBuf::from(cwd).join(candidate);
    }
    if let Some(source_dir) = VM_SOURCE_DIR.with(|sd| sd.borrow().clone()) {
        return source_dir.join(candidate);
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_source_relative_path_ignores_thread_source_dir_without_execution_context() {
        let dir = std::env::temp_dir().join(format!("harn-process-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let current_dir = std::env::current_dir().unwrap();
        set_thread_source_dir(&dir);
        let resolved = resolve_source_relative_path("templates/prompt.txt");
        assert_eq!(resolved, current_dir.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_source_relative_path_prefers_execution_cwd_over_source_dir() {
        let cwd = std::env::temp_dir().join(format!("harn-process-cwd-{}", uuid::Uuid::now_v7()));
        let source_dir =
            std::env::temp_dir().join(format!("harn-process-source-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        set_thread_source_dir(&source_dir);
        set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            source_dir: Some(source_dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }));
        let resolved = resolve_source_relative_path("templates/prompt.txt");
        assert_eq!(resolved, cwd.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn resolve_source_asset_path_prefers_execution_source_dir_over_cwd() {
        let cwd = std::env::temp_dir().join(format!("harn-asset-cwd-{}", uuid::Uuid::now_v7()));
        let source_dir =
            std::env::temp_dir().join(format!("harn-asset-source-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        set_thread_source_dir(&source_dir);
        set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            source_dir: Some(source_dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }));
        let resolved = resolve_source_asset_path("templates/prompt.txt");
        assert_eq!(resolved, source_dir.join("templates/prompt.txt"));
        reset_process_state();
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn set_thread_source_dir_absolutizes_relative_paths() {
        reset_process_state();
        let current_dir = std::env::current_dir().unwrap();
        set_thread_source_dir(std::path::Path::new("scripts"));
        assert_eq!(source_root_path(), current_dir.join("scripts"));
        reset_process_state();
    }

    #[test]
    fn exec_context_sets_default_cwd_and_env() {
        let dir = std::env::temp_dir().join(format!("harn-process-ctx-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "ok").unwrap();
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: BTreeMap::from([("HARN_PROCESS_TEST".to_string(), "present".to_string())]),
            ..Default::default()
        }));
        let output = exec_shell(
            None,
            "sh",
            "-c",
            "printf '%s:' \"$HARN_PROCESS_TEST\" && test -f marker.txt",
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "present:");
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_at_resolves_relative_to_execution_cwd() {
        let dir = std::env::temp_dir().join(format!("harn-process-rel-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("marker.txt"), "ok").unwrap();
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        }));
        let output = exec_shell(Some("nested"), "sh", "-c", "test -f marker.txt").unwrap();
        assert!(output.status.success());
        reset_process_state();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_paths_uses_configurable_state_roots() {
        let base =
            std::env::temp_dir().join(format!("harn-process-runtime-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, ".custom-harn");
        std::env::set_var(crate::runtime_paths::HARN_RUN_DIR_ENV, ".custom-runs");
        std::env::set_var(
            crate::runtime_paths::HARN_WORKTREE_DIR_ENV,
            ".custom-worktrees",
        );
        set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(base.to_string_lossy().into_owned()),
            ..Default::default()
        }));

        let mut vm = crate::vm::Vm::new();
        register_process_builtins(&mut vm);
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get("runtime_paths")
            .expect("runtime_paths builtin");
        let paths = match builtin(&[], &mut out).unwrap() {
            VmValue::Dict(map) => map,
            other => panic!("expected dict, got {other:?}"),
        };
        assert_eq!(
            paths.get("state_root").unwrap().display(),
            base.join(".custom-harn").display().to_string()
        );
        assert_eq!(
            paths.get("run_root").unwrap().display(),
            base.join(".custom-runs").display().to_string()
        );
        assert_eq!(
            paths.get("worktree_root").unwrap().display(),
            base.join(".custom-worktrees").display().to_string()
        );

        reset_process_state();
        std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV);
        std::env::remove_var(crate::runtime_paths::HARN_RUN_DIR_ENV);
        std::env::remove_var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV);
        let _ = std::fs::remove_dir_all(&base);
    }
}
