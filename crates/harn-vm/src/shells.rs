use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::value::{VmError, VmValue};

thread_local! {
    static SELECTED_DEFAULT_SHELL_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellDescriptor {
    pub id: String,
    pub label: String,
    pub path: String,
    pub platform: String,
    pub available: bool,
    pub supports_login: bool,
    pub supports_interactive: bool,
    pub default_args: Vec<String>,
    pub login_args: Vec<String>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCatalog {
    pub shells: Vec<ShellDescriptor>,
    pub default_shell_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub command_arg_index: usize,
    pub shell: ShellDescriptor,
}

pub fn clear_selected_default_shell_for_test() {
    SELECTED_DEFAULT_SHELL_ID.with(|selected| *selected.borrow_mut() = None);
}

pub fn discover_shells() -> ShellCatalog {
    let shells = platform_shells();
    let selected = SELECTED_DEFAULT_SHELL_ID.with(|selected| selected.borrow().clone());
    let default_shell_id = selected
        .filter(|id| {
            shells
                .iter()
                .any(|shell| shell.id == *id && shell.available)
        })
        .or_else(|| {
            shells
                .iter()
                .find(|shell| shell.available)
                .map(|shell| shell.id.clone())
        })
        .or_else(|| shells.first().map(|shell| shell.id.clone()));
    ShellCatalog {
        shells,
        default_shell_id,
    }
}

pub fn get_default_shell() -> Option<ShellDescriptor> {
    let catalog = discover_shells();
    catalog
        .default_shell_id
        .as_deref()
        .and_then(|id| catalog.shells.iter().find(|shell| shell.id == id))
        .cloned()
        .or_else(|| catalog.shells.first().cloned())
}

pub fn set_default_shell(shell_id: &str) -> Result<ShellDescriptor, String> {
    let catalog = discover_shells();
    let Some(shell) = catalog
        .shells
        .iter()
        .find(|shell| shell.id == shell_id && shell.available)
        .cloned()
    else {
        return Err(format!("unknown or unavailable shell id {shell_id:?}"));
    };
    SELECTED_DEFAULT_SHELL_ID.with(|selected| *selected.borrow_mut() = Some(shell.id.clone()));
    Ok(shell)
}

pub fn list_shells_vm_value() -> VmValue {
    shell_catalog_to_vm_value(&discover_shells())
}

pub fn default_shell_vm_value() -> VmValue {
    get_default_shell()
        .map(|shell| shell_descriptor_to_vm_value(&shell))
        .unwrap_or(VmValue::Nil)
}

pub fn set_default_shell_vm_value(params: &BTreeMap<String, VmValue>) -> Result<VmValue, VmError> {
    let shell_id = params
        .get("shell_id")
        .or_else(|| params.get("id"))
        .and_then(vm_string)
        .ok_or_else(|| {
            VmError::Runtime("process.set_default_shell missing shell_id".to_string())
        })?;
    set_default_shell(shell_id)
        .map(|shell| shell_descriptor_to_vm_value(&shell))
        .map_err(|err| VmError::Runtime(format!("process.set_default_shell: {err}")))
}

pub fn shell_invocation_vm_value(params: &BTreeMap<String, VmValue>) -> Result<VmValue, VmError> {
    resolve_invocation_from_vm_params(params)
        .map(|invocation| shell_invocation_to_vm_value(&invocation))
        .map_err(|err| VmError::Runtime(format!("process.shell_invocation: {err}")))
}

pub fn default_shell_invocation(command: &str) -> Result<ShellInvocation, String> {
    let shell = get_default_shell().ok_or_else(|| "no shell candidates available".to_string())?;
    Ok(invocation_for_shell(
        shell,
        command.to_string(),
        false,
        false,
    ))
}

pub fn resolve_invocation_from_vm_params(
    params: &BTreeMap<String, VmValue>,
) -> Result<ShellInvocation, String> {
    let command = params
        .get("command")
        .and_then(vm_string)
        .unwrap_or("{command}")
        .to_string();
    let login = optional_bool(params, "login").unwrap_or(false);
    let interactive = optional_bool(params, "interactive").unwrap_or(false);
    let shell = resolve_shell_from_vm_params(params)?;
    Ok(invocation_for_shell(shell, command, login, interactive))
}

pub fn resolve_shell_from_vm_params(
    params: &BTreeMap<String, VmValue>,
) -> Result<ShellDescriptor, String> {
    if let Some(shell) = params.get("shell").and_then(|value| value.as_dict()) {
        return shell_descriptor_from_vm_dict(shell);
    }
    if let Some(shell_id) = params.get("shell_id").and_then(vm_string) {
        return shell_by_id(shell_id);
    }
    Err("shell mode requires `shell` or `shell_id`".to_string())
}

pub fn shell_descriptor_to_vm_value(shell: &ShellDescriptor) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert("id".to_string(), string(&shell.id));
    map.insert("label".to_string(), string(&shell.label));
    map.insert("path".to_string(), string(&shell.path));
    map.insert("platform".to_string(), string(&shell.platform));
    map.insert("available".to_string(), VmValue::Bool(shell.available));
    map.insert(
        "supports_login".to_string(),
        VmValue::Bool(shell.supports_login),
    );
    map.insert(
        "supports_interactive".to_string(),
        VmValue::Bool(shell.supports_interactive),
    );
    map.insert("default_args".to_string(), string_list(&shell.default_args));
    map.insert("login_args".to_string(), string_list(&shell.login_args));
    map.insert("source".to_string(), string(&shell.source));
    VmValue::Dict(Rc::new(map))
}

pub fn shell_invocation_to_vm_value(invocation: &ShellInvocation) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert("program".to_string(), string(&invocation.program));
    map.insert("args".to_string(), string_list(&invocation.args));
    map.insert(
        "command_arg_index".to_string(),
        VmValue::Int(invocation.command_arg_index as i64),
    );
    map.insert(
        "shell".to_string(),
        shell_descriptor_to_vm_value(&invocation.shell),
    );
    VmValue::Dict(Rc::new(map))
}

