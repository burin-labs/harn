//! Terminal UI builtins backing the Harn `std/tui` module.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};
use std::rc::Rc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const CLEAR_SEQUENCE: &str = "\x1b[2J\x1b[H";
const DEFAULT_TERMINAL_WIDTH: usize = 80;
const DEFAULT_RULE_CHAR: &str = "─";

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] =
    &[&__TUI_PAGE_DEF, &__TUI_CLEAR_DEF, &__TUI_TERMINAL_WIDTH_DEF];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageOptions {
    title: Option<String>,
    body: String,
    footer: Option<String>,
    format: PageFormat,
    no_pager: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageFormat {
    Text,
    Markdown,
}

pub(crate) fn register_tui_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

/// Render a text or markdown page through a pager when appropriate.
#[harn_builtin(sig = "__tui_page(options: dict) -> dict", category = "tui")]
fn __tui_page(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let options = parse_page_options(args)?;
    let content = render_page_content(&options);

    if should_print_without_pager(&options) {
        super::io::write_stdout(out, &content);
        return Ok(page_result(true, false, None));
    }

    match run_pager(&content) {
        Ok(()) => Ok(page_result(true, true, None)),
        Err(PagerError::Unavailable(_)) => {
            super::io::write_stdout(out, &content);
            Ok(page_result(true, false, None))
        }
        Err(error) => Ok(page_result(false, true, Some(error.to_string()))),
    }
}

/// Write the ANSI clear-screen sequence to stdout.
#[harn_builtin(sig = "__tui_clear() -> nil", category = "tui")]
fn __tui_clear(_args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    super::io::write_stdout(out, CLEAR_SEQUENCE);
    Ok(VmValue::Nil)
}

/// Return the current terminal width or the supplied default.
#[harn_builtin(
    sig = "__tui_terminal_width(default_width?: int) -> int",
    category = "tui"
)]
fn __tui_terminal_width(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let default = args
        .first()
        .and_then(VmValue::as_int)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_TERMINAL_WIDTH);
    Ok(VmValue::Int(terminal_width(default) as i64))
}

fn parse_page_options(args: &[VmValue]) -> Result<PageOptions, VmError> {
    let Some(VmValue::Dict(opts)) = args.first() else {
        return Err(VmError::Runtime(
            "page: options must be a dict with a body string".to_string(),
        ));
    };
    let body = required_string(opts, "body", "page")?;
    let title = optional_string(opts, "title", "page")?;
    let footer = optional_string(opts, "footer", "page")?;
    let no_pager = optional_bool(opts, "no_pager", "page")?.unwrap_or(false);
    let format = match optional_string(opts, "format", "page")?.as_deref() {
        None | Some("text") => PageFormat::Text,
        Some("markdown") => PageFormat::Markdown,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "page: format must be 'text' or 'markdown', got '{other}'"
            )));
        }
    };
    Ok(PageOptions {
        title,
        body,
        footer,
        format,
        no_pager,
    })
}

