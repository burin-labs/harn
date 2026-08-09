use std::io::IsTerminal;

use harn_lexer::Span;
use yansi::{Color, Paint};

use crate::diagnostic_codes::Repair;
use crate::ParserError;

mod harness_migrations_generated;

/// The typed harness path that replaced a removed global, from the generated
/// projection of `harn_vm::stdlib::harness_migration_for_builtin`.
///
/// `harn-parser` compiles below `harn-vm`, so it cannot query the registry;
/// `make gen-harness-migrations` emits the table and
/// `make check-harness-migrations` keeps it from drifting.
///
/// **This is the fallback, not the first answer.** The hand-written tables
/// above it encode migration decisions the registry cannot express, because the
/// registry is keyed by name and some legacy globals collide with unrelated
/// typed methods — the ambient `read_file` migrated to `harness.fs.read_text`,
/// but a *different* `harness.tools.read_file` also exists, and that is what a
/// name lookup finds. [`removed_global_replacement`] composes the sources in
/// the right order; [`HARNESS_MIGRATION_DISAGREEMENTS`] is the audited list.
pub fn generated_harness_migration(name: &str) -> Option<&'static str> {
    harness_migrations_generated::HARNESS_MIGRATIONS
        .binary_search_by_key(&name, |(legacy, _)| *legacy)
        .ok()
        .map(|index| harness_migrations_generated::HARNESS_MIGRATIONS[index].1)
}

pub struct RelatedSpanLabel<'a> {
    pub span: &'a Span,
    pub label: &'a str,
}

/// Normalize diagnostic filenames lexically for display.
///
/// This deliberately does not touch the filesystem: diagnostics should cancel
/// `.` and `..` path components even when the path points at a file that no
/// longer exists, without resolving symlinks.
pub fn normalize_diagnostic_path(path: &str) -> String {
    let posix = path.replace('\\', "/");
    if posix.is_empty() {
        return String::new();
    }

    let bytes = posix.as_bytes();
    let mut drive = "";
    let mut rest = posix.as_str();
    #[expect(
        clippy::string_slice,
        reason = "bytes 0 and 1 are ASCII, so 2 is a char boundary"
    )]
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        drive = &posix[..2];
        rest = &posix[2..];
    }

    let absolute = rest.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for segment in rest.split('/').filter(|segment| !segment.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                if let Some(top) = stack.last() {
                    if *top != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !absolute {
                    stack.push("..");
                }
            }
            _ => stack.push(segment),
        }
    }

    let mut normalized = String::new();
    normalized.push_str(drive);
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&stack.join("/"));
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn has_same_snake_case_segments(a: &str, b: &str) -> bool {
    if !a.contains('_') || !b.contains('_') {
        return false;
    }
    let mut a_segments: Vec<_> = a.split('_').collect();
    let mut b_segments: Vec<_> = b.split('_').collect();
    if a_segments.len() < 2
        || a_segments.len() != b_segments.len()
        || a_segments.iter().any(|segment| segment.is_empty())
        || b_segments.iter().any(|segment| segment.is_empty())
    {
        return false;
    }
    a_segments.sort_unstable();
    b_segments.sort_unstable();
    a_segments == b_segments
}

