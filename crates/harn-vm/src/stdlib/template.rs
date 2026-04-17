//! Prompt-template engine for `.harn.prompt` assets and the `render` /
//! `render_prompt` builtins.
//!
//! # Surface
//!
//! ```text
//! {{ name }}                                 interpolation
//! {{ user.name }} / {{ items[0] }}           nested path access
//! {{ name | upper | default: "anon" }}       filter pipeline
//! {{ if expr }}..{{ elif expr }}..{{ else }}..{{ end }}
//! {{ for x in xs }}..{{ else }}..{{ end }}   else = empty-iterable fallback
//! {{ for k, v in dict }}..{{ end }}
//! {{ include "partial.harn.prompt" }}
//! {{ include "partial.harn.prompt" with { x: name } }}
//! {{# comment — stripped at parse time #}}
//! {{ raw }}..literal {{braces}}..{{ endraw }}
//! {{- x -}}                                  whitespace-trim markers
//! ```
//!
//! Back-compat: bare `{{ident}}` resolves silently to the empty fallthrough
//! (writes back the literal text on miss) — preserving the pre-v2 contract.
//! All new constructs raise `TemplateError` on parse or evaluation failure.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};

// Thread-local registry of recent prompt renders keyed by `prompt_id`.
// Populated by `render_with_provenance` so the DAP adapter can serve
// `burin/promptProvenance` and `burin/promptConsumers` reverse queries
// without forcing the pipeline author to pass the spans dict back up
// through the bridge. Capped at 64 renders (FIFO) to bound memory.
thread_local! {
    static PROMPT_REGISTRY: RefCell<Vec<RegisteredPrompt>> = const { RefCell::new(Vec::new()) };
}

const PROMPT_REGISTRY_CAP: usize = 64;

#[derive(Debug, Clone)]
pub struct RegisteredPrompt {
    pub prompt_id: String,
    pub template_uri: String,
    pub rendered: String,
    pub spans: Vec<PromptSourceSpan>,
}

/// Record a provenance map in the thread-local registry and return the
/// assigned `prompt_id`. Newest entries push to the back; when the cap
/// is reached the oldest entry is dropped so the registry never grows
/// unboundedly over long sessions.
pub(crate) fn register_prompt(
    template_uri: String,
    rendered: String,
    spans: Vec<PromptSourceSpan>,
) -> String {
    let prompt_id = format!("prompt-{}", next_prompt_serial());
    PROMPT_REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        if reg.len() >= PROMPT_REGISTRY_CAP {
            reg.remove(0);
        }
        reg.push(RegisteredPrompt {
            prompt_id: prompt_id.clone(),
            template_uri,
            rendered,
            spans,
        });
    });
    prompt_id
}

thread_local! {
    static PROMPT_SERIAL: RefCell<u64> = const { RefCell::new(0) };
}

fn next_prompt_serial() -> u64 {
    PROMPT_SERIAL.with(|s| {
        let mut s = s.borrow_mut();
        *s += 1;
        *s
    })
}

/// Resolve an output byte offset to its originating template span.
/// Returns the innermost matching `Expr` / `LegacyBareInterp` span when
/// one exists, falling back to broader structural spans (If / For /
/// Include) so a click anywhere in a rendered loop iteration still
/// navigates somewhere useful.
pub fn lookup_prompt_span(
    prompt_id: &str,
    output_offset: usize,
) -> Option<(String, PromptSourceSpan)> {
    PROMPT_REGISTRY.with(|reg| {
        let reg = reg.borrow();
        let entry = reg.iter().find(|p| p.prompt_id == prompt_id)?;
        let best = entry
            .spans
            .iter()
            .filter(|s| {
                output_offset >= s.output_start
                    && output_offset < s.output_end.max(s.output_start + 1)
            })
            .min_by_key(|s| {
                let width = s.output_end.saturating_sub(s.output_start);
                let kind_weight = match s.kind {
                    PromptSpanKind::Expr => 0,
                    PromptSpanKind::LegacyBareInterp => 1,
                    PromptSpanKind::Text => 2,
                    PromptSpanKind::Include => 3,
                    PromptSpanKind::ForIteration => 4,
                    PromptSpanKind::If => 5,
                };
                (kind_weight, width)
            })?
            .clone();
        Some((entry.template_uri.clone(), best))
    })
}

/// Return every span across every registered prompt that overlaps a
/// template range. Powers the inverse "which rendered ranges consumed
/// this template region?" navigation.
pub fn lookup_prompt_consumers(
    template_uri: &str,
    template_line_start: usize,
    template_line_end: usize,
) -> Vec<(String, PromptSourceSpan)> {
    PROMPT_REGISTRY.with(|reg| {
        let reg = reg.borrow();
        reg.iter()
            .filter(|p| p.template_uri == template_uri)
            .flat_map(|p| {
                let prompt_id = p.prompt_id.clone();
                p.spans
                    .iter()
                    .filter(move |s| {
                        let line = s.template_line;
                        line > 0 && line >= template_line_start && line <= template_line_end
                    })
                    .cloned()
                    .map(move |s| (prompt_id.clone(), s))
            })
            .collect()
    })
}

/// Clear the registry. Wired into `reset_thread_local_state` so tests
/// and serialized adapter sessions start from a clean slate.
pub(crate) fn reset_prompt_registry() {
    PROMPT_REGISTRY.with(|reg| reg.borrow_mut().clear());
    PROMPT_SERIAL.with(|s| *s.borrow_mut() = 0);
}

/// Parse-only validation for lint/preflight. Returns a human-readable error
/// message when the template body is syntactically invalid; `Ok(())` when the
/// template would parse. Does not resolve `{{ include }}` targets — those are
/// validated at render time with their own error reporting.
pub fn validate_template_syntax(src: &str) -> Result<(), String> {
    parse(src).map(|_| ()).map_err(|e| e.message())
}

/// Full-featured entrypoint that preserves errors. `base` is the directory
/// used to resolve `{{ include "..." }}` paths; `source_path` (if known) is
/// included in error messages.
pub(crate) fn render_template_result(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
) -> Result<String, TemplateError> {
    let (rendered, _spans) =
        render_template_with_provenance(template, bindings, base, source_path, false)?;
    Ok(rendered)
}

/// One byte-range in a rendered prompt mapped back to its source
/// template. Foundation for the prompt-provenance UX (burin-code #93):
/// hover a chunk of the live prompt in the debugger and jump to the
/// `.harn.prompt` line that produced it.
///
/// `output_start` / `output_end` are byte offsets into the rendered
/// string. `template_line` / `template_col` are 1-based positions in
/// the source template. `bound_value` carries a short preview of the
/// expression's runtime value when it's a scalar; omitted for
/// structural nodes (if/for/include) so callers don't log a giant
/// dict display for a single `{% for %}`.
#[derive(Debug, Clone)]
pub struct PromptSourceSpan {
    pub template_line: usize,
    pub template_col: usize,
    pub output_start: usize,
    pub output_end: usize,
    pub kind: PromptSpanKind,
    pub bound_value: Option<String>,
    /// When the span was rendered from inside an `include` (possibly
    /// transitively), this points at the including call's span in the
    /// parent template. Chained boxes let the IDE walk `A → B → C`
    /// cross-template breadcrumbs when a deep render spans three
    /// files. `None` for top-level spans.
    pub parent_span: Option<Box<PromptSourceSpan>>,
    /// Template URI for the file that authored this span. Top-level
    /// spans carry the root render's template uri; included-child
    /// spans carry the included file's uri so breadcrumb navigation
    /// can open the right file when the user clicks through the
    /// `parent_span` chain. Defaults to empty string for callers that
    /// don't plumb it through.
    pub template_uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSpanKind {
    /// Literal template text between directives.
    Text,
    /// `{{ expr }}` interpolation — the most common kind the IDE
    /// wants to highlight on hover.
    Expr,
    /// Legacy bare `{{ident}}` fallthrough, surfaced separately so the
    /// IDE can visually distinguish resolved from pass-through.
    LegacyBareInterp,
    /// Conditional branch text that actually rendered (the taken branch).
    If,
    /// One loop iteration's rendered body.
    ForIteration,
    /// Rendered partial/include expansion. Child spans still carry
    /// their own template_uri via a future extension (#96).
    Include,
}

/// Provenance-aware rendering. Returns the rendered string plus — when
/// `collect_provenance` is true — one `PromptSourceSpan` per node so the
/// IDE can link rendered byte ranges back to template source offsets.
/// When `collect_provenance` is false, this degrades to the cheap
/// non-tracked rendering path that the legacy callers use.
pub(crate) fn render_template_with_provenance(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    base: Option<&Path>,
    source_path: Option<&Path>,
    collect_provenance: bool,
) -> Result<(String, Vec<PromptSourceSpan>), TemplateError> {
    let nodes = parse(template).map_err(|mut e| {
        if let Some(p) = source_path {
            e.path = Some(p.to_path_buf());
        }
        e
    })?;
    let mut out = String::with_capacity(template.len());
    let mut scope = Scope::new(bindings);
    let mut rc = RenderCtx {
        base: base.map(Path::to_path_buf),
        include_stack: Vec::new(),
        current_path: source_path.map(Path::to_path_buf),
        current_include_parent: None,
    };
    let mut spans = if collect_provenance {
        Some(Vec::new())
    } else {
        None
    };
    render_nodes(&nodes, &mut scope, &mut rc, &mut out, spans.as_mut()).map_err(|mut e| {
        if e.path.is_none() {
            e.path = source_path.map(Path::to_path_buf);
        }
        e
    })?;
    Ok((out, spans.unwrap_or_default()))
}

// =========================================================================
// Errors
// =========================================================================

#[derive(Debug, Clone)]
pub(crate) struct TemplateError {
    pub path: Option<PathBuf>,
    pub line: usize,
    pub col: usize,
    pub kind: String,
}

impl TemplateError {
    fn new(line: usize, col: usize, msg: impl Into<String>) -> Self {
        Self {
            path: None,
            line,
            col,
            kind: msg.into(),
        }
    }

