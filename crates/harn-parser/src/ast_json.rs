//! Stable JSON projections for parser-facing tooling.
//!
//! The core AST keeps its Rust-native shape for compiler consumers.
//! This module owns the CLI/tooling projection so `harn parse --json`
//! and `harn tokens --json` can evolve under explicit schema versions.

use harn_lexer::{Span, Token, TokenKind};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::SNode;

pub const AST_JSON_SCHEMA_VERSION: u32 = 1;
pub const TOKEN_JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AstJsonProgram {
    pub kind: &'static str,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub body: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenJson {
    pub kind: &'static str,
    pub lexeme: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

pub fn program_to_json(nodes: &[SNode]) -> AstJsonProgram {
    AstJsonProgram {
        kind: "Program",
        schema_version: AST_JSON_SCHEMA_VERSION,
        body: nodes.iter().map(node_to_json).collect(),
    }
}

pub fn node_to_json(node: &SNode) -> Value {
    let raw = serde_json::to_value(node).expect("AST node should serialize to JSON");
    normalize_value(raw)
}

pub fn tokens_to_json(source: &str, tokens: &[Token]) -> Vec<TokenJson> {
    tokens
        .iter()
        .map(|token| token_to_json(source, token))
        .collect()
}

pub fn token_to_json(source: &str, token: &Token) -> TokenJson {
    TokenJson {
        kind: token_kind_name(&token.kind),
        lexeme: lexeme_for_span(source, token.span),
        start: token.span.start,
        end: token.span.end,
        line: token.span.line,
        column: token.span.column,
    }
}

pub fn token_kind_name(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::Pipeline => "Pipeline",
        TokenKind::Extends => "Extends",
        TokenKind::Override => "Override",
        TokenKind::Let => "Let",
        TokenKind::Var => "Var",
        TokenKind::If => "If",
        TokenKind::Else => "Else",
        TokenKind::For => "For",
        TokenKind::In => "In",
        TokenKind::Match => "Match",
        TokenKind::Retry => "Retry",
        TokenKind::Parallel => "Parallel",
        TokenKind::Return => "Return",
        TokenKind::Import => "Import",
        TokenKind::True => "True",
        TokenKind::False => "False",
        TokenKind::Nil => "Nil",
        TokenKind::Try => "Try",
        TokenKind::Catch => "Catch",
        TokenKind::Throw => "Throw",
        TokenKind::Finally => "Finally",
        TokenKind::Fn => "Fn",
        TokenKind::Spawn => "Spawn",
        TokenKind::While => "While",
        TokenKind::TypeKw => "TypeKw",
        TokenKind::Enum => "Enum",
        TokenKind::EvalPack => "EvalPack",
        TokenKind::Struct => "Struct",
        TokenKind::Interface => "Interface",
        TokenKind::Emit => "Emit",
        TokenKind::Pub => "Pub",
        TokenKind::From => "From",
        TokenKind::To => "To",
        TokenKind::Tool => "Tool",
        TokenKind::Exclusive => "Exclusive",
        TokenKind::Guard => "Guard",
        TokenKind::Require => "Require",
        TokenKind::Deadline => "Deadline",
        TokenKind::Defer => "Defer",
        TokenKind::Yield => "Yield",
        TokenKind::Mutex => "Mutex",
        TokenKind::Break => "Break",
        TokenKind::Continue => "Continue",
        TokenKind::Select => "Select",
        TokenKind::Impl => "Impl",
        TokenKind::Skill => "Skill",
        TokenKind::RequestApproval => "RequestApproval",
        TokenKind::DualControl => "DualControl",
        TokenKind::AskUser => "AskUser",
        TokenKind::EscalateTo => "EscalateTo",
        TokenKind::Identifier(_) => "Identifier",
        TokenKind::StringLiteral(_) => "StringLiteral",
        TokenKind::InterpolatedString(_) => "InterpolatedString",
        TokenKind::RawStringLiteral(_) => "RawStringLiteral",
        TokenKind::IntLiteral(_) => "IntLiteral",
        TokenKind::FloatLiteral(_) => "FloatLiteral",
        TokenKind::DurationLiteral(_) => "DurationLiteral",
        TokenKind::Eq => "Eq",
        TokenKind::Neq => "Neq",
        TokenKind::And => "And",
        TokenKind::Or => "Or",
        TokenKind::Pipe => "Pipe",
        TokenKind::NilCoal => "NilCoal",
        TokenKind::Pow => "Pow",
        TokenKind::QuestionDot => "QuestionDot",
        TokenKind::Arrow => "Arrow",
        TokenKind::Lte => "Lte",
        TokenKind::Gte => "Gte",
        TokenKind::PlusAssign => "PlusAssign",
        TokenKind::MinusAssign => "MinusAssign",
        TokenKind::StarAssign => "StarAssign",
        TokenKind::SlashAssign => "SlashAssign",
        TokenKind::PercentAssign => "PercentAssign",
        TokenKind::Assign => "Assign",
        TokenKind::Not => "Not",
        TokenKind::Dot => "Dot",
        TokenKind::Plus => "Plus",
        TokenKind::Minus => "Minus",
        TokenKind::Star => "Star",
        TokenKind::Slash => "Slash",
        TokenKind::Percent => "Percent",
        TokenKind::Lt => "Lt",
        TokenKind::Gt => "Gt",
        TokenKind::Question => "Question",
        TokenKind::Bar => "Bar",
        TokenKind::Amp => "Amp",
        TokenKind::LBrace => "LBrace",
        TokenKind::RBrace => "RBrace",
        TokenKind::LParen => "LParen",
        TokenKind::RParen => "RParen",
        TokenKind::LBracket => "LBracket",
        TokenKind::RBracket => "RBracket",
        TokenKind::Comma => "Comma",
        TokenKind::Colon => "Colon",
        TokenKind::Semicolon => "Semicolon",
        TokenKind::At => "At",
        TokenKind::LineComment { .. } => "LineComment",
        TokenKind::BlockComment { .. } => "BlockComment",
        TokenKind::Newline => "Newline",
        TokenKind::Eof => "Eof",
    }
}