/// Find the closest match to `name` among `candidates`, within `max_dist` edits
/// or by reordering its non-empty underscore-separated segments. Candidates
/// within the edit-distance threshold rank ahead of reorder-only matches.
pub fn find_closest_match<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
    max_dist: usize,
) -> Option<&'a str> {
    candidates
        .filter(|candidate| *candidate != name)
        .filter_map(|candidate| {
            let reordered = has_same_snake_case_segments(name, candidate);
            if candidate.len().abs_diff(name.len()) > max_dist && !reordered {
                return None;
            }
            let distance = strsim::levenshtein(name, candidate);
            (distance <= max_dist || reordered).then_some((distance, candidate))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Return the replacement for stdlib symbols that were directly renamed.
pub fn renamed_stdlib_symbol(name: &str) -> Option<&'static str> {
    match name {
        "retry_with_backoff" => Some("retry_predicate_with_backoff"),
        "print" => Some("harness.stdio.print"),
        "println" => Some("harness.stdio.println"),
        "eprint" => Some("harness.stdio.eprint"),
        "eprintln" => Some("harness.stdio.eprintln"),
        "read_line" => Some("harness.stdio.read_line"),
        "prompt_user" => Some("harness.stdio.prompt"),
        "agent_session_open" => Some("harness.agent.open"),
        "agent_session_workspace_anchor" => Some("harness.agent.workspace_anchor"),
        "agent_session_set_workspace_anchor" => Some("harness.agent.set_workspace_anchor"),
        "agent_session_workspace_policy" => Some("harness.agent.workspace_policy"),
        "agent_session_set_workspace_policy" => Some("harness.agent.set_workspace_policy"),
        "agent_session_add_root" => Some("harness.agent.add_root"),
        "agent_session_remove_root" => Some("harness.agent.remove_root"),
        "agent_session_list_roots" => Some("harness.agent.list_roots"),
        "agent_session_exists" => Some("harness.agent.exists"),
        "agent_session_length" => Some("harness.agent.length"),
        "agent_session_snapshot" => Some("harness.agent.snapshot"),
        "agent_session_ancestry" => Some("harness.agent.ancestry"),
        "agent_session_current_id" => Some("harness.agent.current_id"),
        "agent_session_record_changed_path" => Some("harness.agent.record_changed_path"),
        "agent_session_actor_chain" => Some("harness.agent.actor_chain"),
        "agent_session_tool_format" => Some("harness.agent.tool_format"),
        "agent_session_system_prompt" => Some("harness.agent.system_prompt"),
        "agent_session_scratchpad" => Some("harness.agent.scratchpad"),
        "agent_session_set_scratchpad" => Some("harness.agent.set_scratchpad"),
        "agent_session_clear_scratchpad" => Some("harness.agent.clear_scratchpad"),
        "agent_session_claim_tool_format" => Some("harness.agent.claim_tool_format"),
        "agent_session_reset" => Some("harness.agent.reset"),
        "agent_session_fork" => Some("harness.agent.fork"),
        "agent_session_fork_at" => Some("harness.agent.fork_at"),
        "agent_session_rollback" => Some("harness.agent.rollback"),
        "agent_session_redo" => Some("harness.agent.redo"),
        "agent_session_close" => Some("harness.agent.close"),
        "agent_session_trim" => Some("harness.agent.trim"),
        "agent_session_attach" => Some("harness.agent.attach"),
        "agent_session_takeover" => Some("harness.agent.takeover"),
        "agent_session_detach" => Some("harness.agent.detach"),
        "agent_session_heartbeat" => Some("harness.agent.heartbeat"),
        "agent_session_live_clients" => Some("harness.agent.live_clients"),
        "agent_session_client_inject_prompt" => Some("harness.agent.client_inject_prompt"),
        "agent_session_route_permission" => Some("harness.agent.route_permission"),
        "agent_session_inject" => Some("harness.agent.inject"),
        "agent_session_post_event" => Some("harness.agent.post_event"),
        "agent_session_drain_inbox" => Some("harness.agent.drain_inbox"),
        "agent_session_seed_from_jsonl" => Some("harness.agent.seed_from_jsonl"),
        "agent_session_reanchor" => Some("harness.agent.reanchor"),
        "agent_session_compact" => Some("harness.agent.compact"),
        _ => None,
    }
}

/// What an undefined name *should* have been, for the type checker's
/// "did you mean".
///
/// Three sources, narrowest first: the in-place renames above, then the
/// `harness_*_replacement` families, then [`generated_harness_migration`] for
/// the long tail — 417 generated rows against roughly 60 hand-written ones.
///
/// Deliberately separate from [`renamed_stdlib_symbol`], which the linter uses
/// to decide whether to raise `HARN-LNT-001`. Widening *that* makes every
/// migrated global draw two warnings: the rename lint and the capability-family
/// lint that already claims the name (`HARN-LNT-052`/`053`/`054`/`057`/`071`).
/// Measured before splitting them — a four-call probe went from four findings to
/// eight. Which lint should own a name is a policy question; answering "what
/// replaced it" is not, and only the type checker asks this one.
///
/// Order matters, and not for style. The generated table is keyed by builtin
/// name, and some legacy globals share a name with an unrelated typed method —
/// the ambient `read_file` migrated to `harness.fs.read_text`, while a separate
/// `harness.tools.read_file` agent tool is what a name lookup finds.
/// [`HARNESS_MIGRATION_DISAGREEMENTS`] pins those, so a new one is a build
/// failure rather than a confidently wrong suggestion.
pub fn removed_global_replacement(name: &str) -> Option<&'static str> {
    if let Some(replacement) = renamed_stdlib_symbol(name) {
        return Some(replacement);
    }
    for family in HARNESS_REPLACEMENT_FAMILIES {
        if let Some(replacement) = family(name) {
            return Some(replacement);
        }
    }
    generated_harness_migration(name)
}

/// The `harness_*_replacement` families, in one list so anything meaning "any
/// capability migration" walks the same set.
///
/// A new family belongs here as well as beside its siblings. Nothing proves
/// that mechanically — Rust has no reflection over free functions — but this
/// list is the only way anything reaches them in bulk, so forgetting it leaves
/// the new family unreachable rather than half-wired.
pub const HARNESS_REPLACEMENT_FAMILIES: &[fn(&str) -> Option<&'static str>] = &[
    harness_clock_replacement,
    harness_stdio_replacement,
    harness_fs_replacement,
    harness_env_replacement,
    harness_random_replacement,
    harness_net_replacement,
];