    pub(crate) fn message(&self) -> String {
        let p = self
            .path
            .as_ref()
            .map(|p| format!("{} ", p.display()))
            .unwrap_or_default();
        format!("{}at {}:{}: {}", p, self.line, self.col, self.kind)
    }
}

impl From<TemplateError> for VmError {
    fn from(e: TemplateError) -> Self {
        VmError::Thrown(VmValue::String(Rc::from(e.message())))
    }
}

// =========================================================================
// Tokenization (source → coarse token stream)
// =========================================================================

#[derive(Debug, Clone)]
enum Token {
    /// Literal text between directives.
    Text {
        content: String,
        /// `{{-` on the following directive — trim trailing whitespace of this text.
        trim_right: bool,
        /// `-}}` on the preceding directive — trim leading whitespace of this text.
        trim_left: bool,
    },
    /// Directive body (content between `{{` / `}}`, with `-` markers stripped).
    Directive {
        body: String,
        line: usize,
        col: usize,
    },
    /// Verbatim content of a `{{ raw }}..{{ endraw }}` block.
    Raw(String),
}

fn tokenize(src: &str) -> Result<Vec<Token>, TemplateError> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut cursor = 0;
    let mut pending_trim_left = false;
    let len = bytes.len();

    while cursor < len {
        // Look for the next `{{`.
        let open = find_from(src, cursor, "{{");
        let text_end = open.unwrap_or(len);
        let raw_text = &src[cursor..text_end];

        let this_trim_left = pending_trim_left;
        pending_trim_left = false;

        let mut this_trim_right = false;
        if let Some(o) = open {
            // Inspect the directive start for a `-` trim marker.
            if o + 2 < len && bytes[o + 2] == b'-' {
                this_trim_right = true;
            }
        }

        if !raw_text.is_empty() || this_trim_left || this_trim_right {
            tokens.push(Token::Text {
                content: raw_text.to_string(),
                trim_right: this_trim_right,
                trim_left: this_trim_left,
            });
        }

        let Some(open) = open else {
            break;
        };

        // Position after `{{` (and optional `-`).
        let body_start = open + 2 + if this_trim_right { 1 } else { 0 };

        // Handle `{{# comment #}}`: comments are stripped outright.
        if body_start < len && bytes[body_start] == b'#' {
            // Scan for `#}}` — allowing an optional `-` trim marker before `}}`.
            let after_hash = body_start + 1;
            let Some(close_hash) = find_from(src, after_hash, "#}}") else {
                let (line, col) = line_col(src, open);
                return Err(TemplateError::new(line, col, "unterminated comment"));
            };
            cursor = close_hash + 3;
            // Comments do not consume trim markers that would otherwise apply —
            // but we already consumed the leading `-`. Keep it simple: comments
            // don't trim surrounding text.
            continue;
        }

        // Handle `{{ raw }}` specially: capture until `{{ endraw }}` verbatim.
        let body_trim_start = skip_ws(src, body_start);
        let raw_kw_end = body_trim_start + 3;
        if raw_kw_end <= len && &src[body_trim_start..raw_kw_end.min(len)] == "raw" && {
            // Ensure "raw" is its own token; next char must be whitespace or `}}` or `-}}`.
            let after = raw_kw_end;
            after >= len
                || bytes[after] == b' '
                || bytes[after] == b'\t'
                || bytes[after] == b'\n'
                || bytes[after] == b'\r'
                || (after + 1 < len && &src[after..after + 2] == "}}")
                || (after + 2 < len && &src[after..after + 3] == "-}}")
        } {
            // Find closing of this raw-open directive.
            let Some(dir_close) = find_from(src, raw_kw_end, "}}") else {
                let (line, col) = line_col(src, open);
                return Err(TemplateError::new(line, col, "unterminated directive"));
            };
            // Check trailing `-` on `}}`.
            let raw_body_start = dir_close + 2;
            let trim_after_open = dir_close > 0 && bytes[dir_close - 1] == b'-';
            let _ = trim_after_open; // Raw blocks don't honor whitespace trim.

            // Scan for `{{ endraw }}` or `{{-endraw-}}`, whitespace-tolerant.
            let (raw_end_open, raw_end_close) =
                find_endraw(src, raw_body_start).ok_or_else(|| {
                    let (line, col) = line_col(src, open);
                    TemplateError::new(line, col, "unterminated `{{ raw }}` block")
                })?;
            let raw_content = src[raw_body_start..raw_end_open].to_string();
            tokens.push(Token::Raw(raw_content));
            cursor = raw_end_close;
            continue;
        }

        // Standard directive: scan for `}}`, respecting quoted strings so a
        // `}}` inside `"..."` doesn't prematurely terminate.
        let (close_pos, trim_after) = find_directive_close(src, body_start).ok_or_else(|| {
            let (line, col) = line_col(src, open);
            TemplateError::new(line, col, "unterminated directive")
        })?;
        let body_end = if trim_after { close_pos - 1 } else { close_pos };
        let body = src[body_start..body_end].trim().to_string();
        let (line, col) = line_col(src, open);
        tokens.push(Token::Directive { body, line, col });
        cursor = close_pos + 2;
        pending_trim_left = trim_after;
    }

    Ok(tokens)
}

fn find_from(s: &str, from: usize, pat: &str) -> Option<usize> {
    s[from..].find(pat).map(|i| i + from)
}

fn skip_ws(s: &str, from: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = from;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

fn line_col(s: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in s.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Scan forward from `start` looking for an unquoted `}}`. Returns
/// `(offset_of_closing_braces, trim_marker_present)` where the trim marker
/// is the `-` immediately before the `}}`.
fn find_directive_close(s: &str, start: usize) -> Option<(usize, bool)> {
    let bytes = s.as_bytes();
    let mut i = start;
    let mut in_str = false;
    let mut str_quote = b'"';
    while i + 1 < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == str_quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_str = true;
            str_quote = b;
            i += 1;
            continue;
        }
        if b == b'}' && bytes[i + 1] == b'}' {
            let trim = i > start && bytes[i - 1] == b'-';
            return Some((i, trim));
        }
        i += 1;
    }
    None
}

/// Find the matching `{{ endraw }}` (whitespace- and trim-marker-tolerant),
/// returning `(directive_open_offset, directive_close_offset_exclusive)`.
fn find_endraw(s: &str, from: usize) -> Option<(usize, usize)> {
    let mut cursor = from;
    while let Some(open) = find_from(s, cursor, "{{") {
        let after = open + 2;
        let body_start = if s.as_bytes().get(after) == Some(&b'-') {
            after + 1
        } else {
            after
        };
        let body_trim_start = skip_ws(s, body_start);
        let close = find_directive_close(s, body_start)?;
        let body_end = if close.1 { close.0 - 1 } else { close.0 };
        let body = s[body_trim_start..body_end].trim();
        if body == "endraw" {
            return Some((open, close.0 + 2));
        }
        cursor = close.0 + 2;
    }
    None
}

// =========================================================================
// AST
// =========================================================================

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Expr {
        expr: Expr,
        line: usize,
        col: usize,
    },
    If {
        branches: Vec<(Expr, Vec<Node>)>,
        else_branch: Option<Vec<Node>>,
        line: usize,
        col: usize,
    },
    For {
        value_var: String,
        key_var: Option<String>,
        iter: Expr,
        body: Vec<Node>,
        empty: Option<Vec<Node>>,
        line: usize,
        col: usize,
    },
    Include {
        path: Expr,
        with: Option<Vec<(String, Expr)>>,
        line: usize,
        col: usize,
    },
    /// A legacy bare `{{ident}}` that should silently pass-through its source
    /// text on miss — preserves pre-v2 semantics for back-compat.
    LegacyBareInterp {
        ident: String,
    },
}

