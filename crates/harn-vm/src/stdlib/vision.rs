use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use async_trait::async_trait;
use base64::Engine;
use serde::Serialize;
use sha2::Digest;
use tokio::process::Command;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const VISION_OCR_AUDIT_TOPIC: &str = "audit.vision_ocr";

thread_local! {
    static OCR_BACKEND_OVERRIDE: RefCell<Option<Rc<dyn OcrBackend>>> = RefCell::new(None);
}

#[derive(Clone, Debug)]
struct NormalizedImageInput {
    kind: String,
    path: Option<String>,
    name: Option<String>,
    mime_type: String,
    sha256: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct OcrOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredText {
    #[serde(rename = "_type")]
    type_name: &'static str,
    text: String,
    blocks: Vec<StructuredBlock>,
    lines: Vec<StructuredLine>,
    tokens: Vec<StructuredToken>,
    source: StructuredSource,
    backend: StructuredBackend,
    stats: StructuredStats,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredBlock {
    page: i64,
    block: i64,
    text: String,
    token_start: usize,
    token_count: usize,
    line_start: usize,
    line_count: usize,
    char_start: usize,
    char_end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    bbox: StructuredBox,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredLine {
    page: i64,
    block: i64,
    paragraph: i64,
    line: i64,
    text: String,
    token_start: usize,
    token_count: usize,
    char_start: usize,
    char_end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    bbox: StructuredBox,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredToken {
    page: i64,
    block: i64,
    paragraph: i64,
    line: i64,
    word: i64,
    text: String,
    normalized: String,
    char_start: usize,
    char_end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    bbox: StructuredBox,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredSource {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    mime_type: String,
    byte_len: usize,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredBackend {
    name: String,
}

#[derive(Clone, Debug, Serialize)]
struct StructuredStats {
    token_count: usize,
    line_count: usize,
    block_count: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct StructuredBox {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OcrWord {
    page: i64,
    block: i64,
    paragraph: i64,
    line: i64,
    word: i64,
    left: i64,
    top: i64,
    width: i64,
    height: i64,
    confidence_milli: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OcrWordText {
    word: OcrWord,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockKey {
    page: i64,
    block: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LineKey {
    page: i64,
    block: i64,
    paragraph: i64,
    line: i64,
}

#[derive(Clone, Copy, Debug)]
struct RunningBox {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

#[derive(Clone, Debug, Default)]
struct ConfidenceAccumulator {
    sum_milli: i64,
    count: usize,
}

#[derive(Clone, Debug)]
struct OcrLineState {
    key: LineKey,
    token_start: usize,
    char_start: usize,
    bbox: RunningBox,
    confidence: ConfidenceAccumulator,
}

#[derive(Clone, Debug)]
struct OcrBlockState {
    key: BlockKey,
    token_start: usize,
    line_start: usize,
    char_start: usize,
    bbox: RunningBox,
    confidence: ConfidenceAccumulator,
}

#[async_trait(?Send)]
trait OcrBackend {
    fn name(&self) -> &'static str;

    async fn recognize(
        &self,
        input: &NormalizedImageInput,
        options: &OcrOptions,
    ) -> Result<Vec<OcrWordText>, String>;
}

struct TesseractCliBackend;

struct TempInputFile {
    path: PathBuf,
}

impl Drop for TempInputFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn reset_vision_state() {
    OCR_BACKEND_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[cfg(test)]
fn install_test_backend(backend: Rc<dyn OcrBackend>) {
    OCR_BACKEND_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = Some(backend);
    });
}

pub(crate) fn register_vision_builtins(vm: &mut Vm) {
    vm.register_async_builtin("vision_ocr", |args| async move {
        let input = normalize_image_input(args.first())?;
        let options = parse_ocr_options(args.get(1))?;
        let backend = current_backend();
        let words = backend
            .recognize(&input, &options)
            .await
            .map_err(|error| VmError::Runtime(format!("vision_ocr: {error}")))?;
        let structured = build_structured_text(&words, &input, backend.name());
        audit_vision_ocr_active("stdlib.vision_ocr", &input, &options, &structured, None).await;
        let value = serde_json::to_value(&structured)
            .map_err(|error| VmError::Runtime(format!("vision_ocr: {error}")))?;
        Ok(crate::schema::json_to_vm_value(&value))
    });
}

fn current_backend() -> Rc<dyn OcrBackend> {
    OCR_BACKEND_OVERRIDE.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(|| Rc::new(TesseractCliBackend))
    })
}

fn normalize_image_input(value: Option<&VmValue>) -> Result<NormalizedImageInput, VmError> {
    match value {
        Some(VmValue::String(path)) => normalized_input_from_path(path.as_ref(), None, None),
        Some(VmValue::Dict(map)) => {
            let path = map
                .get("path")
                .and_then(|value| match value {
                    VmValue::String(text) => Some(text.to_string()),
                    _ => None,
                })
                .or_else(|| {
                    map.get("storage")
                        .and_then(VmValue::as_dict)
                        .and_then(|storage| storage.get("path"))
                        .and_then(|value| match value {
                            VmValue::String(text) => Some(text.to_string()),
                            _ => None,
                        })
                });
            let name = map.get("name").and_then(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            });
            let mime_type = map.get("mime_type").and_then(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            });
            if let Some(path) = path {
                return normalized_input_from_path(&path, name, mime_type);
            }

            if let Some(bytes) = map.get("bytes_base64").and_then(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            }) {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(bytes.as_bytes())
                    .map_err(|error| {
                        VmError::Runtime(format!(
                            "vision_ocr: invalid bytes_base64 payload: {error}"
                        ))
                    })?;
                return normalized_input_from_bytes(
                    "bytes",
                    None,
                    name,
                    mime_type.unwrap_or_else(|| "application/octet-stream".to_string()),
                    decoded,
                );
            }

            if let Some(data_url) = map.get("data_url").and_then(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            }) {
                let (detected_mime, bytes) = decode_data_url(&data_url)?;
                return normalized_input_from_bytes(
                    "data_url",
                    None,
                    name,
                    mime_type.unwrap_or(detected_mime),
                    bytes,
                );
            }

            Err(VmError::Runtime(
                "vision_ocr: expected a path string or image dict with path, storage.path, bytes_base64, or data_url".to_string(),
            ))
        }
        Some(VmValue::Nil) | None => Err(VmError::Runtime(
            "vision_ocr: image input is required".to_string(),
        )),
        Some(other) => Err(VmError::Runtime(format!(
            "vision_ocr: expected image path or dict, got {}",
            other.type_name()
        ))),
    }
}

fn normalized_input_from_path(
    path: &str,
    name: Option<String>,
    mime_type: Option<String>,
) -> Result<NormalizedImageInput, VmError> {
    let resolved = crate::stdlib::process::resolve_source_relative_path(path);
    let bytes = std::fs::read(&resolved).map_err(|error| {
        VmError::Runtime(format!(
            "vision_ocr: failed to read image {}: {error}",
            resolved.display()
        ))
    })?;
    let resolved_text = resolved.display().to_string();
    normalized_input_from_bytes(
        "path",
        Some(resolved_text.clone()),
        name.or_else(|| {
            Path::new(&resolved_text)
                .file_name()
                .map(|part| part.to_string_lossy().into_owned())
        }),
        mime_type.unwrap_or_else(|| guess_mime_type_from_path(&resolved)),
        bytes,
    )
}

fn normalized_input_from_bytes(
    kind: &str,
    path: Option<String>,
    name: Option<String>,
    mime_type: String,
    bytes: Vec<u8>,
) -> Result<NormalizedImageInput, VmError> {
    if bytes.is_empty() {
        return Err(VmError::Runtime(
            "vision_ocr: image payload is empty".to_string(),
        ));
    }
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    Ok(NormalizedImageInput {
        kind: kind.to_string(),
        path,
        name,
        mime_type,
        sha256,
        bytes,
    })
}

fn parse_ocr_options(value: Option<&VmValue>) -> Result<OcrOptions, VmError> {
    let Some(value) = value else {
        return Ok(OcrOptions::default());
    };
    match value {
        VmValue::Nil => Ok(OcrOptions::default()),
        VmValue::Dict(map) => {
            let language = match map.get("language") {
                Some(VmValue::String(text)) if !text.trim().is_empty() => Some(text.to_string()),
                Some(VmValue::Nil) | None => None,
                Some(_) => {
                    return Err(VmError::Runtime(
                        "vision_ocr: options.language must be a string".to_string(),
                    ))
                }
            };
            Ok(OcrOptions { language })
        }
        _ => Err(VmError::Runtime(
            "vision_ocr: options must be a dict".to_string(),
        )),
    }
}

fn decode_data_url(data_url: &str) -> Result<(String, Vec<u8>), VmError> {
    let Some(rest) = data_url.strip_prefix("data:") else {
        return Err(VmError::Runtime(
            "vision_ocr: data_url must start with data:".to_string(),
        ));
    };
    let Some((meta, payload)) = rest.split_once(',') else {
        return Err(VmError::Runtime(
            "vision_ocr: malformed data_url payload".to_string(),
        ));
    };
    if !meta.ends_with(";base64") {
        return Err(VmError::Runtime(
            "vision_ocr: only base64 data URLs are supported".to_string(),
        ));
    }
    let mime_type = meta.trim_end_matches(";base64");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.as_bytes())
        .map_err(|error| {
            VmError::Runtime(format!("vision_ocr: invalid data_url payload: {error}"))
        })?;
    Ok((mime_type.to_string(), bytes))
}

fn guess_mime_type_from_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("tif") | Some("tiff") => "image/tiff",
        Some("bmp") => "image/bmp",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pbm") => "image/x-portable-bitmap",
        Some("pgm") => "image/x-portable-graymap",
        Some("ppm") => "image/x-portable-pixmap",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/x-portable-bitmap" => "pbm",
        "image/x-portable-graymap" => "pgm",
        "image/x-portable-pixmap" => "ppm",
        _ => "bin",
    }
}

fn normalized_source(input: &NormalizedImageInput) -> StructuredSource {
    StructuredSource {
        kind: input.kind.clone(),
        path: input.path.clone(),
        name: input.name.clone(),
        mime_type: input.mime_type.clone(),
        byte_len: input.bytes.len(),
        sha256: input.sha256.clone(),
    }
}

fn build_structured_text(
    words: &[OcrWordText],
    input: &NormalizedImageInput,
    backend_name: &str,
) -> StructuredText {
    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut lines = Vec::new();
    let mut blocks = Vec::new();
    let mut current_block: Option<OcrBlockState> = None;
    let mut current_line: Option<OcrLineState> = None;

    for word in words {
        let block_key = BlockKey {
            page: word.word.page,
            block: word.word.block,
        };
        let line_key = LineKey {
            page: word.word.page,
            block: word.word.block,
            paragraph: word.word.paragraph,
            line: word.word.line,
        };

        if current_block.as_ref().map(|state| state.key) != Some(block_key) {
            finish_line(&mut current_line, &text, &mut lines, tokens.len());
            finish_block(
                &mut current_block,
                &text,
                &mut blocks,
                tokens.len(),
                lines.len(),
            );
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            current_block = Some(OcrBlockState {
                key: block_key,
                token_start: tokens.len(),
                line_start: lines.len(),
                char_start: text.len(),
                bbox: RunningBox::from_word(&word.word),
                confidence: ConfidenceAccumulator::from_word(word.word.confidence_milli),
            });
        } else if let Some(block) = current_block.as_mut() {
            block.bbox.include_word(&word.word);
            block.confidence.record(word.word.confidence_milli);
        }

        if current_line.as_ref().map(|state| state.key) != Some(line_key) {
            finish_line(&mut current_line, &text, &mut lines, tokens.len());
            if current_block.is_some() && !text.is_empty() && !text.ends_with("\n\n") {
                text.push('\n');
            }
            current_line = Some(OcrLineState {
                key: line_key,
                token_start: tokens.len(),
                char_start: text.len(),
                bbox: RunningBox::from_word(&word.word),
                confidence: ConfidenceAccumulator::from_word(word.word.confidence_milli),
            });
        } else if let Some(line) = current_line.as_mut() {
            line.bbox.include_word(&word.word);
            line.confidence.record(word.word.confidence_milli);
        }

        if current_line
            .as_ref()
            .is_some_and(|state| tokens.len() > state.token_start)
        {
            text.push(' ');
        }
        let char_start = text.len();
        text.push_str(&word.text);
        let char_end = text.len();
        tokens.push(StructuredToken {
            page: word.word.page,
            block: word.word.block,
            paragraph: word.word.paragraph,
            line: word.word.line,
            word: word.word.word,
            text: word.text.clone(),
            normalized: word.text.to_ascii_lowercase(),
            char_start,
            char_end,
            confidence: word
                .word
                .confidence_milli
                .map(|value| value as f64 / 1000.0),
            bbox: StructuredBox::from_word(&word.word),
        });
    }

    finish_line(&mut current_line, &text, &mut lines, tokens.len());
    finish_block(
        &mut current_block,
        &text,
        &mut blocks,
        tokens.len(),
        lines.len(),
    );

    let line_count = lines.len();
    let block_count = blocks.len();
    StructuredText {
        type_name: "structured_text",
        text,
        blocks,
        lines,
        tokens,
        source: normalized_source(input),
        backend: StructuredBackend {
            name: backend_name.to_string(),
        },
        stats: StructuredStats {
            token_count: words.len(),
            line_count,
            block_count,
        },
    }
}

fn finish_line(
    state: &mut Option<OcrLineState>,
    text: &str,
    lines: &mut Vec<StructuredLine>,
    token_end: usize,
) {
    let Some(state) = state.take() else {
        return;
    };
    lines.push(StructuredLine {
        page: state.key.page,
        block: state.key.block,
        paragraph: state.key.paragraph,
        line: state.key.line,
        text: text[state.char_start..].trim_end_matches('\n').to_string(),
        token_start: state.token_start,
        token_count: token_end.saturating_sub(state.token_start),
        char_start: state.char_start,
        char_end: text.len(),
        confidence: state.confidence.average(),
        bbox: state.bbox.finish(),
    });
}

fn finish_block(
    state: &mut Option<OcrBlockState>,
    text: &str,
    blocks: &mut Vec<StructuredBlock>,
    token_end: usize,
    line_end: usize,
) {
    let Some(state) = state.take() else {
        return;
    };
    blocks.push(StructuredBlock {
        page: state.key.page,
        block: state.key.block,
        text: text[state.char_start..].trim_end_matches('\n').to_string(),
        token_start: state.token_start,
        token_count: token_end.saturating_sub(state.token_start),
        line_start: state.line_start,
        line_count: line_end.saturating_sub(state.line_start),
        char_start: state.char_start,
        char_end: text.len(),
        confidence: state.confidence.average(),
        bbox: state.bbox.finish(),
    });
}

async fn audit_vision_ocr_active(
    caller: &str,
    input: &NormalizedImageInput,
    options: &OcrOptions,
    structured: &StructuredText,
    error: Option<&str>,
) {
    let Some(event_log) = active_event_log() else {
        return;
    };
    let payload = serde_json::json!({
        "caller": caller,
        "input": {
            "kind": input.kind,
            "path": input.path,
            "name": input.name,
            "mime_type": input.mime_type,
            "byte_len": input.bytes.len(),
            "sha256": input.sha256,
            "bytes_base64": base64::engine::general_purpose::STANDARD.encode(&input.bytes),
        },
        "options": options,
        "output": structured,
        "error": error,
        "observed_at": crate::orchestration::now_rfc3339(),
    });
    let topic = Topic::new(VISION_OCR_AUDIT_TOPIC).expect("vision OCR topic is valid");
    let kind = if error.is_some() {
        "ocr_failed"
    } else {
        "ocr_completed"
    };
    if let Err(log_error) = event_log.append(&topic, LogEvent::new(kind, payload)).await {
        crate::events::log_warn(
            "vision_ocr.audit",
            &format!("failed to append vision OCR audit event: {log_error}"),
        );
    }
}

#[async_trait(?Send)]
impl OcrBackend for TesseractCliBackend {
    fn name(&self) -> &'static str {
        "tesseract_cli"
    }

    async fn recognize(
        &self,
        input: &NormalizedImageInput,
        options: &OcrOptions,
    ) -> Result<Vec<OcrWordText>, String> {
        let mut temp_file = None;
        let input_path = if let Some(path) = input.path.as_ref() {
            PathBuf::from(path)
        } else {
            let temp = write_temp_input(input).map_err(|error| error.to_string())?;
            let path = temp.path.clone();
            temp_file = Some(temp);
            path
        };
        let mut command = Command::new("tesseract");
        command.arg(&input_path).arg("stdout");
        if let Some(language) = options.language.as_deref() {
            command.arg("-l").arg(language);
        }
        command.arg("tsv");

        let output = command.output().await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "tesseract executable not found on PATH".to_string()
            } else {
                format!("failed to launch tesseract: {error}")
            }
        })?;
        drop(temp_file);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("tesseract exited with status {}", output.status)
            } else {
                format!("tesseract failed: {stderr}")
            });
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("tesseract returned non-UTF8 TSV output: {error}"))?;
        parse_tesseract_tsv(&stdout)
    }
}