/// Legacy globals whose hand-written migration deliberately differs from what a
/// name lookup in the generated table finds, with the reason.
///
/// Reviewed rather than discovered: a new entry means the runtime registry and
/// the migration record disagree about a name, which is a finding to
/// investigate — not a list to extend casually.
pub const HARNESS_MIGRATION_DISAGREEMENTS: &[(&str, &str, &str)] = &[
    (
        "read_file",
        "harness.fs.read_text",
        "`harness.tools.read_file` is an agent tool, not the filesystem capability",
    ),
    (
        "write_file",
        "harness.fs.write_text",
        "`harness.tools.write_file` is an agent tool, not the filesystem capability",
    ),
    (
        "delete_file",
        "harness.fs.delete",
        "`harness.tools.delete_file` is an agent tool, not the filesystem capability",
    ),
    (
        "elapsed",
        "harness.clock.monotonic_ms",
        "`elapsed` was renamed, so the same-named `harness.clock.elapsed` is not its successor",
    ),
];

/// Map an ambient clock-capability builtin to its `harness.clock.*`
/// replacement. Returns the new identifier text (including the receiver
/// path) so the `bindings/thread-harness-clock` repair can replace the
/// call-site identifier in place. The mapping is the source of truth for
/// the E4.3 → E4.6 migration; downstream replatform agents query it via
/// [`Code::repair_template`].
pub fn harness_clock_replacement(name: &str) -> Option<&'static str> {
    match name {
        "now_ms" => Some("harness.clock.now_ms"),
        "monotonic_ms" => Some("harness.clock.monotonic_ms"),
        "sleep_ms" => Some("harness.clock.sleep_ms"),
        "sleep" => Some("harness.clock.sleep_ms"),
        "timestamp" => Some("harness.clock.timestamp"),
        "elapsed" => Some("harness.clock.monotonic_ms"),
        "date_now" => Some("harness.clock.now"),
        "date_now_iso" => Some("harness.clock.date_iso"),
        _ => None,
    }
}

/// Map an ambient stdio-capability builtin to its `harness.stdio.*`
/// replacement so `harn fix` can replace the call in place once the
/// relevant harness binding is available.
pub fn harness_stdio_replacement(name: &str) -> Option<&'static str> {
    match name {
        "print" => Some("harness.stdio.print"),
        "println" => Some("harness.stdio.println"),
        "eprint" => Some("harness.stdio.eprint"),
        "eprintln" => Some("harness.stdio.eprintln"),
        "read_line" => Some("harness.stdio.read_line"),
        "prompt_user" => Some("harness.stdio.prompt"),
        _ => None,
    }
}

/// Map an ambient fs-capability builtin to its `harness.fs.*` replacement.
/// Backs the `bindings/thread-harness-fs` repair the E4.4 → E4.6
/// migration uses to rewrite `.harn` scripts off the legacy surface.
pub fn harness_fs_replacement(name: &str) -> Option<&'static str> {
    match name {
        "read_file" => Some("harness.fs.read_text"),
        "read_file_result" => Some("harness.fs.read_text_result"),
        "read_file_bytes" => Some("harness.fs.read_bytes"),
        "write_file" => Some("harness.fs.write_text"),
        "write_file_bytes" => Some("harness.fs.write_bytes"),
        "replace_file" => Some("harness.fs.replace_text"),
        "replace_file_result" => Some("harness.fs.replace_text_result"),
        "replace_file_bytes" => Some("harness.fs.replace_bytes"),
        "replace_file_bytes_result" => Some("harness.fs.replace_bytes_result"),
        "file_exists" => Some("harness.fs.exists"),
        "path_status" => Some("harness.fs.status"),
        "delete_file" => Some("harness.fs.delete"),
        "append_file" => Some("harness.fs.append"),
        "append_file_locked" => Some("harness.fs.append_locked"),
        "list_dir" => Some("harness.fs.list_dir"),
        "mkdir" => Some("harness.fs.mkdir"),
        "copy_file" => Some("harness.fs.copy"),
        "temp_dir" => Some("harness.fs.temp_dir"),
        "workspace_temp_dir" => Some("harness.fs.workspace_temp_dir"),
        "mkdtemp" => Some("harness.fs.mkdtemp"),
        "mkdtemp_in_workspace" => Some("harness.fs.mkdtemp_in_workspace"),
        "stat" => Some("harness.fs.stat"),
        "move_file" => Some("harness.fs.rename"),
        "read_lines" => Some("harness.fs.read_lines"),
        "read_lines_page_result" => Some("harness.fs.read_lines_page_result"),
        "walk_dir" => Some("harness.fs.walk"),
        "glob" => Some("harness.fs.glob"),
        "find_text" => Some("harness.fs.find_text"),
        "find_evidence" => Some("harness.fs.find_evidence"),
        "cwd" => Some("harness.fs.cwd"),
        _ => None,
    }
}

/// Map an ambient env-capability builtin to its `harness.env.*` replacement.
/// Backs the `bindings/thread-harness-env` repair.
pub fn harness_env_replacement(name: &str) -> Option<&'static str> {
    match name {
        "env" => Some("harness.env.get"),
        "env_or" => Some("harness.env.get_or"),
        _ => None,
    }
}