#[derive(Debug, Clone)]
enum Expr {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Path(Vec<PathSeg>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Filter(Box<Expr>, String, Vec<Expr>),
}

#[derive(Debug, Clone)]
enum PathSeg {
    Field(String),
    Index(i64),
    Key(String),
}

#[derive(Debug, Clone, Copy)]
enum UnOp {
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

// =========================================================================
// Parser (token stream → AST)
// =========================================================================

fn parse(src: &str) -> Result<Vec<Node>, TemplateError> {
    let tokens = tokenize(src)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let nodes = p.parse_block(&[])?;
    if p.pos < tokens.len() {
        // Unclosed block — shouldn't reach here; parse_block returns on EOF.
    }
    Ok(nodes)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn parse_block(&mut self, stops: &[&str]) -> Result<Vec<Node>, TemplateError> {
        let mut out = Vec::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::Text {
                    content,
                    trim_right,
                    trim_left,
                } => {
                    let mut s = content.clone();
                    if *trim_left {
                        s = trim_leading_line(&s);
                    }
                    if *trim_right {
                        s = trim_trailing_line(&s);
                    }
                    if !s.is_empty() {
                        out.push(Node::Text(s));
                    }
                    self.pos += 1;
                }
                Token::Raw(content) => {
                    if !content.is_empty() {
                        out.push(Node::Text(content.clone()));
                    }
                    self.pos += 1;
                }
                Token::Directive { body, line, col } => {
                    let (line, col) = (*line, *col);
                    let body = body.clone();
                    // Check for terminator tokens first — these are consumed by the caller.
                    let first_word = first_word(&body);
                    if stops.contains(&first_word) {
                        return Ok(out);
                    }
                    self.pos += 1;

                    if body == "end" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ end }}`"));
                    }
                    if body == "else" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ else }}`"));
                    }
                    if first_word == "elif" {
                        return Err(TemplateError::new(line, col, "unexpected `{{ elif }}`"));
                    }

                    if first_word == "if" {
                        let cond_src = body[2..].trim();
                        let cond = parse_expr(cond_src, line, col)?;
                        let node = self.parse_if(cond, line, col)?;
                        out.push(node);
                    } else if first_word == "for" {
                        let node = self.parse_for(body[3..].trim(), line, col)?;
                        out.push(node);
                    } else if first_word == "include" {
                        let node = parse_include(body[7..].trim(), line, col)?;
                        out.push(node);
                    } else if is_bare_ident(&body) {
                        out.push(Node::LegacyBareInterp { ident: body });
                    } else {
                        let expr = parse_expr(&body, line, col)?;
                        out.push(Node::Expr { expr, line, col });
                    }
                }
            }
        }
        Ok(out)
    }

    fn parse_if(
        &mut self,
        first_cond: Expr,
        line: usize,
        col: usize,
    ) -> Result<Node, TemplateError> {
        let mut branches = Vec::new();
        let mut else_branch = None;
        let mut cur_cond = first_cond;
        loop {
            let body = self.parse_block(&["end", "else", "elif"])?;
            branches.push((cur_cond, body));
            // Consume the terminator directive.
            let tok = self.peek().cloned();
            match tok {
                Some(Token::Directive {
                    body: tbody,
                    line: tline,
                    col: tcol,
                }) => {
                    let fw = first_word(&tbody);
                    self.pos += 1;
                    match fw {
                        "end" => break,
                        "else" => {
                            let eb = self.parse_block(&["end"])?;
                            else_branch = Some(eb);
                            // Consume `{{ end }}`.
                            match self.peek() {
                                Some(Token::Directive { body, .. }) if body == "end" => {
                                    self.pos += 1;
                                }
                                _ => {
                                    return Err(TemplateError::new(
                                        tline,
                                        tcol,
                                        "`{{ else }}` missing matching `{{ end }}`",
                                    ));
                                }
                            }
                            break;
                        }
                        "elif" => {
                            let cond = parse_expr(tbody[4..].trim(), tline, tcol)?;
                            cur_cond = cond;
                            continue;
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        "`{{ if }}` missing matching `{{ end }}`",
                    ));
                }
            }
        }
        Ok(Node::If {
            branches,
            else_branch,
            line,
            col,
        })
    }

    fn parse_for(&mut self, spec: &str, line: usize, col: usize) -> Result<Node, TemplateError> {
        // Accept "x in expr" or "k, v in expr".
        let (head, iter_src) = match split_once_keyword(spec, " in ") {
            Some(p) => p,
            None => return Err(TemplateError::new(line, col, "expected `in` in for-loop")),
        };
        let head = head.trim();
        let iter_src = iter_src.trim();
        let (value_var, key_var) = if let Some((a, b)) = head.split_once(',') {
            let a = a.trim().to_string();
            let b = b.trim().to_string();
            if !is_ident(&a) || !is_ident(&b) {
                return Err(TemplateError::new(line, col, "invalid for-loop variables"));
            }
            (b, Some(a)) // `k, v in dict` → value_var = v, key_var = k
        } else {
            if !is_ident(head) {
                return Err(TemplateError::new(line, col, "invalid for-loop variable"));
            }
            (head.to_string(), None)
        };
        let iter = parse_expr(iter_src, line, col)?;
        let body = self.parse_block(&["end", "else"])?;
        let (empty, _) = match self.peek().cloned() {
            Some(Token::Directive { body: tbody, .. }) => {
                let fw = first_word(&tbody);
                self.pos += 1;
                if fw == "end" {
                    (None, ())
                } else if fw == "else" {
                    let empty_body = self.parse_block(&["end"])?;
                    match self.peek() {
                        Some(Token::Directive { body, .. }) if body == "end" => {
                            self.pos += 1;
                        }
                        _ => {
                            return Err(TemplateError::new(
                                line,
                                col,
                                "`{{ else }}` missing matching `{{ end }}`",
                            ));
                        }
                    }
                    (Some(empty_body), ())
                } else {
                    unreachable!()
                }
            }
            _ => {
                return Err(TemplateError::new(
                    line,
                    col,
                    "`{{ for }}` missing matching `{{ end }}`",
                ));
            }
        };
        Ok(Node::For {
            value_var,
            key_var,
            iter,
            body,
            empty,
            line,
            col,
        })
    }
}

fn parse_include(spec: &str, line: usize, col: usize) -> Result<Node, TemplateError> {
    // "<path-expr>" or "<path-expr> with { k: v, ... }"
    let (path_src, with_src) = match split_once_keyword(spec, " with ") {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (spec.trim(), None),
    };
    let path = parse_expr(path_src, line, col)?;
    let with = if let Some(src) = with_src {
        Some(parse_dict_literal(src, line, col)?)
    } else {
        None
    };
    Ok(Node::Include {
        path,
        with,
        line,
        col,
    })
}

fn parse_dict_literal(
    src: &str,
    line: usize,
    col: usize,
) -> Result<Vec<(String, Expr)>, TemplateError> {
    let s = src.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return Err(TemplateError::new(
            line,
            col,
            "expected `{ ... }` after `with`",
        ));
    }
    let inner = &s[1..s.len() - 1];
    let mut pairs = Vec::new();
    for chunk in split_top_level(inner, ',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let (k, v) = match split_once_top_level(chunk, ':') {
            Some(p) => p,
            None => {
                return Err(TemplateError::new(
                    line,
                    col,
                    "expected `key: value` in include bindings",
                ));
            }
        };
        let k = k.trim();
        if !is_ident(k) {
            return Err(TemplateError::new(line, col, "invalid include binding key"));
        }
        let v = parse_expr(v.trim(), line, col)?;
        pairs.push((k.to_string(), v));
    }
    Ok(pairs)
}

fn first_word(s: &str) -> &str {
    s.split(|c: char| c.is_whitespace()).next().unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn is_bare_ident(s: &str) -> bool {
    // A single identifier with no dot/bracket/filter — used for back-compat
    // silent pass-through.
    is_ident(s)
}

fn trim_leading_line(s: &str) -> String {
    // Strip whitespace up to and including the first newline.
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        return s[i + 1..].to_string();
    }
    if i < bytes.len() && bytes[i] == b'\r' {
        if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return s[i + 2..].to_string();
        }
        return s[i + 1..].to_string();
    }
    // No trailing newline — strip leading spaces only.
    s[i..].to_string()
}

