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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShellDialectResolution {
    pub(super) dialect: Option<crate::shells::ShellDialect>,
    pub(super) source: String,
    pub(super) shell_id: Option<String>,
    pub(super) unresolved_reason: Option<&'static str>,
}

pub(super) fn request_shell_dialect(ctx: &JsonValue) -> ShellDialectResolution {
    if ctx
        .pointer("/request/shell_resolution_error")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return ShellDialectResolution {
            dialect: None,
            source: "request".to_string(),
            shell_id: None,
            unresolved_reason: Some("shell_resolution_failed"),
        };
    }
    let shell = ctx.pointer("/request/shell");
    let shell_source = shell
        .and_then(|value| value.get("source"))
        .and_then(JsonValue::as_str);
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
    if let Some(identity) = identity {
        let dialect = crate::shells::shell_dialect_for_id(&identity);
        return ShellDialectResolution {
            dialect,
            source: shell_source.unwrap_or("request").to_string(),
            shell_id: Some(identity),
            unresolved_reason: dialect.is_none().then_some("shell_dialect_unknown"),
        };
    }
    if platform.as_deref() == Some("windows") {
        return ShellDialectResolution {
            dialect: Some(crate::shells::ShellDialect::PowerShell),
            source: "platform_default".to_string(),
            shell_id: None,
            unresolved_reason: None,
        };
    }
    match crate::shells::get_default_shell() {
        Some(shell) => {
            let dialect = crate::shells::shell_dialect_for_id(&shell.id);
            ShellDialectResolution {
                dialect,
                source: shell.source,
                shell_id: Some(shell.id),
                unresolved_reason: dialect.is_none().then_some("shell_dialect_unknown"),
            }
        }
        None => ShellDialectResolution {
            dialect: None,
            source: "host_default".to_string(),
            shell_id: None,
            unresolved_reason: Some("shell_not_available"),
        },
    }
}
