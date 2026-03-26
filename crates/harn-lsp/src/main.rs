use std::collections::HashMap;
use std::sync::Mutex;

use harn_lexer::{Lexer, LexerError};
use harn_parser::{Node, Parser, ParserError};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

/// Known builtin names for completion.
const BUILTINS: &[&str] = &[
    "log",
    "print",
    "println",
    "type_of",
    "to_string",
    "to_int",
    "to_float",
    "json_stringify",
    "json_parse",
    "env",
    "timestamp",
    "sleep",
    "read_file",
    "write_file",
    "exit",
    "regex_match",
    "regex_replace",
    "http_get",
    "http_post",
    "llm_call",
    "agent_loop",
    "await",
    "cancel",
    "spawn",
];

/// Known keywords for completion.
const KEYWORDS: &[&str] = &[
    "pipeline",
    "extends",
    "override",
    "let",
    "var",
    "if",
    "else",
    "for",
    "in",
    "match",
    "retry",
    "parallel",
    "parallel_map",
    "return",
    "import",
    "true",
    "false",
    "nil",
    "try",
    "catch",
    "throw",
    "fn",
    "spawn",
    "while",
];

struct HarnLsp {
    client: Client,
    documents: Mutex<HashMap<Url, String>>,
}

impl HarnLsp {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
    }

    /// Parse a document and return diagnostics.
    fn diagnose(&self, source: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        // Lex
        let mut lexer = Lexer::new(source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(lexer_error_to_diagnostic(&e));
                return diagnostics;
            }
        };

        // Parse
        let mut parser = Parser::new(tokens);
        if let Err(e) = parser.parse() {
            diagnostics.push(parser_error_to_diagnostic(&e));
        }

        diagnostics
    }

    /// Extract symbols (pipelines, functions, variables) from a document.
    fn extract_symbols(&self, source: &str) -> Vec<DocumentSymbol> {
        let mut lexer = Lexer::new(source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut parser = Parser::new(tokens);
        let nodes = match parser.parse() {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };

        let mut symbols = Vec::new();
        extract_symbols_from_nodes(&nodes, source, &mut symbols);
        symbols
    }

    /// Find the definition location of a symbol.
    fn find_definition(&self, source: &str, name: &str) -> Option<Range> {
        let lines: Vec<&str> = source.lines().collect();
        for (line_idx, line) in lines.iter().enumerate() {
            // Check for pipeline declaration
            if let Some(pos) = line.find(&format!("pipeline {name}")) {
                let col = pos + "pipeline ".len();
                return Some(Range {
                    start: Position::new(line_idx as u32, col as u32),
                    end: Position::new(line_idx as u32, (col + name.len()) as u32),
                });
            }
            // Check for fn declaration
            if let Some(pos) = line.find(&format!("fn {name}")) {
                let col = pos + "fn ".len();
                return Some(Range {
                    start: Position::new(line_idx as u32, col as u32),
                    end: Position::new(line_idx as u32, (col + name.len()) as u32),
                });
            }
            // Check for let/var binding
            for prefix in ["let ", "var "] {
                if let Some(pos) = line.find(&format!("{prefix}{name}")) {
                    let col = pos + prefix.len();
                    return Some(Range {
                        start: Position::new(line_idx as u32, col as u32),
                        end: Position::new(line_idx as u32, (col + name.len()) as u32),
                    });
                }
            }
        }
        None
    }

    /// Get the word at a given position.
    fn word_at_position(&self, source: &str, position: Position) -> Option<String> {
        let lines: Vec<&str> = source.lines().collect();
        let line = lines.get(position.line as usize)?;
        let col = position.character as usize;
        if col > line.len() {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut start = col;
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        let mut end = col;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }

        if start == end {
            return None;
        }
        Some(chars[start..end].iter().collect())
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for HarnLsp {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Harn LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let source = params.text_document.text.clone();
        self.documents
            .lock()
            .unwrap()
            .insert(uri.clone(), source.clone());

        let diagnostics = self.diagnose(&source);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            let source = change.text;
            self.documents
                .lock()
                .unwrap()
                .insert(uri.clone(), source.clone());

            let diagnostics = self.diagnose(&source);
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let source = match docs.get(uri) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        drop(docs);

        let mut items = Vec::new();

        // Add builtins
        for name in BUILTINS {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("builtin".to_string()),
                ..Default::default()
            });
        }

        // Add keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        // Add user-defined symbols
        let mut lexer = Lexer::new(&source);
        if let Ok(tokens) = lexer.tokenize() {
            let mut parser = Parser::new(tokens);
            if let Ok(nodes) = parser.parse() {
                collect_user_symbols(&nodes, &mut items);
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let source = match docs.get(uri) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        drop(docs);

        let word = match self.word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        if let Some(range) = self.find_definition(&source, &word) {
            Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range,
            })))
        } else {
            Ok(None)
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let source = match docs.get(uri) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        drop(docs);

        let symbols = self.extract_symbols(&source);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let source = match docs.get(uri) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        drop(docs);

        let word = match self.word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Check if it's a builtin
        if let Some(doc) = builtin_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check if it's a keyword
        if KEYWORDS.contains(&word.as_str()) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("**{word}** — Harn keyword"),
                }),
                range: None,
            }));
        }

        Ok(None)
    }
}