fn write_temp_input(input: &NormalizedImageInput) -> std::io::Result<TempInputFile> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "harn-vision-ocr-{}.{}",
        uuid::Uuid::now_v7(),
        extension_for_mime(&input.mime_type)
    ));
    std::fs::write(&path, &input.bytes)?;
    Ok(TempInputFile { path })
}

fn parse_tesseract_tsv(tsv: &str) -> Result<Vec<OcrWordText>, String> {
    let mut words = Vec::new();
    for (index, raw_line) in tsv.lines().enumerate() {
        if index == 0 && raw_line.starts_with("level\tpage_num\tblock_num") {
            continue;
        }
        if raw_line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = raw_line.split('\t').collect();
        if parts.len() < 12 {
            return Err(format!("unexpected Tesseract TSV row: {raw_line}"));
        }
        let level = parse_i64(parts[0], "level")?;
        if level != 5 {
            continue;
        }
        let text = parts[11..].join("\t").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let confidence_milli = parse_confidence_milli(parts[10])?;
        words.push(OcrWordText {
            word: OcrWord {
                page: parse_i64(parts[1], "page_num")?,
                block: parse_i64(parts[2], "block_num")?,
                paragraph: parse_i64(parts[3], "par_num")?,
                line: parse_i64(parts[4], "line_num")?,
                word: parse_i64(parts[5], "word_num")?,
                left: parse_i64(parts[6], "left")?,
                top: parse_i64(parts[7], "top")?,
                width: parse_i64(parts[8], "width")?,
                height: parse_i64(parts[9], "height")?,
                confidence_milli,
            },
            text,
        });
    }
    Ok(words)
}

