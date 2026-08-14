use std::cell::Cell;

use super::*;

fn diagnostic(
    file: &str,
    code: Code,
    repair_id: &str,
    expected_type: Option<TypeExpr>,
) -> RepairCandidate {
    RepairCandidate {
        file: file.to_string(),
        source: "test",
        severity: "warning",
        code,
        message: "test diagnostic".to_string(),
        unresolved_name: None,
        expected_type,
        span: Some(Span::with_offsets(4, 8, 1, 5)),
        repair: Repair {
            id: harn_parser::RepairId::from_owned(repair_id.to_string()),
            summary: "test repair".to_string(),
            safety: RepairSafety::ScopeLocal,
        },
        impact: RepairImpactWire::generic(),
        edits: Vec::new(),
    }
}

#[test]
fn diagnostic_index_normalizes_each_relevant_file_once() {
    let diagnostics = vec![
        diagnostic(
            "a.harn",
            Code::LintAmbientFsBuiltin,
            "bindings/thread-harness-fs",
            None,
        ),
        diagnostic(
            "a.harn",
            Code::ArgumentTypeMismatch,
            "bindings/prepend-capability-argument",
            Some(TypeExpr::Named("HarnessFs".to_string())),
        ),
        diagnostic(
            "b.harn",
            Code::LintAmbientClockBuiltin,
            "bindings/thread-harness-clock",
            None,
        ),
        diagnostic(
            "c.harn",
            Code::FormatterWouldReformat,
            "format/reformat",
            None,
        ),
    ];
    let normalizations = Cell::new(0);

    let index = diagnostic_index_with(&diagnostics, |path| {
        normalizations.set(normalizations.get() + 1);
        path.to_path_buf()
    });

    assert_eq!(normalizations.get(), 2);
    let indexed = index.get(Path::new("a.harn")).unwrap();
    assert_eq!(indexed.ambient_spans.len(), 1);
    assert_eq!(indexed.missing_capability_arguments.len(), 1);
    assert_eq!(index[Path::new("b.harn")].ambient_spans.len(), 1);
    assert!(!index.contains_key(Path::new("c.harn")));
}

#[test]
fn capability_carrier_alias_resolution_stops_at_cycles() {
    let aliases = BTreeMap::from([
        ("First".to_string(), TypeExpr::Named("Second".to_string())),
        ("Second".to_string(), TypeExpr::Named("First".to_string())),
    ]);

    assert_eq!(
        capability_carrier_kind(
            &TypeExpr::Named("First".to_string()),
            &aliases,
            &mut BTreeSet::new(),
        ),
        None
    );
}
