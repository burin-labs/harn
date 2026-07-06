use std::fs;
use std::process;

use harn_lexer::{Lexer, Token};
use harn_parser::ast_json::{
    self, AstJsonProgram, TokenJson, AST_JSON_SCHEMA_VERSION, TOKEN_JSON_SCHEMA_VERSION,
};
use harn_parser::parse_source;
use serde::Serialize;
use serde_json::json;

use crate::cli::{ParseArgs, TokensArgs};
use crate::json_envelope::{self, JsonEnvelope};

pub(crate) const PARSE_JSON_SCHEMA_VERSION: u32 = AST_JSON_SCHEMA_VERSION;
pub(crate) const TOKENS_JSON_SCHEMA_VERSION: u32 = TOKEN_JSON_SCHEMA_VERSION;

pub(crate) fn run_parse(args: &ParseArgs) -> Result<(), String> {
    let source = match fs::read_to_string(&args.path) {
        Ok(source) => source,
        Err(error) if args.json => json_error::<AstJsonProgram>(
            PARSE_JSON_SCHEMA_VERSION,
            "read_failed",
            format!("failed to read {}: {error}", args.path),
            &args.path,
        ),
        Err(error) => return Err(format!("failed to read {}: {error}", args.path)),
    };

    let program = match parse_source(&source) {
        Ok(program) => program,
        Err(error) if args.json => json_error::<AstJsonProgram>(
            PARSE_JSON_SCHEMA_VERSION,
            "parse_failed",
            error.to_string(),
            &args.path,
        ),
        Err(error) => return Err(error.to_string()),
    };

    if args.json {
        let envelope = JsonEnvelope::ok(
            PARSE_JSON_SCHEMA_VERSION,
            ast_json::program_to_json(&program),
        );
        println!("{}", json_envelope::to_string_pretty(&envelope));
    } else {
        println!("{program:#?}");
    }
    Ok(())
}

pub(crate) fn run_tokens(args: &TokensArgs) -> Result<(), String> {
    let source = match fs::read_to_string(&args.path) {
        Ok(source) => source,
        Err(error) if args.json => json_error::<Vec<TokenJson>>(
            TOKENS_JSON_SCHEMA_VERSION,
            "read_failed",
            format!("failed to read {}: {error}", args.path),
            &args.path,
        ),
        Err(error) => return Err(format!("failed to read {}: {error}", args.path)),
    };

    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize_with_comments() {
        Ok(tokens) => tokens,
        Err(error) if args.json => json_error::<Vec<TokenJson>>(
            TOKENS_JSON_SCHEMA_VERSION,
            "lex_failed",
            error.to_string(),
            &args.path,
        ),
        Err(error) => return Err(error.to_string()),
    };

    if args.json {
        let envelope = JsonEnvelope::ok(
            TOKENS_JSON_SCHEMA_VERSION,
            ast_json::tokens_to_json(&source, &tokens),
        );
        println!("{}", json_envelope::to_string_pretty(&envelope));
    } else {
        for token in &tokens {
            println!("{}", format_token_line(&source, token));
        }
    }
    Ok(())
}

fn format_token_line(source: &str, token: &Token) -> String {
    let lexeme = source
        .get(token.span.start..token.span.end)
        .unwrap_or_default();
    format!(
        "{:>4}:{:<3} {:>6}..{:<6} {:<22} {:?}",
        token.span.line,
        token.span.column,
        token.span.start,
        token.span.end,
        ast_json::token_kind_name(&token.kind),
        lexeme
    )
}

fn json_error<T: Serialize>(
    schema_version: u32,
    code: &'static str,
    message: String,
    path: &str,
) -> ! {
    let envelope =
        JsonEnvelope::<T>::err(schema_version, code, message).with_details(json!({ "path": path }));
    println!("{}", json_envelope::to_string_pretty(&envelope));
    process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_token_line_includes_span_kind_and_lexeme() {
        let source = "const x = 1\n";
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().expect("tokenize");

        let line = format_token_line(source, &tokens[0]);

        assert!(line.contains("1:1"));
        assert!(line.contains("0..3"));
        assert!(line.contains("Let"));
        assert!(line.contains("\"let\""));
    }
}
