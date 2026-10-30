use super::*;
use crate::stdlib::template::{
    render_asset_result, PromptSourceSpan, PromptSpanKind, TemplateAsset,
};

#[test]
fn recorded_prompt_spans_point_to_the_kept_source_text() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let source = "{{ if true -}}\nKeep this instruction exactly.\n{{- end }}\n{{ raw }}Raw {{directive}} text.{{ endraw }}";
    let path = tmp.path().join("instructions.harn.prompt");
    std::fs::write(&path, source).unwrap();
    let guard = start_recording(tmp.path(), RecordingLevel::Full, None)
        .unwrap()
        .unwrap();
    let asset = TemplateAsset::inline(source, Some(tmp.path()), Some(&path));
    let rendered = render_asset_result(&asset, None).unwrap();
    assert_eq!(
        rendered,
        "Keep this instruction exactly.\nRaw {{directive}} text."
    );
    let revised = format!("{{{{ if false }}}}ignored\n{{{{ end }}}}{source}");
    let revised_asset = TemplateAsset::inline(&revised, Some(tmp.path()), Some(&path));
    assert_eq!(render_asset_result(&revised_asset, None).unwrap(), rendered);
    assert_eq!(guard.error(), None);
    drop(guard);

    let tape = EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap();
    let renders: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::PromptRender {
                render_id, spans, ..
            } => Some((render_id, spans)),
            _ => None,
        })
        .collect();
    assert_eq!(
        renders.len(),
        2,
        "equal output must retain both source maps"
    );
    assert_ne!(renders[0].0, renders[1].0);
    let revised_spans: Vec<PromptSourceSpan> =
        serde_json::from_slice(&tape.resolve_payload(renders[1].1).unwrap()).unwrap();
    let revised_literal = revised_spans
        .iter()
        .find(|span| {
            span.kind == PromptSpanKind::Text
                && rendered.get(span.output_start..span.output_end)
                    == Some("Keep this instruction exactly.")
        })
        .unwrap();
    assert_eq!(revised_literal.template_line, 3);
    assert_eq!(
        revised_literal.template_revision,
        blake3::hash(revised.as_bytes()).to_hex().to_string()
    );
    let payload = tape
        .records
        .iter()
        .find_map(|record| match &record.kind {
            TapeRecordKind::PromptRender { spans, .. } => Some(spans),
            _ => None,
        })
        .expect("the canonical renderer must record provenance");
    let spans: Vec<PromptSourceSpan> =
        serde_json::from_slice(&tape.resolve_payload(payload).unwrap()).unwrap();
    let revision = blake3::hash(source.as_bytes()).to_hex().to_string();
    for (text, position) in [
        ("Keep this instruction exactly.", (2, 1)),
        ("Raw {{directive}} text.", (4, 10)),
    ] {
        let span = spans
            .iter()
            .find(|span| {
                span.kind == PromptSpanKind::Text
                    && rendered.get(span.output_start..span.output_end) == Some(text)
            })
            .expect("literal source span must survive recording");
        assert_eq!((span.template_line, span.template_col), position);
        assert_eq!(span.template_uri, path.to_string_lossy());
        assert_eq!(span.template_revision, revision);
    }
}