fn trim_trailing_line(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    if i > 0 && bytes[i - 1] == b'\n' {
        // Remove this newline and the trailing whitespace.
        let end = i - 1;
        let end = if end > 0 && bytes[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        return s[..end].to_string();
    }
    // No newline boundary — strip trailing spaces only.
    s[..i].to_string()
}

// ---- Expression parsing -------------------------------------------------

fn parse_expr(src: &str, line: usize, col: usize) -> Result<Expr, TemplateError> {
    let tokens = tokenize_expr(src, line, col)?;
    let mut p = ExprParser {
        toks: &tokens,
        pos: 0,
        line,
        col,
    };
    let e = p.parse_filter()?;
    if p.pos < tokens.len() {
        return Err(TemplateError::new(
            line,
            col,
            format!("unexpected token `{:?}` in expression", p.toks[p.pos]),
        ));
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
enum EToken {
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Comma,
    Colon,
    Pipe,
    Bang,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndKw,
    OrKw,
    NotKw,
    True,
    False,
    Nil,
}

fn tokenize_expr(src: &str, line: usize, col: usize) -> Result<Vec<EToken>, TemplateError> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                toks.push(EToken::LParen);
                i += 1;
            }
            b')' => {
                toks.push(EToken::RParen);
                i += 1;
            }
            b'[' => {
                toks.push(EToken::LBracket);
                i += 1;
            }
            b']' => {
                toks.push(EToken::RBracket);
                i += 1;
            }
            b'.' => {
                toks.push(EToken::Dot);
                i += 1;
            }
            b',' => {
                toks.push(EToken::Comma);
                i += 1;
            }
            b':' => {
                toks.push(EToken::Colon);
                i += 1;
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    toks.push(EToken::OrKw);
                    i += 2;
                } else {
                    toks.push(EToken::Pipe);
                    i += 1;
                }
            }
            b'&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    toks.push(EToken::AndKw);
                    i += 2;
                } else {
                    return Err(TemplateError::new(line, col, "unexpected `&`"));
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::BangEq);
                    i += 2;
                } else {
                    toks.push(EToken::Bang);
                    i += 1;
                }
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::EqEq);
                    i += 2;
                } else {
                    return Err(TemplateError::new(line, col, "unexpected `=` (use `==`)"));
                }
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::Le);
                    i += 2;
                } else {
                    toks.push(EToken::Lt);
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    toks.push(EToken::Ge);
                    i += 2;
                } else {
                    toks.push(EToken::Gt);
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = b;
                let start = i + 1;
                let mut j = start;
                let mut out = String::new();
                while j < bytes.len() && bytes[j] != quote {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        match bytes[j + 1] {
                            b'n' => out.push('\n'),
                            b't' => out.push('\t'),
                            b'r' => out.push('\r'),
                            b'\\' => out.push('\\'),
                            b'"' => out.push('"'),
                            b'\'' => out.push('\''),
                            c => out.push(c as char),
                        }
                        j += 2;
                        continue;
                    }
                    out.push(bytes[j] as char);
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(TemplateError::new(line, col, "unterminated string literal"));
                }
                toks.push(EToken::Str(out));
                i = j + 1;
            }
            b'0'..=b'9' | b'-'
                if b != b'-' || (i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                if bytes[i] == b'-' {
                    i += 1;
                }
                let mut is_float = false;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    if bytes[i] == b'.' {
                        // Only treat as float if followed by digit — otherwise it's a field access.
                        if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                            is_float = true;
                            i += 1;
                            continue;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                }
                let lex = &src[start..i];
                if is_float {
                    let v: f64 = lex.parse().map_err(|_| {
                        TemplateError::new(line, col, format!("invalid number `{lex}`"))
                    })?;
                    toks.push(EToken::Float(v));
                } else {
                    let v: i64 = lex.parse().map_err(|_| {
                        TemplateError::new(line, col, format!("invalid integer `{lex}`"))
                    })?;
                    toks.push(EToken::Int(v));
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &src[start..i];
                match word {
                    "true" => toks.push(EToken::True),
                    "false" => toks.push(EToken::False),
                    "nil" => toks.push(EToken::Nil),
                    "and" => toks.push(EToken::AndKw),
                    "or" => toks.push(EToken::OrKw),
                    "not" => toks.push(EToken::NotKw),
                    other => toks.push(EToken::Ident(other.to_string())),
                }
            }
            _ => {
                return Err(TemplateError::new(
                    line,
                    col,
                    format!("unexpected character `{}` in expression", b as char),
                ));
            }
        }
    }
    Ok(toks)
}