/// Map an ambient random-capability builtin to its `harness.random.*`
/// replacement. Backs the `bindings/thread-harness-random` repair.
pub fn harness_random_replacement(name: &str) -> Option<&'static str> {
    match name {
        "random" => Some("harness.random.f64"),
        "random_int" => Some("harness.random.range"),
        "random_choice" => Some("harness.random.choice"),
        "random_shuffle" => Some("harness.random.shuffle"),
        _ => None,
    }
}

/// Map an ambient net-capability builtin to its `harness.net.*`
/// replacement. Backs the `bindings/thread-harness-net` repair. Every
/// script-facing network effect is exposed only through `HarnessNet`; response
/// constructors and event encoders remain pure globals.
pub fn harness_net_replacement(name: &str) -> Option<&'static str> {
    match name {
        "http_get" => Some("harness.net.get"),
        "http_post" => Some("harness.net.post"),
        "http_put" => Some("harness.net.put"),
        "http_patch" => Some("harness.net.patch"),
        "http_delete" => Some("harness.net.delete"),
        "http_request" => Some("harness.net.request"),
        "http_download" => Some("harness.net.download"),
        "http_server" => Some("harness.net.server"),
        "http_server_after" => Some("harness.net.server_after"),
        "http_server_before" => Some("harness.net.server_before"),
        "http_server_on_shutdown" => Some("harness.net.server_on_shutdown"),
        "http_server_readiness" => Some("harness.net.server_readiness"),
        "http_server_ready" => Some("harness.net.server_ready"),
        "http_server_request" => Some("harness.net.server_request"),
        "http_server_route" => Some("harness.net.server_route"),
        "http_server_security_headers" => Some("harness.net.server_security_headers"),
        "http_server_set_ready" => Some("harness.net.server_set_ready"),
        "http_server_shutdown" => Some("harness.net.server_shutdown"),
        "http_server_test" => Some("harness.net.server_test"),
        "http_server_tls_edge" => Some("harness.net.server_tls_edge"),
        "http_server_tls_pem" => Some("harness.net.server_tls_pem"),
        "http_server_tls_plain" => Some("harness.net.server_tls_plain"),
        "http_server_tls_self_signed_dev" => Some("harness.net.server_tls_self_signed_dev"),
        "http_session" => Some("harness.net.session"),
        "http_session_close" => Some("harness.net.session_close"),
        "http_session_request" => Some("harness.net.session_request"),
        "http_stream_close" => Some("harness.net.stream_close"),
        "http_stream_info" => Some("harness.net.stream_info"),
        "http_stream_open" => Some("harness.net.stream_open"),
        "http_stream_read" => Some("harness.net.stream_read"),
        "sse_close" => Some("harness.net.sse_close"),
        "sse_connect" => Some("harness.net.sse_connect"),
        "sse_receive" => Some("harness.net.sse_receive"),
        "sse_server_cancel" => Some("harness.net.sse_server_cancel"),
        "sse_server_cancelled" => Some("harness.net.sse_server_cancelled"),
        "sse_server_close" => Some("harness.net.sse_server_close"),
        "sse_server_disconnected" => Some("harness.net.sse_server_disconnected"),
        "sse_server_flush" => Some("harness.net.sse_server_flush"),
        "sse_server_heartbeat" => Some("harness.net.sse_server_heartbeat"),
        "sse_server_response" => Some("harness.net.sse_server_response"),
        "sse_server_send" => Some("harness.net.sse_server_send"),
        "sse_server_status" => Some("harness.net.sse_server_status"),
        "websocket_accept" => Some("harness.net.websocket_accept"),
        "websocket_close" => Some("harness.net.websocket_close"),
        "websocket_connect" => Some("harness.net.websocket_connect"),
        "websocket_receive" => Some("harness.net.websocket_receive"),
        "websocket_route" => Some("harness.net.websocket_route"),
        "websocket_send" => Some("harness.net.websocket_send"),
        "websocket_server" => Some("harness.net.websocket_server"),
        "websocket_server_close" => Some("harness.net.websocket_server_close"),
        _ => None,
    }
}

/// Render a Rust-style diagnostic message.
///
/// Example output:
/// ```text
/// error: undefined variable `x`
///   --> example.harn:5:12
///    |
///  5 |     let y = x + 1
///    |             ^ not found in this scope
/// ```
pub fn render_diagnostic(
    source: &str,
    filename: &str,
    span: &Span,
    severity: &str,
    message: &str,
    label: Option<&str>,
    help: Option<&str>,
) -> String {
    render_diagnostic_inner(RenderDiagnostic {
        source,
        filename,
        span,
        severity,
        code: None,
        message,
        label,
        help,
        related: &[],
        repair: None,
    })
}

pub fn render_diagnostic_with_code(
    source: &str,
    filename: &str,
    span: &Span,
    severity: &str,
    code: crate::diagnostic_codes::Code,
    message: &str,
    label: Option<&str>,
    help: Option<&str>,
) -> String {
    let repair_owned = code.repair_template().map(Repair::from_template);
    render_diagnostic_inner(RenderDiagnostic {
        source,
        filename,
        span,
        severity,
        code: Some(code.as_str()),
        message,
        label,
        help,
        related: &[],
        repair: repair_owned.as_ref(),
    })
}

