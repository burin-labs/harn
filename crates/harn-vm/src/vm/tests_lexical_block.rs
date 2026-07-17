use crate::compiler::CompilerOptions;
use crate::VmValue;

use super::tests_runtime::{run_harn, run_harn_result_display_with_options};

#[test]
fn lexical_block_runs_cleanup_before_the_outer_statement() {
    let (_, result) = run_harn(
        r#"
        let events = []
        block {
          defer { events = events + ["cleanup"] }
          events = events + ["body"]
        }
        events = events + ["outer"]
        if events != ["body", "cleanup", "outer"] {
          throw "lexical block cleanup ran out of order"
        }
        "#,
    );
    assert!(matches!(result, VmValue::Nil));
}

#[test]
fn lexical_block_auto_drops_owned_bindings_on_exit() {
    let error = run_harn_result_display_with_options(
        r#"
        let alias = nil
        block {
          const ch: owned<channel> = channel("lexical-block-drop", 1)
          alias = ch
        }
        send(alias, "after exit")
        "#,
        CompilerOptions::optimized(),
    )
    .unwrap_err();
    assert!(error.contains("ChannelClosed"), "{error}");
}
