use super::*;

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