pub fn render_diagnostic_with_related(
    source: &str,
    filename: &str,
    span: &Span,
    severity: &str,
    message: &str,
    label: Option<&str>,
    help: Option<&str>,
    related: &[RelatedSpanLabel<'_>],
) -> String {
    render_diagnostic_inner(RenderDiagnostic {
        source,
        filename,
        span,
        severity,
        code: None,
        message,
        label,
        help,
        related,
        repair: None,
    })
}

struct RenderDiagnostic<'a> {
    source: &'a str,
    filename: &'a str,
    span: &'a Span,
    severity: &'a str,
    code: Option<&'a str>,
    message: &'a str,
    label: Option<&'a str>,
    help: Option<&'a str>,
    related: &'a [RelatedSpanLabel<'a>],
    repair: Option<&'a Repair>,
}

fn render_diagnostic_inner(input: RenderDiagnostic<'_>) -> String {
    let mut out = String::new();
    let source = input.source;
    let span = input.span;
    let severity = input.severity;
    let message = input.message;
    let label = input.label;
    let help = input.help;
    let related = input.related;
    let filename = normalize_diagnostic_path(input.filename);
    let severity_color = severity_color(severity);
    let gutter = style_fragment("|", Color::Blue, false);
    let arrow = style_fragment("-->", Color::Blue, true);
    let help_prefix = style_fragment("help", Color::Cyan, true);
    let note_prefix = style_fragment("note", Color::Magenta, true);

    out.push_str(&style_fragment(severity, severity_color, true));
    if let Some(code) = input.code {
        out.push('[');
        out.push_str(code);
        out.push(']');
    }
    out.push_str(": ");
    out.push_str(message);
    out.push('\n');

    let line_num = span.line;
    let col_num = span.column;

    let gutter_width = line_num.to_string().len();

    out.push_str(&format!(
        "{:>width$}{arrow} {filename}:{line_num}:{col_num}\n",
        " ",
        width = gutter_width + 1,
    ));

    out.push_str(&format!(
        "{:>width$} {gutter}\n",
        " ",
        width = gutter_width + 1,
    ));

    let source_line_opt = line_num.checked_sub(1).and_then(|n| source.lines().nth(n));
    if let Some(source_line) = source_line_opt {
        out.push_str(&format!(
            "{:>width$} {gutter} {source_line}\n",
            line_num,
            width = gutter_width + 1,
        ));

        if let Some(label_text) = label {
            // Span width must use char count, not byte offsets, so carets align with the source text.
            let span_len = diagnostic_span_char_len(source, span);
            let col_num = col_num.max(1);
            let padding = " ".repeat(col_num - 1);
            let carets = style_fragment(&"^".repeat(span_len), severity_color, true);
            out.push_str(&format!(
                "{:>width$} {gutter} {padding}{carets} {label_text}\n",
                " ",
                width = gutter_width + 1,
            ));
        }
    }

    if let Some(help_text) = help {
        out.push_str(&format!(
            "{:>width$} = {help_prefix}: {help_text}\n",
            " ",
            width = gutter_width + 1,
        ));
    }

    if let Some(repair) = input.repair {
        let repair_prefix = style_fragment("repair", Color::Cyan, true);
        out.push_str(&format!(
            "{:>width$} = {repair_prefix}: {} [{}] — {}\n",
            " ",
            repair.id,
            repair.safety,
            repair.summary,
            width = gutter_width + 1,
        ));
    }

    for item in related {
        out.push_str(&format!(
            "{:>width$} = {note_prefix}: {}\n",
            " ",
            item.label,
            width = gutter_width + 1,
        ));
        render_related_span(
            &mut out,
            source,
            &filename,
            item.span,
            item.label,
            gutter_width,
        );
    }

    if let Some(note_text) = fun_note(severity) {
        out.push_str(&format!(
            "{:>width$} = {note_prefix}: {note_text}\n",
            " ",
            width = gutter_width + 1,
        ));
    }

    out
}

pub fn render_type_diagnostic(
    source: &str,
    filename: &str,
    diag: &crate::typechecker::TypeDiagnostic,
) -> String {
    let severity = match diag.severity {
        crate::typechecker::DiagnosticSeverity::Error => "error",
        crate::typechecker::DiagnosticSeverity::Warning => "warning",
    };
    let related = diag
        .related
        .iter()
        .map(|related| RelatedSpanLabel {
            span: &related.span,
            label: &related.message,
        })
        .collect::<Vec<_>>();
    let primary_label = type_diagnostic_primary_label(diag);
    match &diag.span {
        Some(span) => render_diagnostic_inner(RenderDiagnostic {
            source,
            filename,
            span,
            severity,
            code: Some(diag.code.as_str()),
            message: &diag.message,
            label: primary_label.as_deref(),
            help: diag.help.as_deref(),
            related: &related,
            repair: diag.repair.as_ref(),
        }),
        None => match diag.repair.as_ref() {
            Some(repair) => format!(
                "{severity}[{}]: {}\n  = repair: {} [{}] — {}\n",
                diag.code, diag.message, repair.id, repair.safety, repair.summary,
            ),
            None => format!("{severity}[{}]: {}\n", diag.code, diag.message),
        },
    }
}