fn lexeme_for_span(source: &str, span: Span) -> String {
    source
        .get(span.start..span.end)
        .unwrap_or_default()
        .to_string()
}

fn normalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(normalize_value).collect()),
        Value::Object(map) => normalize_object(map),
        other => other,
    }
}

fn normalize_object(mut map: Map<String, Value>) -> Value {
    if is_span_map(&map) {
        return normalize_span_map(map);
    }

    if is_spanned_map(&map) {
        return normalize_spanned_map(map);
    }

    if map.len() == 1 {
        let key = map.keys().next().cloned().expect("map has one key");
        if is_variant_name(&key) {
            let fields = map.remove(&key).expect("variant payload is present");
            return tagged_value(key, normalize_value(fields));
        }
    }

    for value in map.values_mut() {
        let raw = std::mem::take(value);
        *value = normalize_value(raw);
    }
    Value::Object(map)
}

fn normalize_spanned_map(mut map: Map<String, Value>) -> Value {
    let node = map.remove("node").expect("spanned node is present");
    let span = map.remove("span").expect("spanned span is present");
    let mut normalized = normalize_enum_value(node);
    if let Value::Object(object) = &mut normalized {
        object.insert("span".to_string(), normalize_value(span));
    }
    normalized
}

fn normalize_enum_value(value: Value) -> Value {
    match value {
        Value::Object(mut map) if map.len() == 1 => {
            let key = map.keys().next().cloned().expect("map has one key");
            let fields = map.remove(&key).expect("variant payload is present");
            tagged_value(key, normalize_value(fields))
        }
        Value::String(kind) if is_variant_name(&kind) => tagged_value(kind, Value::Null),
        other => normalize_value(other),
    }
}

fn tagged_value(kind: String, fields: Value) -> Value {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::String(kind));
    object.insert("fields".to_string(), fields);
    Value::Object(object)
}

fn normalize_span_map(mut map: Map<String, Value>) -> Value {
    if let Some(end_line) = map.remove("end_line") {
        map.insert("endLine".to_string(), end_line);
    }
    Value::Object(map)
}

fn is_spanned_map(map: &Map<String, Value>) -> bool {
    map.len() == 2 && map.contains_key("node") && map.contains_key("span")
}

fn is_span_map(map: &Map<String, Value>) -> bool {
    map.len() == 5
        && map.contains_key("start")
        && map.contains_key("end")
        && map.contains_key("line")
        && map.contains_key("column")
        && map.contains_key("end_line")
}

fn is_variant_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use harn_lexer::Lexer;

    use super::*;
    use crate::parse_source;

    #[test]
    fn program_json_projects_tagged_nodes_with_spans() {
        let source = "pipeline main(task) {\n  return 1\n}\n";
        let program = parse_source(source).expect("parse");

        let json = program_to_json(&program);

        assert_eq!(json.kind, "Program");
        assert_eq!(json.schema_version, AST_JSON_SCHEMA_VERSION);
        let pipeline = &json.body[0];
        assert_eq!(pipeline["kind"], "Pipeline");
        assert_eq!(pipeline["span"]["start"], 0);
        assert_eq!(pipeline["span"]["line"], 1);
        assert_eq!(pipeline["fields"]["name"], "main");
        assert_eq!(pipeline["fields"]["body"][0]["kind"], "ReturnStmt");
        assert_eq!(
            pipeline["fields"]["body"][0]["fields"]["value"]["kind"],
            "IntLiteral"
        );
    }

    #[test]
    fn token_json_preserves_byte_spans_and_lexemes() {
        let source = "let x = \"é\"\n";
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize_with_comments().expect("tokenize");

        let json = tokens_to_json(source, &tokens);
        let string = json
            .iter()
            .find(|token| token.kind == "StringLiteral")
            .expect("string literal token");

        let quote = source.find('"').expect("opening quote");
        assert_eq!(string.lexeme, "\"é\"");
        assert_eq!(string.start, quote);
        assert_eq!(string.end, quote + "\"é\"".len());
        assert_eq!(string.line, 1);
        assert_eq!(string.column, 9);
    }
}
