use crate::{format_source, format_source_opts, FmtOptions};

use super::{assert_roundtrip, meaning};

#[test]
fn closure_body_preserves_nested_control_flow_comments_and_width() {
    let source = r#"fn build() {
  const callback = { value ->
    let choices: list<string> = ["first", "second", "third"]
    const fallback = value
    if value > 0 {
      // positive branch
      log(choices)
    } else if value == 0 {
      log(fallback)
      // zero branch tail
    } else {
      // negative branch
      log("negative")
    }
    for item in choices {
      log(item)
    }
    while ready {
      try {
        work()
        // try tail
      } catch (error) {
        recover(error)
      } finally {
        cleanup()
        // finally tail
      }
    }
  }
}
"#;
    assert_eq!(format_source(source).unwrap(), source);
    let original = harn_parser::parse_source(source).unwrap();
    for width in [40, 60, 100] {
        let options = FmtOptions {
            line_width: width,
            ..FmtOptions::default()
        };
        let formatted = format_source_opts(source, &options).unwrap();
        assert!(
            formatted.lines().all(|line| line.len() <= width),
            "nested statement exceeded width {width}:\n{formatted}"
        );
        assert_eq!(format_source_opts(&formatted, &options).unwrap(), formatted);
        let reparsed = harn_parser::parse_source(&formatted).unwrap();
        assert_eq!(meaning(&original), meaning(&reparsed));
    }
}

#[test]
fn closure_body_preserves_nested_declarations_and_eval_summary() {
    let source = r#"fn build() {
  const callback = { ->
    fn nested(value: int) -> int {
      return value
    }
    tool probe(value: int) -> int {
      return value
    }
    skill summarize {
      description "Summarize"
      prompt "Summarize the result"
    }
    eval_pack checks "nested-checks" {
      cases: [{id: "one"}]
      for item in cases {
        log(item.id)
      }
      summarize {
        log(checks.id)
      }
    }
  }
}
"#;
    assert_eq!(format_source(source).unwrap(), source);
    assert_roundtrip(source);
}