pub fn lexer_error_code(err: &harn_lexer::LexerError) -> crate::diagnostic_codes::Code {
    match err {
        harn_lexer::LexerError::UnexpectedCharacter(_, _) => {
            crate::diagnostic_codes::Code::ParserUnexpectedCharacter
        }
        harn_lexer::LexerError::UnterminatedString(_) => {
            crate::diagnostic_codes::Code::ParserUnterminatedString
        }
        harn_lexer::LexerError::UnterminatedBlockComment(_) => {
            crate::diagnostic_codes::Code::ParserUnterminatedBlockComment
        }
        harn_lexer::LexerError::IntegerLiteralOutOfRange(_, _) => {
            crate::diagnostic_codes::Code::ParserIntegerLiteralOutOfRange
        }
    }
}

pub fn parser_error_code(err: &crate::parser::ParserError) -> crate::diagnostic_codes::Code {
    match err {
        crate::parser::ParserError::Unexpected { .. } => {
            crate::diagnostic_codes::Code::ParserUnexpectedToken
        }
        crate::parser::ParserError::UnexpectedEof { .. } => {
            crate::diagnostic_codes::Code::ParserUnexpectedEof
        }
    }
}

fn type_diagnostic_primary_label(diag: &crate::typechecker::TypeDiagnostic) -> Option<String> {
    match &diag.details {
        Some(crate::typechecker::DiagnosticDetails::LintRule { rule }) => {
            Some(format!("lint[{rule}]"))
        }
        Some(crate::typechecker::DiagnosticDetails::TypeMismatch { .. }) => {
            Some("found this type".to_string())
        }
        _ => None,
    }
}

fn render_related_span(
    out: &mut String,
    source: &str,
    filename: &str,
    span: &Span,
    label: &str,
    primary_gutter_width: usize,
) {
    let filename = normalize_diagnostic_path(filename);
    let severity_color = Color::Magenta;
    let gutter = style_fragment("|", Color::Blue, false);
    let arrow = style_fragment("-->", Color::Blue, true);
    let line_num = span.line;
    let col_num = span.column;
    let gutter_width = primary_gutter_width.max(line_num.to_string().len());

    out.push_str(&format!(
        "{:>width$}{arrow} {filename}:{line_num}:{col_num}\n",
        " ",
        width = gutter_width + 1,
    ));
    out.push_str(&format!(
        "{:>width$} {gutter}\n",
        " ",
        width = gutter_width + 1,
    ));

    if let Some(source_line) = line_num.checked_sub(1).and_then(|n| source.lines().nth(n)) {
        out.push_str(&format!(
            "{:>width$} {gutter} {source_line}\n",
            line_num,
            width = gutter_width + 1,
        ));
        let span_len = diagnostic_span_char_len(source, span);
        let padding = " ".repeat(col_num.max(1) - 1);
        let carets = style_fragment(&"^".repeat(span_len), severity_color, true);
        out.push_str(&format!(
            "{:>width$} {gutter} {padding}{carets} {label}\n",
            " ",
            width = gutter_width + 1,
        ));
    }
}

fn diagnostic_span_char_len(source: &str, span: &Span) -> usize {
    if span.end <= span.start || span.start >= source.len() {
        return 1;
    }
    let mut start = span.start.min(source.len());
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = span.end.min(source.len());
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    source
        .get(start..end)
        .map(|text| text.chars().count().max(1))
        .unwrap_or(1)
}

fn severity_color(severity: &str) -> Color {
    match severity {
        "error" => Color::Red,
        "warning" => Color::Yellow,
        "note" => Color::Magenta,
        _ => Color::Cyan,
    }
}

fn style_fragment(text: &str, color: Color, bold: bool) -> String {
    if !colors_enabled() {
        return text.to_string();
    }

    let mut paint = Paint::new(text).fg(color);
    if bold {
        paint = paint.bold();
    }
    paint.to_string()
}