struct ExprParser<'a> {
    toks: &'a [EToken],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> ExprParser<'a> {
    fn peek(&self) -> Option<&EToken> {
        self.toks.get(self.pos)
    }
    fn eat(&mut self, t: &EToken) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn err(&self, m: impl Into<String>) -> TemplateError {
        TemplateError::new(self.line, self.col, m)
    }

    fn parse_filter(&mut self) -> Result<Expr, TemplateError> {
        let mut left = self.parse_or()?;
        while self.eat(&EToken::Pipe) {
            let name = match self.peek() {
                Some(EToken::Ident(n)) => n.clone(),
                _ => return Err(self.err("expected filter name after `|`")),
            };
            self.pos += 1;
            let mut args = Vec::new();
            if self.eat(&EToken::Colon) {
                loop {
                    let a = self.parse_or()?;
                    args.push(a);
                    if !self.eat(&EToken::Comma) {
                        break;
                    }
                }
            }
            left = Expr::Filter(Box::new(left), name, args);
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Expr, TemplateError> {
        let mut left = self.parse_and()?;
        while self.eat(&EToken::OrKw) {
            let right = self.parse_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, TemplateError> {
        let mut left = self.parse_not()?;
        while self.eat(&EToken::AndKw) {
            let right = self.parse_not()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, TemplateError> {
        if self.eat(&EToken::Bang) || self.eat(&EToken::NotKw) {
            let inner = self.parse_not()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(inner)));
        }
        self.parse_cmp()
    }

    fn parse_cmp(&mut self) -> Result<Expr, TemplateError> {
        let left = self.parse_unary()?;
        let op = match self.peek() {
            Some(EToken::EqEq) => Some(BinOp::Eq),
            Some(EToken::BangEq) => Some(BinOp::Neq),
            Some(EToken::Lt) => Some(BinOp::Lt),
            Some(EToken::Le) => Some(BinOp::Le),
            Some(EToken::Gt) => Some(BinOp::Gt),
            Some(EToken::Ge) => Some(BinOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let right = self.parse_unary()?;
            return Ok(Expr::Binary(op, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, TemplateError> {
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, TemplateError> {
        let tok = self
            .peek()
            .cloned()
            .ok_or_else(|| self.err("expected expression"))?;
        self.pos += 1;
        let base = match tok {
            EToken::Nil => Expr::Nil,
            EToken::True => Expr::Bool(true),
            EToken::False => Expr::Bool(false),
            EToken::Int(n) => Expr::Int(n),
            EToken::Float(f) => Expr::Float(f),
            EToken::Str(s) => Expr::Str(s),
            EToken::LParen => {
                let e = self.parse_or()?;
                if !self.eat(&EToken::RParen) {
                    return Err(self.err("expected `)`"));
                }
                e
            }
            EToken::Ident(name) => self.parse_path(name)?,
            EToken::Bang | EToken::NotKw => {
                let inner = self.parse_primary()?;
                Expr::Unary(UnOp::Not, Box::new(inner))
            }
            other => return Err(self.err(format!("unexpected token `{:?}`", other))),
        };
        Ok(base)
    }

    fn parse_path(&mut self, head: String) -> Result<Expr, TemplateError> {
        let mut segs = vec![PathSeg::Field(head)];
        loop {
            match self.peek() {
                Some(EToken::Dot) => {
                    self.pos += 1;
                    match self.peek().cloned() {
                        Some(EToken::Ident(n)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Field(n));
                        }
                        _ => return Err(self.err("expected identifier after `.`")),
                    }
                }
                Some(EToken::LBracket) => {
                    self.pos += 1;
                    match self.peek().cloned() {
                        Some(EToken::Int(n)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Index(n));
                        }
                        Some(EToken::Str(s)) => {
                            self.pos += 1;
                            segs.push(PathSeg::Key(s));
                        }
                        _ => return Err(self.err("expected integer or string inside `[...]`")),
                    }
                    if !self.eat(&EToken::RBracket) {
                        return Err(self.err("expected `]`"));
                    }
                }
                _ => break,
            }
        }
        Ok(Expr::Path(segs))
    }
}

// =========================================================================
// Evaluation
// =========================================================================

#[derive(Default, Debug, Clone)]
struct Scope<'a> {
    /// Root bindings passed by the caller.
    root: Option<&'a BTreeMap<String, VmValue>>,
    /// Override stack — pushed for `for`-loop variables and `include with`.
    overrides: Vec<BTreeMap<String, VmValue>>,
}

impl<'a> Scope<'a> {
    fn new(root: Option<&'a BTreeMap<String, VmValue>>) -> Self {
        Self {
            root,
            overrides: Vec::new(),
        }
    }

    fn lookup(&self, name: &str) -> Option<VmValue> {
        for layer in self.overrides.iter().rev() {
            if let Some(v) = layer.get(name) {
                return Some(v.clone());
            }
        }
        self.root.and_then(|m| m.get(name)).cloned()
    }

    fn push(&mut self, layer: BTreeMap<String, VmValue>) {
        self.overrides.push(layer);
    }

    fn pop(&mut self) {
        self.overrides.pop();
    }

    /// Materialize a flat BTreeMap merging root + all overrides. Used when
    /// passing a fresh snapshot into an included partial.
    fn flatten(&self) -> BTreeMap<String, VmValue> {
        let mut out = BTreeMap::new();
        if let Some(r) = self.root {
            for (k, v) in r.iter() {
                out.insert(k.clone(), v.clone());
            }
        }
        for layer in &self.overrides {
            for (k, v) in layer {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}

struct RenderCtx {
    base: Option<PathBuf>,
    include_stack: Vec<PathBuf>,
    current_path: Option<PathBuf>,
    /// When inside an `{% include %}`, this holds the include-call's
    /// span (in the *parent* template). Every span emitted during the
    /// recursive render points at this as its `parent_span`, so the
    /// IDE can walk a breadcrumb back through nested includes
    /// (#96). `None` at the top level.
    current_include_parent: Option<Box<PromptSourceSpan>>,
}

/// Template URI reported alongside every span — the absolute path of
/// the currently-rendering `.harn.prompt` file. Empty string when the
/// renderer doesn't know (inline template arg or synthetic snippet).
fn current_template_uri(rc: &RenderCtx) -> String {
    rc.current_path
        .as_deref()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn render_nodes(
    nodes: &[Node],
    scope: &mut Scope<'_>,
    rc: &mut RenderCtx,
    out: &mut String,
    mut spans: Option<&mut Vec<PromptSourceSpan>>,
) -> Result<(), TemplateError> {
    for n in nodes {
        render_node(n, scope, rc, out, spans.as_deref_mut())?;
    }
    Ok(())
}

fn render_node(
    node: &Node,
    scope: &mut Scope<'_>,
    rc: &mut RenderCtx,
    out: &mut String,
    mut spans: Option<&mut Vec<PromptSourceSpan>>,
) -> Result<(), TemplateError> {
    // Capture the output cursor before the node writes so we can
    // record the exact byte range it produced. Nodes that delegate to
    // `render_nodes` (If / For / Include) record a span only after
    // their children finish so the range covers everything.
    let start = out.len();
    match node {
        Node::Text(s) => {
            out.push_str(s);
            if let Some(spans) = spans.as_deref_mut() {
                // Text nodes don't carry their own line/col in the AST;
                // attribute them to the *start* of the next directive
                // (line/col 0 is a sentinel meaning "unknown"). The IDE
                // uses Text spans only to fill gaps between directive
                // spans, so column precision here is not load-bearing.
                spans.push(PromptSourceSpan {
                    template_line: 0,
                    template_col: 0,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Text,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
        Node::Expr { expr, line, col } => {
            let v = eval_expr(expr, scope, *line, *col)?;
            let rendered = display_value(&v);
            out.push_str(&rendered);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Expr,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: Some(truncate_for_preview(&rendered)),
                });
            }
        }
        Node::LegacyBareInterp { ident } => {
            let (rendered, preview) = match scope.lookup(ident) {
                Some(v) => {
                    let s = display_value(&v);
                    (s.clone(), Some(truncate_for_preview(&s)))
                }
                None => (format!("{{{{{ident}}}}}"), None),
            };
            out.push_str(&rendered);
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: 0,
                    template_col: 0,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::LegacyBareInterp,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: preview,
                });
            }
        }
        Node::If {
            branches,
            else_branch,
            line,
            col,
        } => {
            let mut matched = false;
            for (cond, body) in branches {
                let v = eval_expr(cond, scope, *line, *col)?;
                if truthy(&v) {
                    render_nodes(body, scope, rc, out, spans.as_deref_mut())?;
                    matched = true;
                    break;
                }
            }
            if !matched {
                if let Some(eb) = else_branch {
                    render_nodes(eb, scope, rc, out, spans.as_deref_mut())?;
                }
            }
            if let Some(spans) = spans.as_deref_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::If,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
        Node::For {
            value_var,
            key_var,
            iter,
            body,
            empty,
            line,
            col,
        } => {
            let v = eval_expr(iter, scope, *line, *col)?;
            let items: Vec<(VmValue, VmValue)> =
                iterable_items(&v).map_err(|m| TemplateError::new(*line, *col, m))?;
            if items.is_empty() {
                if let Some(eb) = empty {
                    render_nodes(eb, scope, rc, out, spans.as_deref_mut())?;
                }
            } else {
                let length = items.len() as i64;
                for (idx, (k, val)) in items.iter().enumerate() {
                    let mut layer: BTreeMap<String, VmValue> = BTreeMap::new();
                    layer.insert(value_var.clone(), val.clone());
                    if let Some(kv) = key_var {
                        layer.insert(kv.clone(), k.clone());
                    }
                    let mut loop_map: BTreeMap<String, VmValue> = BTreeMap::new();
                    loop_map.insert("index".into(), VmValue::Int(idx as i64 + 1));
                    loop_map.insert("index0".into(), VmValue::Int(idx as i64));
                    loop_map.insert("first".into(), VmValue::Bool(idx == 0));
                    loop_map.insert("last".into(), VmValue::Bool(idx as i64 == length - 1));
                    loop_map.insert("length".into(), VmValue::Int(length));
                    layer.insert("loop".into(), VmValue::Dict(Rc::new(loop_map)));
                    scope.push(layer);
                    let iter_start = out.len();
                    let res = render_nodes(body, scope, rc, out, spans.as_deref_mut());
                    scope.pop();
                    res?;
                    if let Some(spans) = spans.as_deref_mut() {
                        spans.push(PromptSourceSpan {
                            template_line: *line,
                            template_col: *col,
                            output_start: iter_start,
                            output_end: out.len(),
                            kind: PromptSpanKind::ForIteration,
                            parent_span: rc.current_include_parent.clone(),
                            template_uri: current_template_uri(rc),
                            bound_value: None,
                        });
                    }
                }
            }
        }
        Node::Include {
            path,
            with,
            line,
            col,
        } => {
            let path_val = eval_expr(path, scope, *line, *col)?;
            let path_str = match path_val {
                VmValue::String(s) => s.to_string(),
                other => {
                    return Err(TemplateError::new(
                        *line,
                        *col,
                        format!("include path must be a string (got {})", other.type_name()),
                    ))
                }
            };
            // Resolve relative to the including file's directory, falling back
            // to the asset-root resolver used by render(...).
            let resolved: PathBuf = if Path::new(&path_str).is_absolute() {
                PathBuf::from(&path_str)
            } else if let Some(base) = &rc.base {
                base.join(&path_str)
            } else {
                crate::stdlib::process::resolve_source_asset_path(&path_str)
            };
            let canonical = resolved.canonicalize().unwrap_or(resolved.clone());
            if rc.include_stack.iter().any(|p| p == &canonical) {
                let chain = rc
                    .include_stack
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" → ");
                return Err(TemplateError::new(
                    *line,
                    *col,
                    format!(
                        "circular include detected: {chain} → {}",
                        canonical.display()
                    ),
                ));
            }
            if rc.include_stack.len() > 32 {
                return Err(TemplateError::new(
                    *line,
                    *col,
                    "include depth exceeded (32 levels)",
                ));
            }
            let contents = std::fs::read_to_string(&resolved).map_err(|e| {
                TemplateError::new(
                    *line,
                    *col,
                    format!(
                        "failed to read included template {}: {e}",
                        resolved.display()
                    ),
                )
            })?;
            let new_base = resolved.parent().map(Path::to_path_buf);
            // Build child scope: flatten current + apply `with { }` overrides.
            let mut child_bindings = scope.flatten();
            if let Some(pairs) = with {
                for (k, e) in pairs {
                    let v = eval_expr(e, scope, *line, *col)?;
                    child_bindings.insert(k.clone(), v);
                }
            }
            let child_nodes = parse(&contents).map_err(|mut e| {
                if e.path.is_none() {
                    e.path = Some(resolved.clone());
                }
                e
            })?;
            let mut child_scope = Scope::new(Some(&child_bindings));
            let saved_base = rc.base.clone();
            let saved_current = rc.current_path.clone();
            let saved_parent = rc.current_include_parent.clone();
            // Build the include-call's own span (#96): points at the
            // include directive in the parent template. Every span
            // emitted inside the recursive render links back to this
            // as its parent_span, composing with any already-present
            // chain to give A → B → C breadcrumbs on nested includes.
            let include_call_span = PromptSourceSpan {
                template_line: *line,
                template_col: *col,
                output_start: start,
                output_end: start,
                kind: PromptSpanKind::Include,
                bound_value: None,
                parent_span: saved_parent.clone(),
                template_uri: current_template_uri(rc),
            };
            rc.base = new_base;
            rc.current_path = Some(resolved.clone());
            rc.current_include_parent = Some(Box::new(include_call_span));
            rc.include_stack.push(canonical);
            let res = render_nodes(
                &child_nodes,
                &mut child_scope,
                rc,
                out,
                spans.as_deref_mut(),
            );
            rc.include_stack.pop();
            rc.base = saved_base;
            rc.current_path = saved_current;
            rc.current_include_parent = saved_parent;
            res?;
            if let Some(spans) = spans.as_mut() {
                spans.push(PromptSourceSpan {
                    template_line: *line,
                    template_col: *col,
                    output_start: start,
                    output_end: out.len(),
                    kind: PromptSpanKind::Include,
                    parent_span: rc.current_include_parent.clone(),
                    template_uri: current_template_uri(rc),
                    bound_value: None,
                });
            }
        }
    }
    Ok(())
}

/// Cap a rendered value's preview at 80 chars so span records don't
/// carry kilobyte prompt chunks. The IDE can fetch the full text by
/// reading the rendered string at `output_start..output_end`.
fn truncate_for_preview(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let truncated: String = s.chars().take(MAX - 1).collect();
    format!("{truncated}…")
}

fn eval_expr(
    expr: &Expr,
    scope: &Scope<'_>,
    line: usize,
    col: usize,
) -> Result<VmValue, TemplateError> {
    match expr {
        Expr::Nil => Ok(VmValue::Nil),
        Expr::Bool(b) => Ok(VmValue::Bool(*b)),
        Expr::Int(n) => Ok(VmValue::Int(*n)),
        Expr::Float(f) => Ok(VmValue::Float(*f)),
        Expr::Str(s) => Ok(VmValue::String(Rc::from(s.as_str()))),
        Expr::Path(segs) => Ok(resolve_path(segs, scope)),
        Expr::Unary(UnOp::Not, inner) => {
            let v = eval_expr(inner, scope, line, col)?;
            Ok(VmValue::Bool(!truthy(&v)))
        }
        Expr::Binary(op, a, b) => {
            // Short-circuit boolean ops.
            match op {
                BinOp::And => {
                    let av = eval_expr(a, scope, line, col)?;
                    if !truthy(&av) {
                        return Ok(av);
                    }
                    return eval_expr(b, scope, line, col);
                }
                BinOp::Or => {
                    let av = eval_expr(a, scope, line, col)?;
                    if truthy(&av) {
                        return Ok(av);
                    }
                    return eval_expr(b, scope, line, col);
                }
                _ => {}
            }
            let av = eval_expr(a, scope, line, col)?;
            let bv = eval_expr(b, scope, line, col)?;
            Ok(apply_cmp(*op, &av, &bv))
        }
        Expr::Filter(inner, name, args) => {
            let v = eval_expr(inner, scope, line, col)?;
            let arg_vals = args
                .iter()
                .map(|e| eval_expr(e, scope, line, col))
                .collect::<Result<Vec<_>, _>>()?;
            apply_filter(name, &v, &arg_vals, line, col)
        }
    }
}

fn resolve_path(segs: &[PathSeg], scope: &Scope<'_>) -> VmValue {
    let mut cur: VmValue = match segs.first() {
        Some(PathSeg::Field(n)) => match scope.lookup(n) {
            Some(v) => v,
            None => return VmValue::Nil,
        },
        _ => return VmValue::Nil,
    };
    for seg in &segs[1..] {
        cur = match (seg, &cur) {
            (PathSeg::Field(n), VmValue::Dict(d)) => d.get(n).cloned().unwrap_or(VmValue::Nil),
            (PathSeg::Key(k), VmValue::Dict(d)) => d.get(k).cloned().unwrap_or(VmValue::Nil),
            (PathSeg::Index(i), VmValue::List(items)) => {
                let idx = if *i < 0 { items.len() as i64 + *i } else { *i };
                if idx < 0 || (idx as usize) >= items.len() {
                    VmValue::Nil
                } else {
                    items[idx as usize].clone()
                }
            }
            (PathSeg::Index(i), VmValue::String(s)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = if *i < 0 { chars.len() as i64 + *i } else { *i };
                if idx < 0 || (idx as usize) >= chars.len() {
                    VmValue::Nil
                } else {
                    VmValue::String(Rc::from(chars[idx as usize].to_string()))
                }
            }
            _ => VmValue::Nil,
        };
    }
    cur
}

fn truthy(v: &VmValue) -> bool {
    match v {
        VmValue::Nil => false,
        VmValue::Bool(b) => *b,
        VmValue::Int(n) => *n != 0,
        VmValue::Float(f) => *f != 0.0,
        VmValue::String(s) => !s.trim().is_empty(),
        VmValue::List(items) => !items.is_empty(),
        VmValue::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

fn apply_cmp(op: BinOp, a: &VmValue, b: &VmValue) -> VmValue {
    match op {
        BinOp::Eq => VmValue::Bool(values_equal(a, b)),
        BinOp::Neq => VmValue::Bool(!values_equal(a, b)),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ord = compare(a, b);
            match (op, ord) {
                (BinOp::Lt, Some(o)) => VmValue::Bool(o == std::cmp::Ordering::Less),
                (BinOp::Le, Some(o)) => VmValue::Bool(o != std::cmp::Ordering::Greater),
                (BinOp::Gt, Some(o)) => VmValue::Bool(o == std::cmp::Ordering::Greater),
                (BinOp::Ge, Some(o)) => VmValue::Bool(o != std::cmp::Ordering::Less),
                _ => VmValue::Bool(false),
            }
        }
        BinOp::And | BinOp::Or => unreachable!(),
    }
}

fn compare(a: &VmValue, b: &VmValue) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Some(x.cmp(y)),
        (VmValue::Float(x), VmValue::Float(y)) => x.partial_cmp(y),
        (VmValue::Int(x), VmValue::Float(y)) => (*x as f64).partial_cmp(y),
        (VmValue::Float(x), VmValue::Int(y)) => x.partial_cmp(&(*y as f64)),
        (VmValue::String(x), VmValue::String(y)) => Some(x.as_ref().cmp(y.as_ref())),
        _ => None,
    }
}

fn iterable_items(v: &VmValue) -> Result<Vec<(VmValue, VmValue)>, String> {
    match v {
        VmValue::List(items) => Ok(items
            .iter()
            .enumerate()
            .map(|(i, it)| (VmValue::Int(i as i64), it.clone()))
            .collect()),
        VmValue::Dict(d) => Ok(d
            .iter()
            .map(|(k, v)| (VmValue::String(Rc::from(k.as_str())), v.clone()))
            .collect()),
        VmValue::Set(items) => Ok(items
            .iter()
            .enumerate()
            .map(|(i, it)| (VmValue::Int(i as i64), it.clone()))
            .collect()),
        VmValue::Range(r) => {
            let mut out = Vec::new();
            let len = r.len();
            for i in 0..len {
                if let Some(v) = r.get(i) {
                    out.push((VmValue::Int(i), VmValue::Int(v)));
                }
            }
            Ok(out)
        }
        VmValue::Nil => Ok(Vec::new()),
        other => Err(format!(
            "cannot iterate over {} — expected list, dict, set, or range",
            other.type_name()
        )),
    }
}

fn display_value(v: &VmValue) -> String {
    match v {
        VmValue::Nil => String::new(), // empty string — don't render "nil" literal
        other => other.display(),
    }
}

// =========================================================================
// Filters
// =========================================================================

fn apply_filter(
    name: &str,
    v: &VmValue,
    args: &[VmValue],
    line: usize,
    col: usize,
) -> Result<VmValue, TemplateError> {
    let bad_arity = || {
        TemplateError::new(
            line,
            col,
            format!("filter `{name}` got wrong number of arguments"),
        )
    };
    let need = |n: usize, args: &[VmValue]| -> Result<(), TemplateError> {
        if args.len() == n {
            Ok(())
        } else {
            Err(bad_arity())
        }
    };
    let str_of = |v: &VmValue| -> String { display_value(v) };
    match name {
        "upper" => {
            need(0, args)?;
            Ok(VmValue::String(Rc::from(str_of(v).to_uppercase())))
        }
        "lower" => {
            need(0, args)?;
            Ok(VmValue::String(Rc::from(str_of(v).to_lowercase())))
        }
        "trim" => {
            need(0, args)?;
            Ok(VmValue::String(Rc::from(str_of(v).trim())))
        }
        "capitalize" => {
            need(0, args)?;
            let s = str_of(v);
            let mut out = String::with_capacity(s.len());
            let mut chars = s.chars();
            if let Some(c) = chars.next() {
                out.extend(c.to_uppercase());
            }
            for c in chars {
                out.extend(c.to_lowercase());
            }
            Ok(VmValue::String(Rc::from(out)))
        }
        "title" => {
            need(0, args)?;
            let s = str_of(v);
            let mut out = String::with_capacity(s.len());
            let mut at_start = true;
            for c in s.chars() {
                if c.is_whitespace() {
                    at_start = true;
                    out.push(c);
                } else if at_start {
                    out.extend(c.to_uppercase());
                    at_start = false;
                } else {
                    out.extend(c.to_lowercase());
                }
            }
            Ok(VmValue::String(Rc::from(out)))
        }
        "length" => {
            need(0, args)?;
            let n: i64 = match v {
                VmValue::String(s) => s.chars().count() as i64,
                VmValue::List(items) => items.len() as i64,
                VmValue::Set(items) => items.len() as i64,
                VmValue::Dict(d) => d.len() as i64,
                VmValue::Range(r) => r.len(),
                VmValue::Nil => 0,
                other => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        format!("`length` not defined for {}", other.type_name()),
                    ))
                }
            };
            Ok(VmValue::Int(n))
        }
        "first" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => items.first().cloned().unwrap_or(VmValue::Nil),
                VmValue::Set(items) => items.first().cloned().unwrap_or(VmValue::Nil),
                VmValue::String(s) => s
                    .chars()
                    .next()
                    .map(|c| VmValue::String(Rc::from(c.to_string())))
                    .unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            })
        }
        "last" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => items.last().cloned().unwrap_or(VmValue::Nil),
                VmValue::Set(items) => items.last().cloned().unwrap_or(VmValue::Nil),
                VmValue::String(s) => s
                    .chars()
                    .last()
                    .map(|c| VmValue::String(Rc::from(c.to_string())))
                    .unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            })
        }
        "reverse" => {
            need(0, args)?;
            Ok(match v {
                VmValue::List(items) => {
                    let mut out: Vec<VmValue> = items.as_ref().clone();
                    out.reverse();
                    VmValue::List(Rc::new(out))
                }
                VmValue::String(s) => {
                    VmValue::String(Rc::from(s.chars().rev().collect::<String>()))
                }
                _ => v.clone(),
            })
        }
        "join" => {
            need(1, args)?;
            let sep = str_of(&args[0]);
            let parts: Vec<String> = match v {
                VmValue::List(items) => items.iter().map(str_of).collect(),
                VmValue::Set(items) => items.iter().map(str_of).collect(),
                VmValue::String(s) => return Ok(VmValue::String(s.clone())),
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        format!("`join` requires a list (got {})", v.type_name()),
                    ))
                }
            };
            Ok(VmValue::String(Rc::from(parts.join(&sep))))
        }
        "default" => {
            need(1, args)?;
            if truthy(v) {
                Ok(v.clone())
            } else {
                Ok(args[0].clone())
            }
        }
        "json" => {
            if args.len() > 1 {
                return Err(bad_arity());
            }
            let pretty = args.first().map(truthy).unwrap_or(false);
            let jv = crate::llm::helpers::vm_value_to_json(v);
            let s = if pretty {
                serde_json::to_string_pretty(&jv)
            } else {
                serde_json::to_string(&jv)
            }
            .map_err(|e| TemplateError::new(line, col, format!("json serialization: {e}")))?;
            Ok(VmValue::String(Rc::from(s)))
        }
        "indent" => {
            if args.is_empty() || args.len() > 2 {
                return Err(bad_arity());
            }
            let n = match &args[0] {
                VmValue::Int(n) => (*n).max(0) as usize,
                _ => {
                    return Err(TemplateError::new(
                        line,
                        col,
                        "`indent` requires an integer width",
                    ))
                }
            };
            let indent_first = args.get(1).map(truthy).unwrap_or(false);
            let pad: String = " ".repeat(n);
            let s = str_of(v);
            let mut out = String::with_capacity(s.len() + n * 4);
            for (i, line) in s.split('\n').enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                if !line.is_empty() && (i > 0 || indent_first) {
                    out.push_str(&pad);
                }
                out.push_str(line);
            }
            Ok(VmValue::String(Rc::from(out)))
        }
        "lines" => {
            need(0, args)?;
            let s = str_of(v);
            let list: Vec<VmValue> = s
                .split('\n')
                .map(|p| VmValue::String(Rc::from(p)))
                .collect();
            Ok(VmValue::List(Rc::new(list)))
        }
        "escape_md" => {
            need(0, args)?;
            let s = str_of(v);
            let mut out = String::with_capacity(s.len() + 8);
            for c in s.chars() {
                match c {
                    '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+'
                    | '-' | '.' | '!' | '|' | '<' | '>' => {
                        out.push('\\');
                        out.push(c);
                    }
                    _ => out.push(c),
                }
            }
            Ok(VmValue::String(Rc::from(out)))
        }
        "replace" => {
            need(2, args)?;
            let s = str_of(v);
            let from = str_of(&args[0]);
            let to = str_of(&args[1]);
            Ok(VmValue::String(Rc::from(s.replace(&from, &to))))
        }
        other => Err(TemplateError::new(
            line,
            col,
            format!("unknown filter `{other}`"),
        )),
    }
}

