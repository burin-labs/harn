use super::*;

#[test]
fn exception_breakpoints_stop_for_declared_and_legacy_throws() {
    for declaration in ["", " throws string"] {
        let source = format!("fn fail(){declaration} {{ throw \"exception-probe\" }}\nfail()");
        for enabled in [false, true] {
            let mut dbg = Debugger::new();
            let (_dir, file) = write_temp_program("exception.harn", &source);
            dbg.handle_message(make_request(1, "initialize", None));
            dbg.handle_message(make_request(
                2,
                "launch",
                Some(json!({"program": file.to_string_lossy()})),
            ));
            let filters: Vec<&str> = if enabled { vec!["all"] } else { vec![] };
            dbg.handle_message(make_request(
                3,
                "setExceptionBreakpoints",
                Some(json!({"filters": filters})),
            ));
            let mut responses = dbg.handle_message(make_request(4, "configurationDone", None));
            for _ in 0..1000 {
                if !dbg.is_running() {
                    break;
                }
                responses.extend(dbg.step_running_vm());
            }
            assert!(!dbg.is_running(), "exception probe did not finish");
            let stopped = responses.iter().any(|response| {
                response.event.as_deref() == Some("stopped")
                    && response
                        .body
                        .as_ref()
                        .is_some_and(|body| body["reason"] == "exception")
            });
            assert_eq!(stopped, enabled, "declaration={declaration:?}");
            assert!(
                responses.iter().any(|response| {
                    response.event.as_deref() == Some("output")
                        && response.body.as_ref().is_some_and(|body| {
                            body["output"]
                                .as_str()
                                .is_some_and(|text| text.contains("exception-probe"))
                        })
                }),
                "throw was not reached: {responses:?}"
            );
        }
    }
}