thread_local! {
    /// Per-thread override for color output. When `Some`, it wins over the
    /// `NO_COLOR` env var and TTY detection. This lets tests force colors off
    /// deterministically without mutating the process-global `NO_COLOR` env
    /// var, which races across parallel tests (each test runs on its own
    /// thread, so the override is naturally isolated).
    static COLOR_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force color output on (`Some(true)`), off (`Some(false)`), or restore the
/// default env/TTY behavior (`None`) for the current thread only.
#[cfg(test)]
pub(crate) fn set_color_override(force: Option<bool>) {
    COLOR_OVERRIDE.with(|cell| cell.set(force));
}

fn colors_enabled() -> bool {
    if let Some(forced) = COLOR_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

fn fun_note(severity: &str) -> Option<&'static str> {
    if std::env::var("HARN_FUN").ok().as_deref() != Some("1") {
        return None;
    }

    Some(match severity {
        "error" => "the compiler stepped on a rake here.",
        "warning" => "this still runs, but it has strong 'double-check me' energy.",
        _ => "a tiny gremlin has left a note in the margins.",
    })
}

pub fn parser_error_message(err: &ParserError) -> String {
    match err {
        ParserError::Unexpected { got, expected, .. } => {
            format!("expected {expected}, found {got}")
        }
        ParserError::UnexpectedEof { expected, .. } => {
            format!("unexpected end of file, expected {expected}")
        }
    }
}

pub fn parser_error_label(err: &ParserError) -> &'static str {
    match err {
        ParserError::Unexpected { got, .. } if got == "Newline" => "line break not allowed here",
        ParserError::Unexpected { .. } => "unexpected token",
        ParserError::UnexpectedEof { .. } => "file ends here",
    }
}

pub fn parser_error_help(err: &ParserError) -> Option<&'static str> {
    match err {
        ParserError::UnexpectedEof { expected, .. } | ParserError::Unexpected { expected, .. } => {
            match expected.as_str() {
                "}" => Some("add a closing `}` to finish this block"),
                ")" => Some("add a closing `)` to finish this expression or parameter list"),
                "]" => Some("add a closing `]` to finish this list or subscript"),
                "fn, struct, enum, or pipeline after pub" => {
                    Some("use `pub fn`, `pub pipeline`, `pub enum`, or `pub struct`")
                }
                "fn, tool, skill, eval_pack, struct, enum, type, pipeline, const, let, or import after pub" => Some(
                    "use `pub` with `fn`, `tool`, `skill`, `eval_pack`, `struct`, `enum`, `type`, `pipeline`, `const`, `let`, or `import`",
                ),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure ANSI colors are off so plain-text assertions work regardless
    /// of whether the test runner's stderr is a TTY. Uses a thread-local
    /// override rather than the process-global `NO_COLOR` env var so it can't
    /// race with color-sensitive assertions in parallel tests.
    fn disable_colors() {
        set_color_override(Some(false));
    }

    #[test]
    fn test_basic_diagnostic() {
        disable_colors();
        let source = "pipeline default(task) {\n    const y = x + 1\n}";
        let span = Span {
            start: 28,
            end: 29,
            line: 2,
            column: 13,
            end_line: 2,
        };
        let output = render_diagnostic(
            source,
            "example.harn",
            &span,
            "error",
            "undefined variable `x`",
            Some("not found in this scope"),
            None,
        );
        assert!(output.contains("error: undefined variable `x`"));
        assert!(output.contains("--> example.harn:2:13"));
        assert!(output.contains("const y = x + 1"));
        assert!(output.contains("^ not found in this scope"));
    }

    #[test]
    fn test_diagnostic_normalizes_filename() {
        disable_colors();
        let source = "const value = thing";
        let span = Span {
            start: 12,
            end: 17,
            line: 1,
            column: 13,
            end_line: 1,
        };
        let output = render_diagnostic(
            source,
            "/workspace/pipelines/mode/../lib/runtime/loop.harn",
            &span,
            "error",
            "bad value",
            Some("here"),
            None,
        );
        assert!(output.contains("--> /workspace/pipelines/lib/runtime/loop.harn:1:13"));
        assert!(!output.contains("/../"));
    }

    #[test]
    fn test_diagnostic_with_help() {
        disable_colors();
        let source = "const y = xx + 1";
        let span = Span {
            start: 8,
            end: 10,
            line: 1,
            column: 9,
            end_line: 1,
        };
        let output = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "undefined variable `xx`",
            Some("not found in this scope"),
            Some("did you mean `x`?"),
        );
        assert!(output.contains("help: did you mean `x`?"));
    }

    #[test]
    fn test_multiline_source() {
        disable_colors();
        let source = "line1\nline2\nline3";
        let span = Span::with_offsets(6, 11, 2, 1); // "line2"
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "bad line",
            Some("here"),
            None,
        );
        assert!(result.contains("line2"));
        assert!(result.contains("^^^^^"));
    }

    #[test]
    fn diagnostic_rendering_tolerates_offsets_inside_utf8_codepoints() {
        disable_colors();
        let source = "// capability owner — narrow helper";
        let em_dash = source.find('—').expect("em dash");
        let span = Span::with_offsets(0, em_dash + 1, 1, 1);
        let output = render_diagnostic(
            source,
            "unicode.harn",
            &span,
            "warning",
            "legacy comment",
            Some("rewrite this comment"),
            None,
        );
        assert!(output.contains("capability owner — narrow helper"));
        assert!(output.contains("rewrite this comment"));
    }

    #[test]
    fn test_single_char_span() {
        disable_colors();
        let source = "const x = 42";
        let span = Span::with_offsets(4, 5, 1, 5); // "x"
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "warning",
            "unused",
            Some("never used"),
            None,
        );
        assert!(result.contains('^'));
        assert!(result.contains("never used"));
    }

    #[test]
    fn test_with_help() {
        disable_colors();
        let source = "const y = reponse";
        let span = Span::with_offsets(8, 15, 1, 9);
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "undefined",
            None,
            Some("did you mean `response`?"),
        );
        assert!(result.contains("help:"));
        assert!(result.contains("response"));
    }

    #[test]
    fn closest_match_suggests_reordered_snake_case_segments() {
        assert_eq!(
            find_closest_match("parse_json", ["json_parse"].into_iter(), 2),
            Some("json_parse")
        );
        assert_eq!(
            find_closest_match("read_file", ["file_read"].into_iter(), 2),
            Some("file_read")
        );
        assert_eq!(
            find_closest_match("parse_json", ["parse_yaml"].into_iter(), 2),
            None
        );
    }

    #[test]
    fn closest_match_suggests_plain_typo() {
        assert_eq!(
            find_closest_match("json_pars", ["json_parse"].into_iter(), 2),
            Some("json_parse")
        );
    }

    #[test]
    fn test_parser_error_helpers_for_eof() {
        disable_colors();
        let err = ParserError::UnexpectedEof {
            expected: "}".into(),
            span: Span::with_offsets(10, 10, 3, 1),
        };
        assert_eq!(
            parser_error_message(&err),
            "unexpected end of file, expected }"
        );
        assert_eq!(parser_error_label(&err), "file ends here");
        assert_eq!(
            parser_error_help(&err),
            Some("add a closing `}` to finish this block")
        );
    }
}

