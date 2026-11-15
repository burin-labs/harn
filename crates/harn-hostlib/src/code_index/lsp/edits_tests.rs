use super::*;

#[test]
fn file_rename_requires_exact_admission_and_preceding_document_edits() {
    let old = Path::new("/project/src/Before.java");
    let new = Path::new("/project/src/After.java");
    let document = serde_json::json!({
        "textDocument": {"uri": path_to_uri(old), "version": 9}, "edits": []
    });
    let rename = serde_json::json!({
        "kind": "rename", "oldUri": path_to_uri(old), "newUri": path_to_uri(new)
    });
    let parse = |changes| {
        serde_json::from_value::<WorkspaceEdit>(serde_json::json!({
            "documentChanges": changes
        }))
        .unwrap()
    };
    let admitted = parse(vec![document.clone(), rename.clone()])
        .into_rename_file_edits(Some((old, new)))
        .unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[old].version, Some(9));
    assert_eq!(admitted[old].rename_to.as_deref(), Some(new));
    assert!(parse(vec![document.clone(), rename.clone()])
        .into_versioned_file_edits()
        .is_err());
    for changes in [
        vec![rename.clone(), document.clone()],
        vec![document.clone(), rename.clone(), rename.clone()],
        vec![
            document.clone(),
            rename.clone(),
            serde_json::json!({
                "textDocument": {"uri": path_to_uri(new), "version": null}, "edits": []
            }),
        ],
    ] {
        assert!(parse(changes)
            .into_rename_file_edits(Some((old, new)))
            .is_err());
    }
    for field in ["overwrite", "ignoreIfExists"] {
        let mut unsafe_rename = rename.clone();
        unsafe_rename["options"] = serde_json::json!({(field): true});
        assert!(parse(vec![document.clone(), unsafe_rename])
            .into_rename_file_edits(Some((old, new)))
            .is_err());
    }
    let mut annotated = rename.clone();
    annotated["annotationId"] = serde_json::json!("requires-approval");
    assert!(parse(vec![document.clone(), annotated])
        .into_rename_file_edits(Some((old, new)))
        .is_err());
    assert!(parse(vec![document, rename])
        .into_rename_file_edits(Some((old, Path::new("/other/After.java"))))
        .is_err());
}

#[test]
fn document_change_refuses_mixed_operation_tags_and_unnegotiated_annotations() {
    for value in [
        serde_json::json!({"documentChanges": [{
            "textDocument": {"uri": "file:///a.java", "version": 1}, "edits": [],
            "kind": "rename", "oldUri": "file:///a.java", "newUri": "file:///b.java"
        }]}),
        serde_json::json!({"documentChanges": [{
            "textDocument": {"uri": "file:///a.java", "version": 1},
            "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "newText": "b", "annotationId": "requires-approval"}]
        }]}),
    ] {
        assert!(serde_json::from_value::<WorkspaceEdit>(value).is_err());
    }
}

#[test]
fn document_edits_retain_versions_and_refuse_ambiguous_censuses() {
    let edit = serde_json::from_value::<WorkspaceEdit>(serde_json::json!({
        "documentChanges": [{"textDocument": {"uri": "file:///tmp/source.java", "version": 7}, "edits": []}]
    })).unwrap().into_versioned_file_edits().unwrap();
    assert_eq!(edit[&PathBuf::from("/tmp/source.java")].version, Some(7));
    for value in [
        serde_json::json!({"changes": {}, "documentChanges": []}),
        serde_json::json!({"documentChanges": [
            {"textDocument": {"uri": "file:///tmp/source.java", "version": 7}, "edits": []},
            {"textDocument": {"uri": "file:///tmp/source.java", "version": 8}, "edits": []}
        ]}),
        serde_json::json!({"changes": {"file:///tmp/source.java": [], "file://localhost/tmp/source.java": []}}),
    ] {
        assert!(serde_json::from_value::<WorkspaceEdit>(value)
            .unwrap()
            .into_versioned_file_edits()
            .is_err());
    }
}