fn required_string(
    opts: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    match opts.get(key) {
        Some(VmValue::String(value)) => Ok(value.as_ref().to_string()),
        Some(VmValue::Nil) | None => Err(VmError::Runtime(format!(
            "{builtin}: missing string field '{key}'"
        ))),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: field '{key}' must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_string(
    opts: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match opts.get(key) {
        Some(VmValue::String(value)) => Ok(Some(value.as_ref().to_string())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: field '{key}' must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_bool(
    opts: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<bool>, VmError> {
    match opts.get(key) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: field '{key}' must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn render_page_content(options: &PageOptions) -> String {
    let mut rendered = String::new();
    if let Some(title) = options
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        rendered.push_str(title);
        rendered.push('\n');
        rendered.push_str(&DEFAULT_RULE_CHAR.repeat(title.chars().count().max(3)));
        rendered.push_str("\n\n");
    }

    let body = match options.format {
        PageFormat::Text | PageFormat::Markdown => options.body.as_str(),
    };
    rendered.push_str(body);

    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    let footer = options
        .footer
        .clone()
        .unwrap_or_else(|| match options.title.as_deref() {
            Some(title) if !title.trim().is_empty() => format!("--- end of {} ---", title.trim()),
            _ => "--- end ---".to_string(),
        });
    if !footer.is_empty() {
        rendered.push_str(&footer);
        rendered.push('\n');
    }
    rendered
}

fn should_print_without_pager(options: &PageOptions) -> bool {
    options.no_pager || !super::io::is_tty_for("stdout") || pager_command_is_cat()
}

fn pager_command_is_cat() -> bool {
    pager_command()
        .first()
        .and_then(|program| program.rsplit('/').next())
        .is_some_and(|program| program == "cat")
}

fn pager_command() -> Vec<String> {
    let mut command = std::env::var("PAGER")
        .ok()
        .map(|pager| {
            pager
                .split_whitespace()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec!["less".to_string()]);
    add_default_less_flags(&mut command);
    command
}

fn add_default_less_flags(command: &mut Vec<String>) {
    let Some(program) = command.first() else {
        return;
    };
    if program.rsplit('/').next() != Some("less") {
        return;
    }
    for flag in ["-R", "-F", "-X"] {
        if !command.iter().any(|arg| arg == flag) {
            command.push(flag.to_string());
        }
    }
}

#[derive(Debug)]
enum PagerError {
    Unavailable(String),
    Io(String),
    Failed(String),
}

impl std::fmt::Display for PagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PagerError::Unavailable(message)
            | PagerError::Io(message)
            | PagerError::Failed(message) => f.write_str(message),
        }
    }
}

fn run_pager(content: &str) -> Result<(), PagerError> {
    let command = pager_command();
    let Some((program, args)) = command.split_first() else {
        return Err(PagerError::Unavailable(
            "pager command is empty".to_string(),
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                PagerError::Unavailable(format!("pager not found: {program}"))
            } else {
                PagerError::Io(format!("failed to spawn pager {program}: {error}"))
            }
        })?;

    let write_result = child.stdin.take().map(|mut stdin| {
        let result = stdin.write_all(content.as_bytes());
        drop(stdin);
        result
    });

    let status = child
        .wait()
        .map_err(|error| PagerError::Io(format!("failed to wait for pager: {error}")))?;
    if let Some(Err(error)) = write_result {
        if error.kind() != ErrorKind::BrokenPipe || !status.success() {
            return Err(PagerError::Io(format!(
                "failed to write pager input: {error}"
            )));
        }
    }
    if status.success() {
        Ok(())
    } else {
        Err(PagerError::Failed(format!(
            "pager exited with status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )))
    }
}

fn page_result(ok: bool, paged: bool, error: Option<String>) -> VmValue {
    let mut result = BTreeMap::from([
        ("ok".to_string(), VmValue::Bool(ok)),
        ("paged".to_string(), VmValue::Bool(paged)),
    ]);
    if let Some(error) = error {
        result.insert("error".to_string(), VmValue::String(Rc::from(error)));
    }
    VmValue::Dict(Rc::new(result))
}

fn terminal_width(default_width: usize) -> usize {
    terminal_width_from_columns(std::env::var("COLUMNS").ok().as_deref())
        .or_else(platform_terminal_width)
        .unwrap_or(default_width)
}

fn terminal_width_from_columns(raw: Option<&str>) -> Option<usize> {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width > 0)
}

#[cfg(unix)]
fn platform_terminal_width() -> Option<usize> {
    let mut winsize = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let winsize = unsafe { winsize.assume_init() };
    (winsize.ws_col > 0).then_some(winsize.ws_col as usize)
}

#[cfg(not(unix))]
fn platform_terminal_width() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::value::VmValue;

    use super::{
        add_default_less_flags, parse_page_options, render_page_content,
        terminal_width_from_columns, PageFormat,
    };

    fn dict(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
        let map = entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        vec![VmValue::Dict(Rc::new(map))]
    }

    #[test]
    fn page_content_adds_title_rule_and_default_footer() {
        let opts = parse_page_options(&dict(&[
            ("title", VmValue::String(Rc::from("Audit"))),
            ("body", VmValue::String(Rc::from("line one\nline two"))),
        ]))
        .unwrap();

        assert_eq!(
            render_page_content(&opts),
            "Audit\n─────\n\nline one\nline two\n--- end of Audit ---\n"
        );
    }

    #[test]
    fn page_options_accept_markdown_passthrough() {
        let opts = parse_page_options(&dict(&[
            ("body", VmValue::String(Rc::from("# Heading"))),
            ("format", VmValue::String(Rc::from("markdown"))),
            ("no_pager", VmValue::Bool(true)),
        ]))
        .unwrap();

        assert_eq!(opts.format, PageFormat::Markdown);
        assert!(opts.no_pager);
    }

    #[test]
    fn terminal_width_ignores_invalid_columns() {
        assert_eq!(terminal_width_from_columns(Some("132")), Some(132));
        assert_eq!(terminal_width_from_columns(Some("0")), None);
        assert_eq!(terminal_width_from_columns(Some("wide")), None);
        assert_eq!(terminal_width_from_columns(None), None);
    }

    #[test]
    fn less_pager_receives_harn_defaults() {
        let mut command = vec!["less".to_string(), "-S".to_string()];
        add_default_less_flags(&mut command);

        assert_eq!(command, ["less", "-S", "-R", "-F", "-X"]);
    }
}
