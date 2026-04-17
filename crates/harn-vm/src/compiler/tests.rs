use super::*;
use crate::chunk::{Chunk, Constant};
use harn_lexer::Lexer;
use harn_parser::Parser;

fn compile_source(source: &str) -> Chunk {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    Compiler::new().compile(&program).unwrap()
}

#[test]
fn test_compile_arithmetic() {
    let chunk = compile_source("pipeline test(task) { let x = 2 + 3 }");
    assert!(!chunk.code.is_empty());
    assert!(chunk.constants.contains(&Constant::Int(2)));
    assert!(chunk.constants.contains(&Constant::Int(3)));
}

#[test]
fn test_compile_function_call() {
    let chunk = compile_source("pipeline test(task) { log(42) }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CALL"));
}

#[test]
fn test_compile_if_else() {
    let chunk =
        compile_source(r#"pipeline test(task) { if true { log("yes") } else { log("no") } }"#);
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("JUMP_IF_FALSE"));
    assert!(disasm.contains("JUMP"));
}

#[test]
fn test_compile_while() {
    let chunk = compile_source("pipeline test(task) { var i = 0\n while i < 5 { i = i + 1 } }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("JUMP_IF_FALSE"));
    assert!(disasm.contains("JUMP"));
}

#[test]
fn test_compile_closure() {
    let chunk = compile_source("pipeline test(task) { let f = { x -> x * 2 } }");
    assert!(!chunk.functions.is_empty());
    assert_eq!(chunk.functions[0].params, vec!["x"]);
}

#[test]
fn test_compile_list() {
    let chunk = compile_source("pipeline test(task) { let a = [1, 2, 3] }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("BUILD_LIST"));
}

#[test]
fn test_compile_dict() {
    let chunk = compile_source(r#"pipeline test(task) { let d = {name: "test"} }"#);
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("BUILD_DICT"));
}

#[test]
fn test_disassemble() {
    let chunk = compile_source("pipeline test(task) { log(2 + 3) }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CONSTANT"));
    assert!(disasm.contains("ADD"));
    assert!(disasm.contains("CALL"));
}
