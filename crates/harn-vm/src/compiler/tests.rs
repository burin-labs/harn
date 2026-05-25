use super::*;
use crate::chunk::{Chunk, Constant};
use harn_lexer::Lexer;
use harn_parser::Parser;

fn compile_source(source: &str) -> Chunk {
    compile_source_with_options(source, CompilerOptions::optimized())
}

fn compile_source_with_options(source: &str, options: CompilerOptions) -> Chunk {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    Compiler::with_options(options).compile(&program).unwrap()
}

fn disasm_opcodes(disasm: &str) -> Vec<&str> {
    disasm
        .lines()
        .filter_map(|line| {
            line.split_once("] ")
                .and_then(|(_, rest)| rest.split_whitespace().next())
        })
        .collect()
}

fn string_constant_count(chunk: &Chunk, value: &str) -> usize {
    chunk
        .constants
        .iter()
        .filter(|constant| matches!(constant, Constant::String(text) if text == value))
        .count()
}

#[test]
fn test_compile_arithmetic() {
    let chunk = compile_source_with_options(
        "pipeline test(task) { let x = 2 + 3 }",
        CompilerOptions::without_optimizations(),
    );
    assert!(!chunk.code.is_empty());
    assert!(chunk.constants.contains(&Constant::Int(2)));
    assert!(chunk.constants.contains(&Constant::Int(3)));
}

#[test]
fn test_compile_typed_int_loop_ops() {
    let chunk = compile_source(
        "pipeline test(task) {
  var i = 0
  var total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("LESS_INT"));
    assert!(disasm.contains("ADD_INT"));
    assert!(disasm.contains("MUL_INT"));
    assert!(disasm.contains("SUB_INT"));
}

#[test]
fn test_compile_typed_float_ops() {
    let chunk = compile_source(
        "pipeline test(task) {
  let a = 1.0
  let b = 2.0
  let c = a + b
  log(c < 4.0)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("ADD_FLOAT"));
    assert!(disasm.contains("LESS_FLOAT"));
}

#[test]
fn test_compile_typed_equality_ops() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  let a = true
  let b = false
  let left = "a"
  let right = "b"
  log(a == b)
  log(left != right)
}"#,
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("EQUAL_BOOL"));
    assert!(disasm.contains("NOT_EQUAL_STRING"));
}

#[test]
fn test_compile_generic_ops_for_overloaded_or_mixed_cases() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  let left = "a"
  let right = "b"
  let one = 1
  let two = 2.0
  let xs = [1]
  let ys = [2]
  log(left + right)
  log(one + two)
  log(xs + ys)
}"#,
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("ADD"));
    assert!(!disasm.contains("ADD_INT"));
    assert!(!disasm.contains("ADD_FLOAT"));
}

#[test]
fn test_optimizer_folds_scalar_constants() {
    let chunk = compile_source("pipeline test(task) { log(2 + 3 * 4) }");
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk.constants.contains(&Constant::Int(14)));
    assert!(!opcodes.contains(&"ADD_INT"));
    assert!(!opcodes.contains(&"MUL_INT"));
    assert!(!opcodes.contains(&"ADD"));
    assert!(!opcodes.contains(&"MUL"));
}

#[test]
fn test_optimizer_escape_hatch_preserves_unoptimized_bytecode() {
    let chunk = compile_source_with_options(
        "pipeline test(task) { log(2 + 3 * 4) }",
        CompilerOptions::without_optimizations(),
    );
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk.constants.contains(&Constant::Int(2)));
    assert!(chunk.constants.contains(&Constant::Int(3)));
    assert!(chunk.constants.contains(&Constant::Int(4)));
    assert!(opcodes.contains(&"MUL"));
    assert!(opcodes.contains(&"ADD"));
}

#[test]
fn test_optimizer_folds_literal_collections_and_strings() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  log("ha" * 2)
  log([1] + [2, 3])
  log({a: 1} + {b: 2})
}"#,
    );
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk
        .constants
        .contains(&Constant::String("haha".to_string())));
    assert!(!opcodes.contains(&"ADD"));
    assert!(!opcodes.contains(&"MUL"));
}

#[test]
fn test_compiler_reuses_string_constants_within_chunk() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  log("same")
  log("same")
  let row = {status: "same"}
  log(row.status)
}"#,
    );

    assert_eq!(string_constant_count(&chunk, "same"), 1);
    assert_eq!(string_constant_count(&chunk, "status"), 1);
}

#[test]
fn test_compiler_reuses_string_constants_per_nested_chunk() {
    let chunk = compile_source(
        r#"fn inner() {
  log("nested")
  log("nested")
}

pipeline test(task) {
  inner()
}"#,
    );
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name == "inner")
        .expect("inner function should compile");

    assert_eq!(string_constant_count(&function.chunk, "nested"), 1);
}

#[test]
fn test_optimizer_keeps_runtime_erroring_arithmetic_unfolded() {
    let chunk = compile_source("pipeline test(task) { log(1 / 0) }");
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(opcodes.contains(&"DIV_INT"));
}

#[test]
fn test_optimizer_keeps_large_allocations_unfolded() {
    let chunk = compile_source(r#"pipeline test(task) { log("x" * 1000000) }"#);
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(opcodes.contains(&"MUL"));
}

#[test]
fn test_compile_function_call() {
    let chunk = compile_source("pipeline test(task) { log(42) }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CALL_BUILTIN"));
    assert!(disasm.contains("\"log\""));
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
fn test_compile_locals_to_slots() {
    let chunk = compile_source(
        "pipeline test(task) {
  let a = 1
  var i = 0
  while i < 3 {
    i = i + a
  }
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("DEF_LOCAL_SLOT"));
    assert!(disasm.contains("GET_LOCAL_SLOT"));
    assert!(disasm.contains("SET_LOCAL_SLOT"));
    assert!(!disasm.contains("GET_VAR"));
    assert!(!disasm.contains("SET_VAR"));
}

#[test]
fn test_compile_function_params_to_slots() {
    let chunk = compile_source(
        "pipeline test(task) {
  fn add(a, b = 1) {
    return a + b
  }
  log(add(2))
}",
    );
    let disasm = chunk.functions[0].chunk.disassemble("add");
    assert!(disasm.contains("GET_LOCAL_SLOT"));
    assert!(disasm.contains("DEF_LOCAL_SLOT"));
    assert!(!disasm.contains("GET_VAR"));
}

#[test]
fn test_compile_closure() {
    let chunk = compile_source("pipeline test(task) { let f = { x -> x * 2 } }");
    assert!(!chunk.functions.is_empty());
    assert_eq!(
        chunk.functions[0].param_names().collect::<Vec<_>>(),
        vec!["x"]
    );
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
    let chunk = compile_source_with_options(
        "pipeline test(task) { log(2 + 3) }",
        CompilerOptions::without_optimizations(),
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CONSTANT"));
    assert!(disasm.contains("ADD"));
    assert!(disasm.contains("CALL"));
}

#[test]
fn test_compile_discard_bindings_do_not_define_underscore() {
    let chunk = compile_source(
        r#"
pipeline test(task) {
  let _ = 1
  let [_, keep, _] = [10, 20, 30]
  let {drop: _, keep_dict} = {drop: 1, keep_dict: 2}
  for (_, value) in [pair("left", "right")] {
    log(value)
  }
  log(keep)
  log(keep_dict)
}
"#,
    );

    assert!(
        !chunk.constants.contains(&Constant::String("_".to_string())),
        "discard bindings should not emit a named `_` slot: {:?}",
        chunk.constants
    );
}