// =========================================================================
// Small helpers for token/expr splitting
// =========================================================================

fn split_top_level(s: &str, delim: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == delim && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

fn split_once_top_level(s: &str, delim: char) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == delim && depth == 0 => {
                return Some((&s[..i], &s[i + 1..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_once_keyword<'a>(s: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    // Match `kw` only at top level, outside strings and bracket groups.
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = '"';
    let bytes = s.as_bytes();
    let kw_bytes = kw.as_bytes();
    let mut i = 0;
    while i + kw_bytes.len() <= bytes.len() {
        let b = bytes[i] as char;
        if in_str {
            if b == '\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            '"' | '\'' => {
                in_str = true;
                quote = b;
                i += 1;
                continue;
            }
            '(' | '[' | '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            ')' | ']' | '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth == 0 && &bytes[i..i + kw_bytes.len()] == kw_bytes {
            return Some((&s[..i], &s[i + kw_bytes.len()..]));
        }
        i += 1;
    }
    None
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, VmValue)]) -> BTreeMap<String, VmValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(Rc::from(v))
    }

    fn render(tpl: &str, b: &BTreeMap<String, VmValue>) -> String {
        render_template_result(tpl, Some(b), None, None).unwrap()
    }

    fn render_with_spans(
        tpl: &str,
        b: &BTreeMap<String, VmValue>,
    ) -> (String, Vec<PromptSourceSpan>) {
        render_template_with_provenance(tpl, Some(b), None, None, true).unwrap()
    }

    #[test]
    fn bare_interp() {
        let b = dict(&[("name", s("Alice"))]);
        assert_eq!(render("hi {{name}}!", &b), "hi Alice!");
    }

    #[test]
    fn provenance_expr_span_matches_output_range() {
        // Use dotted-path and filter forms so the parser emits
        // `Expr` (not `LegacyBareInterp`) — different kinds carry
        // different span types in the provenance map.
        let mut user = BTreeMap::new();
        user.insert("name".to_string(), s("alice"));
        let b = dict(&[
            ("user", VmValue::Dict(Rc::new(user))),
            ("count", VmValue::Int(42)),
        ]);
        let (out, spans) =
            render_with_spans("hello {{ user.name }} ({{ count | default: 0 }})", &b);
        assert_eq!(out, "hello alice (42)");

        let expr_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == PromptSpanKind::Expr)
            .collect();
        assert_eq!(expr_spans.len(), 2);

        // Every expr span's output range must slice back to the
        // rendered value it produced — the property the IDE relies on.
        let user_span = expr_spans
            .iter()
            .find(|s| &out[s.output_start..s.output_end] == "alice")
            .expect("user expr span");
        assert!(user_span.template_line >= 1);
        assert_eq!(user_span.bound_value.as_deref(), Some("alice"));

        let count_span = expr_spans
            .iter()
            .find(|s| &out[s.output_start..s.output_end] == "42")
            .expect("count expr span");
        assert_eq!(count_span.bound_value.as_deref(), Some("42"));
    }

    #[test]
    fn provenance_legacy_bare_interp_span_tracked() {
        let b = dict(&[("name", s("Alice"))]);
        let (out, spans) = render_with_spans("hi {{name}}!", &b);
        assert_eq!(out, "hi Alice!");

        let bare = spans
            .iter()
            .find(|s| s.kind == PromptSpanKind::LegacyBareInterp)
            .expect("legacy bare span");
        assert_eq!(&out[bare.output_start..bare.output_end], "Alice");
        assert_eq!(bare.bound_value.as_deref(), Some("Alice"));
    }

    #[test]
    fn provenance_includes_loop_iterations() {
        let b = dict(&[(
            "items",
            VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")])),
        )]);
        let tpl = "{{for x in items}}[{{x}}]{{end}}";
        let (out, spans) = render_with_spans(tpl, &b);
        assert_eq!(out, "[a][b][c]");
        let iter_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == PromptSpanKind::ForIteration)
            .collect();
        assert_eq!(iter_spans.len(), 3);
        // Each iteration span should slice to its bracketed item.
        let slices: Vec<&str> = iter_spans
            .iter()
            .map(|s| &out[s.output_start..s.output_end])
            .collect();
        assert_eq!(slices, ["[a]", "[b]", "[c]"]);
    }

    #[test]
    fn provenance_preview_is_truncated() {
        // Long expression values shouldn't balloon the span record.
        // Use the dotted form so it parses as Expr rather than legacy.
        let mut wrap = BTreeMap::new();
        wrap.insert("val".to_string(), s(&"x".repeat(500)));
        let b = dict(&[("blob", VmValue::Dict(Rc::new(wrap)))]);
        let (_, spans) = render_with_spans("{{blob.val}}", &b);
        let expr = spans
            .iter()
            .find(|s| s.kind == PromptSpanKind::Expr)
            .expect("expr span");
        let preview = expr.bound_value.as_deref().unwrap();
        assert!(preview.chars().count() <= 80, "preview too long: {preview}");
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn provenance_off_returns_empty_spans() {
        let b = dict(&[("x", s("y"))]);
        let (_, spans) =
            render_template_with_provenance("{{x}}", Some(&b), None, None, false).unwrap();
        assert!(spans.is_empty());
    }

    #[test]
    fn bare_interp_missing_passthrough() {
        let b = dict(&[]);
        assert_eq!(render("hi {{name}}!", &b), "hi {{name}}!");
    }

    #[test]
    fn legacy_if_truthy() {
        let b = dict(&[("x", VmValue::Bool(true))]);
        assert_eq!(render("{{if x}}yes{{end}}", &b), "yes");
    }

    #[test]
    fn legacy_if_falsey() {
        let b = dict(&[("x", VmValue::Bool(false))]);
        assert_eq!(render("{{if x}}yes{{end}}", &b), "");
    }

    #[test]
    fn if_else() {
        let b = dict(&[("x", VmValue::Bool(false))]);
        assert_eq!(render("{{if x}}A{{else}}B{{end}}", &b), "B");
    }

    #[test]
    fn if_elif_else() {
        let b = dict(&[("n", VmValue::Int(2))]);
        let tpl = "{{if n == 1}}one{{elif n == 2}}two{{elif n == 3}}three{{else}}many{{end}}";
        assert_eq!(render(tpl, &b), "two");
    }

    #[test]
    fn for_loop_basic() {
        let items = VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")]));
        let b = dict(&[("xs", items)]);
        assert_eq!(render("{{for x in xs}}{{x}},{{end}}", &b), "a,b,c,");
    }

    #[test]
    fn for_loop_vars() {
        let items = VmValue::List(Rc::new(vec![s("a"), s("b")]));
        let b = dict(&[("xs", items)]);
        let tpl = "{{for x in xs}}{{loop.index}}:{{x}}{{if !loop.last}},{{end}}{{end}}";
        assert_eq!(render(tpl, &b), "1:a,2:b");
    }

    #[test]
    fn for_empty_else() {
        let b = dict(&[("xs", VmValue::List(Rc::new(vec![])))]);
        assert_eq!(render("{{for x in xs}}A{{else}}empty{{end}}", &b), "empty");
    }

    #[test]
    fn for_dict_kv() {
        let mut d: BTreeMap<String, VmValue> = BTreeMap::new();
        d.insert("a".into(), VmValue::Int(1));
        d.insert("b".into(), VmValue::Int(2));
        let b = dict(&[("m", VmValue::Dict(Rc::new(d)))]);
        assert_eq!(
            render("{{for k, v in m}}{{k}}={{v}};{{end}}", &b),
            "a=1;b=2;"
        );
    }

    #[test]
    fn nested_path() {
        let mut inner: BTreeMap<String, VmValue> = BTreeMap::new();
        inner.insert("name".into(), s("Alice"));
        let b = dict(&[("user", VmValue::Dict(Rc::new(inner)))]);
        assert_eq!(render("{{user.name}}", &b), "Alice");
    }

    #[test]
    fn list_index() {
        let b = dict(&[("xs", VmValue::List(Rc::new(vec![s("a"), s("b"), s("c")])))]);
        assert_eq!(render("{{xs[1]}}", &b), "b");
    }

    #[test]
    fn filter_upper() {
        let b = dict(&[("n", s("alice"))]);
        assert_eq!(render("{{n | upper}}", &b), "ALICE");
    }

    #[test]
    fn filter_default() {
        let b = dict(&[("n", s(""))]);
        assert_eq!(render("{{n | default: \"anon\"}}", &b), "anon");
    }

    #[test]
    fn filter_join() {
        let b = dict(&[("xs", VmValue::List(Rc::new(vec![s("a"), s("b")])))]);
        assert_eq!(render("{{xs | join: \", \"}}", &b), "a, b");
    }

    #[test]
    fn comparison_ops() {
        let b = dict(&[("n", VmValue::Int(5))]);
        assert_eq!(render("{{if n > 3}}big{{end}}", &b), "big");
        assert_eq!(render("{{if n >= 5 and n < 10}}ok{{end}}", &b), "ok");
    }

    #[test]
    fn bool_not() {
        let b = dict(&[("x", VmValue::Bool(false))]);
        assert_eq!(render("{{if not x}}yes{{end}}", &b), "yes");
        assert_eq!(render("{{if !x}}yes{{end}}", &b), "yes");
    }

    #[test]
    fn raw_block() {
        let b = dict(&[]);
        assert_eq!(
            render("A {{ raw }}{{not-a-directive}}{{ endraw }} B", &b),
            "A {{not-a-directive}} B"
        );
    }

    #[test]
    fn comment_stripped() {
        let b = dict(&[("x", s("hi"))]);
        assert_eq!(render("A{{# hidden #}}B{{x}}", &b), "ABhi");
    }

    #[test]
    fn whitespace_trim() {
        let b = dict(&[("x", s("v"))]);
        // Trailing -}} eats newline after it; leading {{- eats newline before it.
        let tpl = "line1\n  {{- x -}}  \nline2";
        assert_eq!(render(tpl, &b), "line1vline2");
    }

    #[test]
    fn filter_json() {
        let b = dict(&[(
            "x",
            VmValue::Dict(Rc::new({
                let mut m = BTreeMap::new();
                m.insert("a".into(), VmValue::Int(1));
                m
            })),
        )]);
        assert_eq!(render("{{x | json}}", &b), r#"{"a":1}"#);
    }

    #[test]
    fn error_unterminated_if() {
        let b = dict(&[("x", VmValue::Bool(true))]);
        let r = render_template_result("{{if x}}open", Some(&b), None, None);
        assert!(r.is_err());
    }

    #[test]
    fn error_unknown_filter() {
        let b = dict(&[("x", s("a"))]);
        let r = render_template_result("{{x | bogus}}", Some(&b), None, None);
        assert!(r.is_err());
    }

    #[test]
    fn include_with() {
        use std::fs;
        let dir = tempdir();
        let partial = dir.join("p.prompt");
        fs::write(&partial, "[{{name}}]").unwrap();
        let parent = dir.join("main.prompt");
        fs::write(
            &parent,
            r#"hello {{ include "p.prompt" with { name: who } }}!"#,
        )
        .unwrap();
        let b = dict(&[("who", s("world"))]);
        let src = fs::read_to_string(&parent).unwrap();
        let out = render_template_result(&src, Some(&b), Some(&dir), Some(&parent)).unwrap();
        assert_eq!(out, "hello [world]!");
    }

    #[test]
    fn include_propagates_parent_span_chain() {
        use std::fs;
        let dir = tempdir();
        // Three-level include chain: top → mid → leaf. Every span
        // emitted inside `leaf` must chain back through mid's include
        // call to top's include call so the IDE can render a
        // breadcrumb.
        let leaf = dir.join("leaf.prompt");
        fs::write(&leaf, "LEAF:{{v}}").unwrap();
        let mid = dir.join("mid.prompt");
        fs::write(&mid, r#"MID:{{ include "leaf.prompt" }}"#).unwrap();
        let top = dir.join("top.prompt");
        fs::write(&top, r#"TOP:{{ include "mid.prompt" }}"#).unwrap();
        let b = dict(&[("v", s("ok"))]);
        let src = fs::read_to_string(&top).unwrap();
        let (rendered, spans) =
            render_template_with_provenance(&src, Some(&b), Some(&dir), Some(&top), true).unwrap();
        assert_eq!(rendered, "TOP:MID:LEAF:ok");

        // Locate the interpolation span for `{{v}}` — it lives at
        // depth 2 (inside leaf, inside mid, inside top). Its
        // parent_span chain must be length 2: leaf's span has mid's
        // include as parent, which has top's include as grandparent.
        // Bare `{{ident}}` is LegacyBareInterp, not Expr.
        let leaf_expr = spans
            .iter()
            .find(|s| {
                matches!(
                    s.kind,
                    PromptSpanKind::Expr | PromptSpanKind::LegacyBareInterp
                ) && s.parent_span.is_some()
            })
            .expect("interpolation span emitted");
        let mid_parent = leaf_expr
            .parent_span
            .as_deref()
            .expect("leaf span must have mid's include as parent");
        assert_eq!(mid_parent.kind, PromptSpanKind::Include);
        let top_parent = mid_parent
            .parent_span
            .as_deref()
            .expect("mid's include must chain up to top's include");
        assert_eq!(top_parent.kind, PromptSpanKind::Include);
        assert!(top_parent.parent_span.is_none(), "chain bottoms out at top");

        // templateUri on each level points at the authoring file so
        // IDE breadcrumbs can open the right source.
        assert!(leaf_expr.template_uri.ends_with("leaf.prompt"));
        assert!(mid_parent.template_uri.ends_with("mid.prompt"));
        assert!(top_parent.template_uri.ends_with("top.prompt"));
    }

    #[test]
    fn include_cycle_detected() {
        use std::fs;
        let dir = tempdir();
        let a = dir.join("a.prompt");
        let b = dir.join("b.prompt");
        fs::write(&a, r#"A{{ include "b.prompt" }}"#).unwrap();
        fs::write(&b, r#"B{{ include "a.prompt" }}"#).unwrap();
        let src = fs::read_to_string(&a).unwrap();
        let r = render_template_result(&src, None, Some(&dir), Some(&a));
        assert!(r.is_err());
        assert!(r.unwrap_err().kind.contains("circular include"));
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!("harn-tpl-{}", nanoid()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn nanoid() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
}