#[cfg(test)]
mod harness_migration_tests {
    use super::{
        generated_harness_migration, removed_global_replacement, renamed_stdlib_symbol,
        HARNESS_MIGRATION_DISAGREEMENTS, HARNESS_REPLACEMENT_FAMILIES,
    };

    /// The case that motivated harn#6151: the type checker's fuzzy fallback
    /// answered `uuid_v5` — a name-based UUID — for the time-ordered `uuid_v7`,
    /// while the runtime record one crate away already knew the answer.
    #[test]
    fn a_migrated_global_resolves_instead_of_falling_through_to_a_guess() {
        assert_eq!(
            removed_global_replacement("uuid_v7"),
            Some("harness.random.uuid_v7")
        );
        // And the linter's narrower question is unchanged, so `uuid_v7` does
        // not start drawing a rename lint on top of its capability lint.
        assert_eq!(renamed_stdlib_symbol("uuid_v7"), None);
    }

    /// The generated table is keyed by builtin name, and a few legacy globals
    /// share a name with an unrelated typed method. The hand-written answer has
    /// to win, or `harn check` sends a filesystem call to an agent tool.
    #[test]
    fn a_name_collision_resolves_to_the_migration_not_the_same_named_method() {
        for (legacy, migration, reason) in HARNESS_MIGRATION_DISAGREEMENTS {
            assert_eq!(
                removed_global_replacement(legacy),
                Some(*migration),
                "`{legacy}` must resolve to its migration — {reason}"
            );
            assert_ne!(
                generated_harness_migration(legacy),
                Some(*migration),
                "`{legacy}` is pinned as a disagreement but the generated table now \
                 agrees; drop it from HARNESS_MIGRATION_DISAGREEMENTS"
            );
        }
    }

    /// Every hand-written family entry either matches the generated table or is
    /// a pinned disagreement. A fifth divergence fails here rather than shipping
    /// a confidently wrong "did you mean".
    #[test]
    fn no_unreviewed_disagreement_between_the_hand_written_and_generated_tables() {
        let pinned: Vec<&str> = HARNESS_MIGRATION_DISAGREEMENTS
            .iter()
            .map(|(legacy, _, _)| *legacy)
            .collect();
        let mut unreviewed = Vec::new();
        for (legacy, generated) in super::harness_migrations_generated::HARNESS_MIGRATIONS {
            if pinned.contains(legacy) {
                continue;
            }
            for family in HARNESS_REPLACEMENT_FAMILIES {
                if let Some(hand_written) = family(legacy) {
                    if hand_written != *generated {
                        unreviewed.push(format!(
                            "`{legacy}`: hand-written={hand_written} generated={generated}"
                        ));
                    }
                }
            }
        }
        assert!(
            unreviewed.is_empty(),
            "these names disagree without a reviewed reason:\n{}",
            unreviewed.join("\n")
        );
    }

    /// The generator emits the table sorted; the lookup binary-searches it.
    #[test]
    fn the_generated_table_is_sorted_and_unique() {
        let table = super::harness_migrations_generated::HARNESS_MIGRATIONS;
        assert!(table.len() > 100, "expected the whole migrated surface");
        for pair in table.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "`{}` and `{}` are out of order or duplicated",
                pair[0].0,
                pair[1].0
            );
        }
    }
}