fn shell_catalog_to_vm_value(catalog: &ShellCatalog) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "shells".to_string(),
        VmValue::List(Rc::new(
            catalog
                .shells
                .iter()
                .map(shell_descriptor_to_vm_value)
                .collect(),
        )),
    );
    map.insert(
        "default_shell_id".to_string(),
        catalog
            .default_shell_id
            .as_ref()
            .map(|id| string(id))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(map))
}

fn shell_descriptor_from_vm_dict(
    dict: &BTreeMap<String, VmValue>,
) -> Result<ShellDescriptor, String> {
    if let Some(path) = dict.get("path").and_then(vm_string) {
        let id = dict
            .get("id")
            .and_then(vm_string)
            .map(ToString::to_string)
            .unwrap_or_else(|| shell_id_from_path(path));
        let platform = dict
            .get("platform")
            .and_then(vm_string)
            .unwrap_or(platform_name())
            .to_string();
        let label = dict
            .get("label")
            .and_then(vm_string)
            .map(ToString::to_string)
            .unwrap_or_else(|| id.clone());
        let default_args = dict
            .get("default_args")
            .and_then(vm_string_list)
            .unwrap_or_else(|| default_args_for_id(&id));
        let login_args = dict
            .get("login_args")
            .and_then(vm_string_list)
            .unwrap_or_else(|| login_args_for_id(&id));
        let available = dict
            .get("available")
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            })
            .unwrap_or_else(|| executable_available(path));
        let supports_login = dict
            .get("supports_login")
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            })
            .unwrap_or_else(|| supports_login_for_id(&id));
        let supports_interactive = dict
            .get("supports_interactive")
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            })
            .unwrap_or_else(|| supports_interactive_for_id(&id));
        return Ok(ShellDescriptor {
            id,
            label,
            path: path.to_string(),
            platform,
            available,
            supports_login,
            supports_interactive,
            default_args,
            login_args,
            source: dict
                .get("source")
                .and_then(vm_string)
                .unwrap_or("host")
                .to_string(),
        });
    }
    if let Some(id) = dict.get("id").and_then(vm_string) {
        return shell_by_id(id);
    }
    Err("shell object requires `path` or `id`".to_string())
}

fn shell_by_id(shell_id: &str) -> Result<ShellDescriptor, String> {
    discover_shells()
        .shells
        .into_iter()
        .find(|shell| shell.id == shell_id)
        .ok_or_else(|| format!("unknown shell id {shell_id:?}"))
}

