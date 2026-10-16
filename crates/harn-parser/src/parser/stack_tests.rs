use super::{Parser, MAX_NESTING_DEPTH};
use crate::Node;
use harn_lexer::Lexer;

const STACK_BUDGET: usize = 4 * 1024 * 1024;

fn accepted_source(name: &str, source: &str, single_expression: bool) {
    let tokens = Lexer::new(source).tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    parser.record_stack_position();
    let program = if single_expression {
        vec![parser.parse_single_expression().unwrap()]
    } else {
        parser.parse().unwrap()
    };
    let used = parser.stack_extent.1 - parser.stack_extent.0;
    eprintln!("{name}: {used} bytes");
    assert_eq!(parser.deepest_nesting, MAX_NESTING_DEPTH, "{name}");
    assert!(used > 0, "measurement did not reach recursive parsing");
    assert!(used < STACK_BUDGET, "{name}: {used} bytes");
    let mut reached_leaf = false;
    crate::visit::walk_program(&program, &mut |node| {
        reached_leaf |= matches!(node.node, Node::IntLiteral(1));
    });
    assert!(reached_leaf, "{name}: parsed tree lost its leaf");
}

#[test]
fn expression_stack_budget() {
    // Use the owning stack contract so a regression fails its measurement
    // assertion instead of aborting the complete test process on overflow.
    crate::runtime_stack::spawn(|| {
        for (name, open, close) in [
            ("parentheses", "(", ")"),
            ("dicts", "{x:", "}"),
            ("lists", "[", "]"),
            ("calls", "f(", ")"),
            ("method calls", "x.f(", ")"),
            ("subscripts", "x[", "]"),
            ("slices", "x[:", "]"),
            ("structs", "Foo {x:", "}"),
            ("unary", "!", ""),
            ("exponents", "1 ** ", ""),
            ("ternaries", "true ? 1 : ", ""),
        ] {
            let source = format!(
                "{}1{}",
                open.repeat(MAX_NESTING_DEPTH),
                close.repeat(MAX_NESTING_DEPTH)
            );
            accepted_source(name, &source, true);

            let refused = format!(
                "{}1{}",
                open.repeat(MAX_NESTING_DEPTH + 1),
                close.repeat(MAX_NESTING_DEPTH + 1)
            );
            let tokens = Lexer::new(&refused).tokenize().unwrap();
            let mut parser = Parser::new(tokens);
            parser.record_stack_position();
            let error = parser
                .parse_single_expression()
                .expect_err("depth limit must refuse the source");
            assert!(
                error
                    .to_string()
                    .contains("parser nesting depth within 64 levels"),
                "{name}: {error}"
            );
            let used = parser.stack_extent.1 - parser.stack_extent.0;
            assert!(used < STACK_BUDGET, "{name} refusal: {used} bytes");
        }

        let source = format!(
            "fn nested() {{ return {}1{} }}",
            "{x:".repeat(MAX_NESTING_DEPTH - 1),
            "}".repeat(MAX_NESTING_DEPTH - 1)
        );
        accepted_source("dicts in a function return", &source, false);
    })
    .join()
    .unwrap();
}