#[test]
fn coalesced_java_edits_keep_literal_text_and_exact_reference_coverage() {
    let source = "class DocumentStore { DocumentStore value; String note = \"DocumentStore\"; // DocumentStore\n }";
    let replacement = "class DocumentLibrary { DocumentStore value; String note = \"DocumentStore\"; // DocumentStore\n }";
    let index = LineIndex::new(source);
    let edits = lower_edits(
        source,
        &[TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: index.position(source.len(), Encoding::Utf16),
            },
            new_text: replacement.into(),
        }],
        Encoding::Utf16,
    )
    .unwrap();
    let fine = identifier_replacements(
        source,
        &edits,
        crate::ast::Language::Java,
        "DocumentStore",
        "DocumentLibrary",
    )
    .unwrap();
    assert_eq!(
        fine.len(),
        1,
        "an unchanged same-name reference cannot hide inside a coalesced edit"
    );
    assert_eq!(
        super::super::super::refactor_core::splice(source, &fine),
        replacement
    );
    for altered in [
        replacement.replace("String", "Object"),
        replacement.replace("\"DocumentStore\"", "\"DocumentLibrary\""),
        replacement.replace("// DocumentStore", "// DocumentLibrary"),
    ] {
        let bad = [EditSpan {
            span: edits[0].span,
            before: source.into(),
            after: altered,
        }];
        assert!(identifier_replacements(
            source,
            &bad,
            crate::ast::Language::Java,
            "DocumentStore",
            "DocumentLibrary"
        )
        .is_err());
    }
}

#[test]
fn coalesced_identifier_projection_handles_prefix_names_and_unicode() {
    for (old, new, source, patched) in [
        ("A", "AA", "class A { A x; }", "class AA { A x; }"),
        ("AA", "A", "class AA { AA x; }", "class A { AA x; }"),
        (
            "Δelta",
            "ΔeltaLong",
            "class Δelta { Δelta x; }",
            "class ΔeltaLong { Δelta x; }",
        ),
    ] {
        let index = LineIndex::new(source);
        let edits = lower_edits(
            source,
            &[TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: index.position(source.len(), Encoding::Utf16),
                },
                new_text: patched.into(),
            }],
            Encoding::Utf16,
        )
        .unwrap();
        let fine =
            identifier_replacements(source, &edits, crate::ast::Language::Java, old, new).unwrap();
        assert_eq!(fine.len(), 1);
        assert_eq!(
            super::super::super::refactor_core::splice(source, &fine),
            patched
        );
    }
}

#[test]
fn java_identifier_projection_reaches_type_references_but_refuses_literal_edits() {
    let source = "class Before { Before value; String note = \"Before\"; /* Before */ }";
    let index = LineIndex::new(source);
    let make_edit = |start: usize, end: usize, after: &str| EditSpan {
        span: index.span(start, end),
        before: source[start..end].into(),
        after: after.into(),
    };
    let all = [make_edit(
        0,
        source.len(),
        "class After { After value; String note = \"Before\"; /* Before */ }",
    )];
    let edits =
        identifier_replacements(source, &all, crate::ast::Language::Java, "Before", "After")
            .unwrap();
    assert_eq!(
        edits.len(),
        2,
        "both declaration and type reference must be reached"
    );
    for start in [
        source.find("\"Before\"").unwrap() + 1,
        source.rfind("Before").unwrap(),
    ] {
        assert!(identifier_replacements(
            source,
            &[make_edit(start, start + 6, "After")],
            crate::ast::Language::Java,
            "Before",
            "After"
        )
        .is_err());
    }
}

#[test]
fn utf16_positions_round_trip_through_astral_characters() {
    let source = "let a = \"𝔸\"; Span\nnext Span\n";
    let index = LineIndex::new(source);
    let byte = source.find("Span").unwrap();
    let utf16 = index.position(byte, Encoding::Utf16);
    let utf8 = index.position(byte, Encoding::Utf8);
    assert_eq!(
        utf16.character + 2,
        utf8.character,
        "𝔸 is 2 UTF-16 units, 4 bytes"
    );
    assert_eq!(index.offset(utf16, Encoding::Utf16), Some(byte));
    assert_eq!(index.offset(utf8, Encoding::Utf8), Some(byte));
    // Mid-character positions do not name a byte.
    let inside = Position {
        line: 0,
        character: utf16.character - 4,
    };
    assert_eq!(index.offset(inside, Encoding::Utf16), None);
    assert_eq!(
        index.offset(
            Position {
                line: 9,
                character: 0
            },
            Encoding::Utf16
        ),
        None
    );
}