fn invocation_for_shell(
    shell: ShellDescriptor,
    command: String,
    login: bool,
    interactive: bool,
) -> ShellInvocation {
    let mut args = if login && shell.supports_login && !shell.login_args.is_empty() {
        shell.login_args.clone()
    } else {
        shell.default_args.clone()
    };
    if interactive && shell.supports_interactive && !args.iter().any(|arg| arg == "-i") {
        args.insert(0, "-i".to_string());
    }
    let command_arg_index = args.len();
    args.push(command);
    ShellInvocation {
        program: shell.path.clone(),
        args,
        command_arg_index,
        shell,
    }
}

#[cfg(windows)]
fn platform_shells() -> Vec<ShellDescriptor> {
    let mut shells = Vec::new();
    if let Ok(value) = std::env::var("HARN_DEFAULT_SHELL") {
        push_shell(&mut shells, descriptor_for_path(&value, "configured"));
    }
    if let Ok(value) = std::env::var("COMSPEC") {
        push_shell(&mut shells, descriptor_for_path(&value, "env"));
    }
    for (id, label, executable) in [
        ("pwsh", "PowerShell 7", "pwsh.exe"),
        ("powershell", "Windows PowerShell", "powershell.exe"),
        ("cmd", "cmd", "cmd.exe"),
    ] {
        let path = find_on_path(executable).unwrap_or_else(|| executable.to_string());
        let mut shell = descriptor_for_path(&path, "fallback");
        shell.id = id.to_string();
        shell.label = label.to_string();
        push_shell(&mut shells, shell);
    }
    shells
}

#[cfg(not(windows))]
fn platform_shells() -> Vec<ShellDescriptor> {
    let mut shells = Vec::new();
    if let Ok(value) = std::env::var("HARN_DEFAULT_SHELL") {
        push_shell(&mut shells, descriptor_for_path(&value, "configured"));
    }
    if let Ok(value) = std::env::var("SHELL") {
        push_shell(&mut shells, descriptor_for_path(&value, "env"));
    }
    if let Some(value) = login_shell_from_passwd() {
        push_shell(&mut shells, descriptor_for_path(&value, "login"));
    }
    for value in shells_from_etc_shells() {
        push_shell(&mut shells, descriptor_for_path(&value, "etc_shells"));
    }
    for value in [
        "/bin/zsh",
        "/bin/bash",
        "/bin/sh",
        "/usr/bin/zsh",
        "/usr/bin/bash",
        "/usr/bin/sh",
    ] {
        push_shell(&mut shells, descriptor_for_path(value, "fallback"));
    }
    shells
}

fn push_shell(shells: &mut Vec<ShellDescriptor>, shell: ShellDescriptor) {
    if shells.iter().any(|existing| existing.id == shell.id) {
        return;
    }
    shells.push(shell);
}

fn descriptor_for_path(path: &str, source: &str) -> ShellDescriptor {
    let id = shell_id_from_path(path);
    ShellDescriptor {
        id: id.clone(),
        label: label_for_id(&id),
        path: path.to_string(),
        platform: platform_name().to_string(),
        available: executable_available(path),
        supports_login: supports_login_for_id(&id),
        supports_interactive: supports_interactive_for_id(&id),
        default_args: default_args_for_id(&id),
        login_args: login_args_for_id(&id),
        source: source.to_string(),
    }
}

fn shell_id_from_path(path: &str) -> String {
    let raw = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_ascii_lowercase();
    let file_name = raw.strip_suffix(".exe").unwrap_or(&raw);
    match file_name {
        "powershell" | "windowspowershell" => "powershell".to_string(),
        "pwsh" => "pwsh".to_string(),
        "cmd" => "cmd".to_string(),
        "bash" => "bash".to_string(),
        "zsh" => "zsh".to_string(),
        "fish" => "fish".to_string(),
        _ if file_name.is_empty() => "shell".to_string(),
        _ => file_name.to_string(),
    }
}

fn label_for_id(id: &str) -> String {
    match id {
        "pwsh" => "PowerShell 7",
        "powershell" => "Windows PowerShell",
        "cmd" => "cmd",
        "bash" => "bash",
        "zsh" => "zsh",
        "fish" => "fish",
        "sh" => "sh",
        other => other,
    }
    .to_string()
}

fn default_args_for_id(id: &str) -> Vec<String> {
    match id {
        "cmd" => vec!["/C".to_string()],
        "pwsh" | "powershell" => vec!["-NoProfile".to_string(), "-Command".to_string()],
        _ => vec!["-c".to_string()],
    }
}