fn lexer_error_to_diagnostic(err: &LexerError) -> Diagnostic {
    let (message, line, col) = match err {
        LexerError::UnexpectedCharacter(ch, span) => (
            format!("Unexpected character '{ch}'"),
            span.line,
            span.column,
        ),
        LexerError::UnterminatedString(span) => {
            ("Unterminated string".to_string(), span.line, span.column)
        }
        LexerError::UnterminatedBlockComment(span) => (
            "Unterminated block comment".to_string(),
            span.line,
            span.column,
        ),
    };

    Diagnostic {
        range: Range {
            start: Position::new((line - 1) as u32, (col - 1) as u32),
            end: Position::new((line - 1) as u32, col as u32),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("harn".to_string()),
        message,
        ..Default::default()
    }
}

fn parser_error_to_diagnostic(err: &ParserError) -> Diagnostic {
    match err {
        ParserError::Unexpected {
            got,
            expected,
            line,
            column,
        } => Diagnostic {
            range: Range {
                start: Position::new((*line - 1) as u32, (*column - 1) as u32),
                end: Position::new((*line - 1) as u32, *column as u32),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Expected {expected}, got {got}"),
            ..Default::default()
        },
        ParserError::UnexpectedEof { expected } => Diagnostic {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(0, 1),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Unexpected end of file, expected {expected}"),
            ..Default::default()
        },
    }
}

#[allow(deprecated)]
fn extract_symbols_from_nodes(nodes: &[Node], source: &str, symbols: &mut Vec<DocumentSymbol>) {
    let lines: Vec<&str> = source.lines().collect();

    for node in nodes {
        match node {
            Node::Pipeline { name, .. } => {
                if let Some(range) = find_name_in_source(&lines, &format!("pipeline {name}"), name)
                {
                    symbols.push(DocumentSymbol {
                        name: name.clone(),
                        detail: Some("pipeline".to_string()),
                        kind: SymbolKind::FUNCTION,
                        range,
                        selection_range: range,
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
            }
            Node::FnDecl { name, .. } => {
                if let Some(range) = find_name_in_source(&lines, &format!("fn {name}"), name) {
                    symbols.push(DocumentSymbol {
                        name: name.clone(),
                        detail: Some("function".to_string()),
                        kind: SymbolKind::FUNCTION,
                        range,
                        selection_range: range,
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
            }
            _ => {}
        }
    }
}

fn find_name_in_source(lines: &[&str], pattern: &str, name: &str) -> Option<Range> {
    for (line_idx, line) in lines.iter().enumerate() {
        if let Some(pos) = line.find(pattern) {
            let col = pos + pattern.len() - name.len();
            return Some(Range {
                start: Position::new(line_idx as u32, col as u32),
                end: Position::new(line_idx as u32, (col + name.len()) as u32),
            });
        }
    }
    None
}

fn collect_user_symbols(nodes: &[Node], items: &mut Vec<CompletionItem>) {
    for node in nodes {
        match node {
            Node::Pipeline { name, body, .. } => {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some("pipeline".to_string()),
                    ..Default::default()
                });
                collect_user_symbols(body, items);
            }
            Node::FnDecl { name, body, .. } => {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some("function".to_string()),
                    ..Default::default()
                });
                collect_user_symbols(body, items);
            }
            Node::LetBinding { name, .. } | Node::VarBinding { name, .. } => {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
}

fn builtin_doc(name: &str) -> Option<String> {
    let doc = match name {
        "log" => "**log(value)** — Print value to stdout with `[harn]` prefix",
        "print" => "**print(value)** — Print value to stdout (no newline)",
        "println" => "**println(value)** — Print value to stdout with newline",
        "type_of" => "**type_of(value)** → string — Returns the type name",
        "to_string" => "**to_string(value)** → string — Convert to string",
        "to_int" => "**to_int(value)** → int — Convert to integer",
        "to_float" => "**to_float(value)** → float — Convert to float",
        "json_parse" => "**json_parse(text)** → value — Parse JSON string into Harn value",
        "json_stringify" => "**json_stringify(value)** → string — Convert value to JSON string",
        "env" => "**env(name)** → string | nil — Get environment variable",
        "timestamp" => "**timestamp()** → float — Unix timestamp in seconds",
        "sleep" => "**sleep(ms)** → nil — Async sleep for milliseconds",
        "read_file" => "**read_file(path)** → string — Read file contents",
        "write_file" => "**write_file(path, content)** → nil — Write string to file",
        "exit" => "**exit(code)** — Terminate process with exit code",
        "regex_match" => "**regex_match(pattern, text)** → list | nil — Find all regex matches",
        "regex_replace" => {
            "**regex_replace(pattern, replacement, text)** → string — Replace regex matches"
        }
        "http_get" => "**http_get(url)** → string — HTTP GET request",
        "http_post" => "**http_post(url, body, headers?)** → string — HTTP POST request",
        "llm_call" => "**llm_call(prompt, system?, options?)** → string — Call an LLM API\n\nOptions: `{provider, model, max_tokens}`",
        "agent_loop" => "**agent_loop(prompt, system?, options?)** → string — Agent loop with tool dispatch\n\nOptions: `{provider, model, persistent, max_iterations, max_nudges, nudge}`\n\nIn persistent mode, loop continues until `##DONE##` sentinel is output.",
        "await" => "**await(handle)** → value — Wait for spawned task to complete",
        "cancel" => "**cancel(handle)** → nil — Cancel a spawned task",
        _ => return None,
    };
    Some(doc.to_string())
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(HarnLsp::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}
