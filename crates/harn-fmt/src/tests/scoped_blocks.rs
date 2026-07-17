use crate::format_source;

use super::assert_roundtrip;

#[test]
fn mutex_blocks_round_trip() {
    assert_roundtrip("pipeline default(task) { mutex { log(1) } }");

    let keyed = "pipeline default(task) { mutex(\"acct\") { log(1) } }";
    let formatted = format_source(keyed).unwrap();
    assert!(formatted.contains("mutex(\"acct\")"), "{formatted}");
    assert_roundtrip(keyed);
}

#[test]
fn explicit_lexical_block_round_trips() {
    let source = "pipeline default(task) { block { const x = 1\nlog(x) } }";
    let formatted = format_source(source).unwrap();
    assert!(formatted.contains("block {\n"), "{formatted}");
    assert_roundtrip(source);
}