#[test]
fn lowering_rejects_overlap_and_reports_before_text() {
    let source = "struct Span;\nfn f(_: Span) {}\n";
    let edit = |line, start, end| TextEdit {
        range: Range {
            start: Position {
                line,
                character: start,
            },
            end: Position {
                line,
                character: end,
            },
        },
        new_text: "TextSpan".into(),
    };
    let spans = lower_edits(source, &[edit(1, 8, 12), edit(0, 7, 11)], Encoding::Utf16).unwrap();
    assert_eq!(spans.len(), 2);
    assert!(spans.iter().all(|span| span.before == "Span"));
    assert_eq!(spans[0].span.start_row, 0, "sorted by position");
    assert!(lower_edits(source, &[edit(0, 7, 11), edit(0, 9, 12)], Encoding::Utf16).is_err());
    assert!(lower_edits(source, &[edit(4, 0, 1)], Encoding::Utf16).is_err());
}

#[test]
fn uris_round_trip_spaces_and_unicode() {
    let path = Path::new("/tmp/a dir/ünï/lib.rs");
    let uri = path_to_uri(path);
    assert_eq!(uri, "file:///tmp/a%20dir/%C3%BCn%C3%AF/lib.rs");
    assert_eq!(uri_to_path(&uri).unwrap(), path);
    assert!(uri_to_path("untitled:Untitled-1").is_err());
    assert_eq!(
        uri_to_path("file://localhost/tmp/a").unwrap(),
        Path::new("/tmp/a")
    );
    for invalid in [
        "file://remote/tmp/a",
        "file://localhost.evil/tmp/a",
        "file://",
        "file://relative",
    ] {
        assert!(uri_to_path(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn resource_operations_are_closed_and_preserve_preconditions() {
    let operation: ResourceOperation = serde_json::from_value(serde_json::json!({
        "kind": "rename", "oldUri": "file:///a/Before.java", "newUri": "file:///a/After.java",
        "options": {"overwrite": false, "ignoreIfExists": true}
    }))
    .unwrap();
    assert!(
        matches!(operation, ResourceOperation::Rename { old_uri, new_uri, options: CreateOptions { overwrite: false, ignore_if_exists: true }, annotation_id: None }
        if old_uri == "file:///a/Before.java" && new_uri == "file:///a/After.java")
    );
    for invalid in [
        serde_json::json!({"kind": "rename", "oldUri": "file:///a/Before.java"}),
        serde_json::json!({"kind": "execute", "uri": "file:///a/Before.java"}),
        serde_json::json!({"kind": "delete", "uri": "file:///a/Before.java", "options": {"recursive": "false"}}),
        serde_json::json!({"kind": "create", "uri": "file:///a/After.java", "options": {"unmeasured": true}}),
    ] {
        assert!(serde_json::from_value::<ResourceOperation>(invalid).is_err());
    }
}

#[test]
fn resource_operations_are_unsupported() {
    let edit: WorkspaceEdit = serde_json::from_value(serde_json::json!({
        "documentChanges": [
            {"textDocument": {"uri": "file:///a/x.rs", "version": 1}, "edits": []},
            {"kind": "rename", "oldUri": "file:///a/x.rs", "newUri": "file:///a/y.rs"}
        ]
    }))
    .unwrap();
    assert!(edit.into_file_edits().unwrap_err().contains("`rename`"));
    let edit: WorkspaceEdit = serde_json::from_value(serde_json::json!({
        "changes": {"file:///a/x.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": "y"}]}
    }))
    .unwrap();
    assert_eq!(edit.into_file_edits().unwrap().len(), 1);
}