fn login_args_for_id(id: &str) -> Vec<String> {
    match id {
        "cmd" | "pwsh" | "powershell" => default_args_for_id(id),
        _ => vec!["-l".to_string(), "-c".to_string()],
    }
}

fn supports_login_for_id(id: &str) -> bool {
    !matches!(id, "cmd" | "pwsh" | "powershell")
}

fn supports_interactive_for_id(id: &str) -> bool {
    !matches!(id, "cmd" | "pwsh" | "powershell")
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

fn executable_available(path: &str) -> bool {
    let path_obj = Path::new(path);
    if path_obj.components().count() > 1 || path_obj.is_absolute() {
        return path_obj.is_file();
    }
    find_on_path(path).is_some()
}

fn find_on_path(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let candidates = path_candidates(program);
    for dir in std::env::split_paths(&path) {
        for candidate in &candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full.display().to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn path_candidates(program: &str) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(program)];
    if Path::new(program).extension().is_none() {
        for ext in [".exe", ".cmd", ".bat"] {
            candidates.push(PathBuf::from(format!("{program}{ext}")));
        }
    }
    candidates
}

#[cfg(not(windows))]
fn path_candidates(program: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(program)]
}

#[cfg(not(windows))]
fn login_shell_from_passwd() -> Option<String> {
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut parts = line.split(':');
        let name = parts.next()?;
        if name != username {
            return None;
        }
        parts
            .nth(5)
            .map(str::trim)
            .filter(|shell| {
                !shell.is_empty() && !shell.ends_with("/false") && !shell.ends_with("/nologin")
            })
            .map(ToString::to_string)
    })
}

#[cfg(not(windows))]
fn shells_from_etc_shells() -> Vec<String> {
    let Ok(content) = std::fs::read_to_string("/etc/shells") else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && line.starts_with('/'))
        .filter(|line| seen.insert((*line).to_string()))
        .map(ToString::to_string)
        .collect()
}

fn optional_bool(params: &BTreeMap<String, VmValue>, key: &str) -> Option<bool> {
    match params.get(key) {
        Some(VmValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn vm_string(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(value) => Some(value.as_ref()),
        _ => None,
    }
}

fn vm_string_list(value: &VmValue) -> Option<Vec<String>> {
    let VmValue::List(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| vm_string(value).map(ToString::to_string))
        .collect()
}

fn string(value: &str) -> VmValue {
    VmValue::String(Rc::from(value.to_string()))
}

fn string_list(values: &[String]) -> VmValue {
    VmValue::List(Rc::new(values.iter().map(|value| string(value)).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_shell_descriptor_uses_split_login_args() {
        let shell = descriptor_for_path("/bin/zsh", "fallback");
        assert_eq!(shell.id, "zsh");
        assert_eq!(shell.default_args, vec!["-c"]);
        assert_eq!(shell.login_args, vec!["-l", "-c"]);
        assert!(shell.supports_login);
        assert!(shell.supports_interactive);
    }

    #[test]
    fn windows_shell_descriptor_distinguishes_cmd_and_pwsh() {
        let cmd = descriptor_for_path("cmd.exe", "fallback");
        assert_eq!(cmd.id, "cmd");
        assert_eq!(cmd.default_args, vec!["/C"]);
        assert!(!cmd.supports_login);

        let pwsh = descriptor_for_path("pwsh.exe", "fallback");
        assert_eq!(pwsh.id, "pwsh");
        assert_eq!(pwsh.default_args, vec!["-NoProfile", "-Command"]);
    }

    #[test]
    fn invocation_appends_command_after_shell_args() {
        let shell = ShellDescriptor {
            id: "zsh".to_string(),
            label: "zsh".to_string(),
            path: "/bin/zsh".to_string(),
            platform: "darwin".to_string(),
            available: true,
            supports_login: true,
            supports_interactive: true,
            default_args: vec!["-c".to_string()],
            login_args: vec!["-l".to_string(), "-c".to_string()],
            source: "test".to_string(),
        };
        let invocation = invocation_for_shell(shell, "echo ok".to_string(), true, true);
        assert_eq!(invocation.program, "/bin/zsh");
        assert_eq!(invocation.args, vec!["-i", "-l", "-c", "echo ok"]);
        assert_eq!(invocation.command_arg_index, 3);
    }
}