fn parse_i64(value: &str, field: &str) -> Result<i64, String> {
    value
        .trim()
        .parse::<i64>()
        .map_err(|error| format!("invalid {field} value {value:?}: {error}"))
}

fn parse_confidence_milli(value: &str) -> Result<Option<i64>, String> {
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|error| format!("invalid conf value {value:?}: {error}"))?;
    if parsed < 0.0 {
        Ok(None)
    } else {
        Ok(Some((parsed * 1000.0).round() as i64))
    }
}

impl RunningBox {
    fn from_word(word: &OcrWord) -> Self {
        Self {
            left: word.left,
            top: word.top,
            right: word.left + word.width,
            bottom: word.top + word.height,
        }
    }

    fn include_word(&mut self, word: &OcrWord) {
        self.left = self.left.min(word.left);
        self.top = self.top.min(word.top);
        self.right = self.right.max(word.left + word.width);
        self.bottom = self.bottom.max(word.top + word.height);
    }

    fn finish(self) -> StructuredBox {
        StructuredBox {
            left: self.left,
            top: self.top,
            width: self.right - self.left,
            height: self.bottom - self.top,
        }
    }
}

impl StructuredBox {
    fn from_word(word: &OcrWord) -> Self {
        Self {
            left: word.left,
            top: word.top,
            width: word.width,
            height: word.height,
        }
    }
}

