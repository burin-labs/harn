use super::*;

pub(super) fn request_mode(ctx: &JsonValue) -> Option<&str> {
    if let Some(mode) = ctx.pointer("/request/mode").and_then(JsonValue::as_str) {
        return Some(mode);
    }
    match ctx.pointer("/request/argv") {
        Some(value) if !value.is_null() => Some("argv"),
        _ => Some("shell"),
    }
}

pub(super) fn request_argv(ctx: &JsonValue) -> Result<Option<Vec<String>>, ()> {
    if request_mode(ctx) != Some("argv") {
        return Ok(None);
    }
    let Some(value) = ctx.pointer("/request/argv") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(values) = value.as_array() else {
        return Err(());
    };
    let argv = values
        .iter()
        .map(|value| value.as_str().map(ToString::to_string).ok_or(()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!argv.is_empty()).then_some(argv))
}

pub(super) fn request_shell_dialect(
    ctx: &JsonValue,
) -> Option<super::super::catastrophic::ShellDialect> {
    if ctx
        .pointer("/request/shell_resolution_error")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let shell = ctx.pointer("/request/shell");
    let platform = shell
        .and_then(|value| value.get("platform"))
        .and_then(JsonValue::as_str)
        .map(str::to_ascii_lowercase);
    let identity = shell.and_then(|value| {
        value
            .get("id")
            .or_else(|| value.get("path"))
            .and_then(JsonValue::as_str)
    });
    let identity = identity.map(|value| {
        value
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(value)
            .to_ascii_lowercase()
    });
    match identity.as_deref() {
        Some("sh" | "bash" | "zsh") => Some(super::super::catastrophic::ShellDialect::Posix),
        Some("powershell" | "powershell.exe" | "pwsh" | "pwsh.exe") => {
            Some(super::super::catastrophic::ShellDialect::PowerShell)
        }
        Some("cmd" | "cmd.exe") => Some(super::super::catastrophic::ShellDialect::Cmd),
        Some(_) => None,
        None if platform.as_deref() == Some("windows") => {
            Some(super::super::catastrophic::ShellDialect::PowerShell)
        }
        None => crate::shells::get_default_shell().and_then(|shell| match shell.id.as_str() {
            "sh" | "bash" | "zsh" => Some(super::super::catastrophic::ShellDialect::Posix),
            "powershell" | "pwsh" => Some(super::super::catastrophic::ShellDialect::PowerShell),
            "cmd" => Some(super::super::catastrophic::ShellDialect::Cmd),
            _ => None,
        }),
    }
}