impl ConfidenceAccumulator {
    fn from_word(value: Option<i64>) -> Self {
        let mut acc = Self::default();
        acc.record(value);
        acc
    }

    fn record(&mut self, value: Option<i64>) {
        if let Some(value) = value {
            self.sum_milli += value;
            self.count += 1;
        }
    }

    fn average(&self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum_milli as f64 / self.count as f64 / 1000.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MockBackend {
        words: Vec<OcrWordText>,
    }

    #[async_trait(?Send)]
    impl OcrBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock_ocr"
        }

        async fn recognize(
            &self,
            _input: &NormalizedImageInput,
            _options: &OcrOptions,
        ) -> Result<Vec<OcrWordText>, String> {
            Ok(self.words.clone())
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn word(
        page: i64,
        block: i64,
        paragraph: i64,
        line: i64,
        ordinal: i64,
        left: i64,
        top: i64,
        width: i64,
        height: i64,
        confidence: f64,
        text: &str,
    ) -> OcrWordText {
        OcrWordText {
            word: OcrWord {
                page,
                block,
                paragraph,
                line,
                word: ordinal,
                left,
                top,
                width,
                height,
                confidence_milli: Some((confidence * 1000.0).round() as i64),
            },
            text: text.to_string(),
        }
    }

    #[test]
    fn normalize_image_input_accepts_data_url() {
        let value = crate::schema::json_to_vm_value(&serde_json::json!({
            "data_url": "data:image/png;base64,ZmFrZQ==",
            "name": "sample.png",
        }));
        let normalized = normalize_image_input(Some(&value)).expect("normalize image input");
        assert_eq!(normalized.kind, "data_url");
        assert_eq!(normalized.name.as_deref(), Some("sample.png"));
        assert_eq!(normalized.mime_type, "image/png");
        assert_eq!(normalized.bytes, b"fake");
    }

    #[test]
    fn build_structured_text_groups_blocks_lines_and_offsets() {
        let structured = build_structured_text(
            &[
                word(1, 1, 1, 1, 1, 0, 0, 10, 10, 97.0, "Hello"),
                word(1, 1, 1, 1, 2, 12, 0, 10, 10, 96.0, "world"),
                word(1, 1, 1, 2, 1, 0, 16, 8, 10, 95.0, "OCR"),
                word(1, 2, 1, 1, 1, 0, 32, 7, 10, 94.0, "Done"),
            ],
            &NormalizedImageInput {
                kind: "bytes".to_string(),
                path: None,
                name: Some("sample.png".to_string()),
                mime_type: "image/png".to_string(),
                sha256: "abc".to_string(),
                bytes: b"fake".to_vec(),
            },
            "mock_ocr",
        );

        assert_eq!(structured.text, "Hello world\nOCR\n\nDone");
        assert_eq!(structured.tokens.len(), 4);
        assert_eq!(structured.lines.len(), 3);
        assert_eq!(structured.blocks.len(), 2);
        assert_eq!(structured.tokens[0].char_start, 0);
        assert_eq!(structured.tokens[1].char_start, 6);
        assert_eq!(structured.tokens[2].char_start, 12);
        assert_eq!(structured.blocks[1].char_start, 17);
    }

    #[test]
    fn parse_tesseract_tsv_keeps_word_rows_only() {
        let parsed = parse_tesseract_tsv(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t200\t100\t-1\t\n\
5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t96.25\tHello\n\
5\t1\t1\t1\t1\t2\t12\t0\t10\t10\t95.50\tworld\n",
        )
        .expect("parse TSV");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "Hello");
        assert_eq!(parsed[0].word.confidence_milli, Some(96250));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vision_ocr_builtin_uses_override_backend_and_logs_event() {
        reset_vision_state();
        crate::event_log::reset_active_event_log();
        let log = crate::event_log::install_memory_for_current_thread(32);
        install_test_backend(Rc::new(MockBackend {
            words: vec![
                word(1, 1, 1, 1, 1, 0, 0, 12, 10, 98.0, "Alpha"),
                word(1, 1, 1, 1, 2, 14, 0, 10, 10, 97.0, "Beta"),
            ],
        }));

        let mut vm = Vm::new();
        register_vision_builtins(&mut vm);
        let result = vm
            .call_named_builtin(
                "vision_ocr",
                vec![crate::schema::json_to_vm_value(&serde_json::json!({
                    "bytes_base64": "ZmFrZQ==",
                    "mime_type": "image/png",
                    "name": "sample.png",
                }))],
            )
            .await
            .expect("vision_ocr succeeds");
        let json = crate::llm::vm_value_to_json(&result);
        assert_eq!(json["text"], "Alpha Beta");
        assert_eq!(json["backend"]["name"], "mock_ocr");
        assert_eq!(json["source"]["name"], "sample.png");
        assert_eq!(json["stats"]["token_count"], 2);

        let topic = Topic::new(VISION_OCR_AUDIT_TOPIC).unwrap();
        let events = log
            .read_range(&topic, None, 10)
            .await
            .expect("read audit events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.kind, "ocr_completed");
        assert_eq!(events[0].1.payload["output"]["text"], "Alpha Beta");
        assert_eq!(events[0].1.payload["input"]["bytes_base64"], "ZmFrZQ==");

        reset_vision_state();
        crate::event_log::reset_active_event_log();
    }
}
