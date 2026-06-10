//! Provider-neutral multimodal content helpers.
//!
//! Harn scripts represent multimodal inputs as content blocks:
//! `{type: "image", url?: string, base64?: string, media_type: string, detail?: "low"|"high"|"auto"}`.
//! `{type: "video", url?: string, base64?: string, media_type: string}`.
//! `{type: "pdf", url?: string, base64?: string, file_id?: string}`.
//! `{type: "audio", url?: string, base64?: string, file_id?: string, media_type: string}`.
//! Provider serializers translate that one shape into their native wire format.

use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageContent {
    pub url: Option<String>,
    pub base64: Option<String>,
    pub media_type: String,
    pub detail: Option<String>,
}

impl ImageContent {
    fn from_block(block: &serde_json::Value) -> Result<Option<Self>, VmError> {
        if block.get("type").and_then(|value| value.as_str()) != Some("image") {
            return Ok(None);
        }
        let url = block
            .get("url")
            .or_else(|| block.get("file_uri"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let base64 = block
            .get("base64")
            .or_else(|| block.get("data"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if url.is_some() == base64.is_some() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "llm_call image content requires exactly one of url or base64",
            ))));
        }
        let media_type = block
            .get("media_type")
            .or_else(|| block.get("mime_type"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "llm_call image content requires media_type",
                )))
            })?
            .to_string();
        let detail = block
            .get("detail")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(detail) = detail.as_deref() {
            if !matches!(detail, "low" | "high" | "auto") {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "llm_call image detail must be \"low\", \"high\", or \"auto\"",
                ))));
            }
        }
        Ok(Some(Self {
            url,
            base64,
            media_type,
            detail,
        }))
    }

    pub(crate) fn openai_url(&self) -> String {
        self.url.clone().unwrap_or_else(|| {
            format!(
                "data:{};base64,{}",
                self.media_type,
                self.base64.as_deref().unwrap_or_default()
            )
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VideoContent {
    pub url: Option<String>,
    pub base64: Option<String>,
    pub media_type: String,
}

impl VideoContent {
    fn from_block(block: &serde_json::Value) -> Result<Option<Self>, VmError> {
        let block_type = block.get("type").and_then(|value| value.as_str());
        if !matches!(block_type, Some("video" | "video_url")) {
            return Ok(None);
        }
        let nested_video_url = block.get("video_url");
        let url = nested_video_url
            .and_then(|value| value.get("url"))
            .or_else(|| block.get("url"))
            .or_else(|| block.get("file_uri"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let base64 = block
            .get("base64")
            .or_else(|| block.get("data"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if url.is_some() == base64.is_some() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "llm_call video content requires exactly one of url or base64",
            ))));
        }
        let media_type = block
            .get("media_type")
            .or_else(|| block.get("mime_type"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "video/mp4".to_string());
        Ok(Some(Self {
            url,
            base64,
            media_type,
        }))
    }

    pub(crate) fn openai_url(&self) -> String {
        self.url.clone().unwrap_or_else(|| {
            format!(
                "data:{};base64,{}",
                self.media_type,
                self.base64.as_deref().unwrap_or_default()
            )
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileContentKind {
    Pdf,
    Audio,
}

impl FileContentKind {
    fn from_type(value: &str) -> Option<Self> {
        match value {
            "pdf" | "document" => Some(Self::Pdf),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }

    fn harn_type(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Audio => "audio",
        }
    }

    fn anthropic_block_type(self) -> &'static str {
        match self {
            Self::Pdf => "document",
            Self::Audio => "audio",
        }
    }

    fn default_media_type(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Audio => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileContent {
    pub kind: FileContentKind,
    pub url: Option<String>,
    pub base64: Option<String>,
    pub file_id: Option<String>,
    pub media_type: String,
}

impl FileContent {
    fn from_block(block: &serde_json::Value) -> Result<Option<Self>, VmError> {
        let Some(kind) = block
            .get("type")
            .and_then(|value| value.as_str())
            .and_then(FileContentKind::from_type)
        else {
            return Ok(None);
        };
        let url = block
            .get("url")
            .or_else(|| block.get("file_uri"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let base64 = block
            .get("base64")
            .or_else(|| block.get("data"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let file_id = block
            .get("file_id")
            .or_else(|| block.get("id"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let source_count = url.is_some() as u8 + base64.is_some() as u8 + file_id.is_some() as u8;
        if source_count != 1 {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "llm_call {} content requires exactly one of url, base64, or file_id",
                    kind.harn_type()
                ),
            ))));
        }
        let media_type = block
            .get("media_type")
            .or_else(|| block.get("mime_type"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| kind.default_media_type().to_string());
        if media_type.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!("llm_call {} content requires media_type", kind.harn_type()),
            ))));
        }
        Ok(Some(Self {
            kind,
            url,
            base64,
            file_id,
            media_type,
        }))
    }
}

pub(crate) fn parse_image_block(
    block: &serde_json::Value,
) -> Result<Option<ImageContent>, VmError> {
    ImageContent::from_block(block)
}

pub(crate) fn parse_file_block(block: &serde_json::Value) -> Result<Option<FileContent>, VmError> {
    FileContent::from_block(block)
}

pub(crate) fn parse_video_block(
    block: &serde_json::Value,
) -> Result<Option<VideoContent>, VmError> {
    VideoContent::from_block(block)
}

pub(crate) fn messages_contain_images(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    for message in messages {
        if message
            .get("images")
            .and_then(|value| value.as_array())
            .is_some_and(|images| !images.is_empty())
        {
            return Ok(true);
        }
        match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if parse_image_block(block)?.is_some() {
                        return Ok(true);
                    }
                }
            }
            Some(content @ serde_json::Value::Object(_)) => {
                let contains_image = parse_image_block(content)?.is_some();
                if contains_image {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(crate) fn messages_contain_videos(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    for message in messages {
        if message
            .get("videos")
            .and_then(|value| value.as_array())
            .is_some_and(|videos| !videos.is_empty())
        {
            return Ok(true);
        }
        match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if parse_video_block(block)?.is_some() {
                        return Ok(true);
                    }
                }
            }
            Some(content @ serde_json::Value::Object(_)) => {
                let contains_video = parse_video_block(content)?.is_some();
                if contains_video {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(crate) fn messages_contain_audio(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    messages_contain_file_kind(messages, FileContentKind::Audio)
}

pub(crate) fn messages_contain_pdf(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    messages_contain_file_kind(messages, FileContentKind::Pdf)
}

pub(crate) fn messages_contain_file_ids(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    for message in messages {
        match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if parse_file_block(block)?.is_some_and(|file| file.file_id.is_some()) {
                        return Ok(true);
                    }
                }
            }
            Some(content @ serde_json::Value::Object(_))
                if parse_file_block(content)?.is_some_and(|file| file.file_id.is_some()) =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

fn messages_contain_file_kind(
    messages: &[serde_json::Value],
    kind: FileContentKind,
) -> Result<bool, VmError> {
    for message in messages {
        match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if parse_file_block(block)?.is_some_and(|file| file.kind == kind) {
                        return Ok(true);
                    }
                }
            }
            Some(content @ serde_json::Value::Object(_))
                if parse_file_block(content)?.is_some_and(|file| file.kind == kind) =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(crate) fn messages_contain_url_images(messages: &[serde_json::Value]) -> Result<bool, VmError> {
    for message in messages {
        match message.get("content") {
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if parse_image_block(block)?.is_some_and(|image| image.url.is_some()) {
                        return Ok(true);
                    }
                }
            }
            Some(content @ serde_json::Value::Object(_)) => {
                let contains_url_image =
                    parse_image_block(content)?.is_some_and(|image| image.url.is_some());
                if contains_url_image {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

fn normalized_text_block(block: &serde_json::Value) -> Option<serde_json::Value> {
    let block_type = block.get("type").and_then(|value| value.as_str());
    match block_type {
        Some("text") | Some("output_text") => {
            let mut normalized = serde_json::json!({
                "type": "text",
                "text": block.get("text").and_then(|value| value.as_str()).unwrap_or_default(),
            });
            copy_cache_control(block, &mut normalized);
            Some(normalized)
        }
        _ => None,
    }
}

fn copy_cache_control(original: &serde_json::Value, normalized: &mut serde_json::Value) {
    if let Some(cache_control) = original.get("cache_control") {
        normalized["cache_control"] = cache_control.clone();
    }
}

pub(crate) fn anthropic_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                if let Ok(Some(image)) = parse_image_block(block) {
                    let source = match (image.base64, image.url) {
                        (Some(data), None) => serde_json::json!({
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": data,
                        }),
                        (None, Some(url)) => serde_json::json!({
                            "type": "url",
                            "url": url,
                        }),
                        _ => continue,
                    };
                    out.push(serde_json::json!({"type": "image", "source": source}));
                } else if parse_video_block(block).is_ok_and(|video| video.is_some()) {
                    out.push(block.clone());
                } else if let Ok(Some(file)) = parse_file_block(block) {
                    out.push(anthropic_file_block(block, file));
                } else if let Some(text) = normalized_text_block(block) {
                    out.push(text);
                } else {
                    out.push(block.clone());
                }
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(_) => {
            if let Ok(Some(image)) = parse_image_block(content) {
                anthropic_content(&serde_json::Value::Array(vec![serde_json::json!(
                    image_to_neutral_json(&image)
                )]))
            } else if parse_video_block(content).is_ok_and(|video| video.is_some()) {
                content.clone()
            } else if let Ok(Some(file)) = parse_file_block(content) {
                serde_json::Value::Array(vec![anthropic_file_block(content, file)])
            } else {
                content.clone()
            }
        }
        _ => content.clone(),
    }
}

pub(crate) fn openai_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                if let Ok(Some(image)) = parse_image_block(block) {
                    let mut image_url = serde_json::json!({"url": image.openai_url()});
                    if let Some(detail) = image.detail {
                        image_url["detail"] = serde_json::json!(detail);
                    }
                    let mut normalized = serde_json::json!({
                        "type": "image_url",
                        "image_url": image_url,
                    });
                    copy_cache_control(block, &mut normalized);
                    out.push(normalized);
                } else if let Ok(Some(video)) = parse_video_block(block) {
                    let mut normalized = serde_json::json!({
                        "type": "video_url",
                        "video_url": {
                            "url": video.openai_url(),
                        },
                    });
                    copy_cache_control(block, &mut normalized);
                    out.push(normalized);
                } else if let Ok(Some(file)) = parse_file_block(block) {
                    out.push(openai_file_block(block, file));
                } else if let Some(text) = normalized_text_block(block) {
                    out.push(text);
                } else {
                    out.push(block.clone());
                }
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(_) => {
            if let Ok(Some(image)) = parse_image_block(content) {
                let mut image_url = serde_json::json!({"url": image.openai_url()});
                if let Some(detail) = image.detail {
                    image_url["detail"] = serde_json::json!(detail);
                }
                let mut normalized = serde_json::json!({
                    "type": "image_url",
                    "image_url": image_url,
                });
                copy_cache_control(content, &mut normalized);
                serde_json::Value::Array(vec![normalized])
            } else if let Ok(Some(video)) = parse_video_block(content) {
                let mut normalized = serde_json::json!({
                    "type": "video_url",
                    "video_url": {
                        "url": video.openai_url(),
                    },
                });
                copy_cache_control(content, &mut normalized);
                serde_json::Value::Array(vec![normalized])
            } else if let Ok(Some(file)) = parse_file_block(content) {
                serde_json::Value::Array(vec![openai_file_block(content, file)])
            } else {
                content.clone()
            }
        }
        _ => content.clone(),
    }
}

pub(crate) fn ollama_message(mut message: serde_json::Value) -> serde_json::Value {
    let Some(object) = message.as_object_mut() else {
        return message;
    };
    let Some(content) = object.get("content").cloned() else {
        return message;
    };
    let serde_json::Value::Array(blocks) = content else {
        return message;
    };
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut passthrough = Vec::new();
    for block in blocks {
        if let Ok(Some(image)) = parse_image_block(&block) {
            if let Some(base64) = image.base64 {
                images.push(serde_json::Value::String(base64));
            }
            continue;
        }
        if let Some(text) = normalized_text_block(&block) {
            if let Some(value) = text.get("text").and_then(|value| value.as_str()) {
                if !value.is_empty() {
                    text_parts.push(value.to_string());
                }
            }
        } else {
            passthrough.push(block);
        }
    }
    if !text_parts.is_empty() {
        object.insert(
            "content".to_string(),
            serde_json::Value::String(text_parts.join("\n\n")),
        );
    }
    if !images.is_empty() {
        object.insert("images".to_string(), serde_json::Value::Array(images));
    }
    if text_parts.is_empty() && !passthrough.is_empty() {
        object.insert("content".to_string(), serde_json::Value::Array(passthrough));
    }
    message
}

pub(crate) fn gemini_parts(content: &serde_json::Value) -> Vec<serde_json::Value> {
    match content {
        serde_json::Value::String(text) => vec![serde_json::json!({"text": text})],
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("functionCall").is_some() || block.get("functionResponse").is_some()
                {
                    return Some(block.clone());
                }
                if block.get("type").and_then(|value| value.as_str()) == Some("tool_call") {
                    let name = block
                        .get("name")
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())?;
                    let mut function_call = serde_json::json!({
                        "name": name,
                        "args": block
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                    });
                    if let Some(id) = block
                        .get("id")
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())
                    {
                        function_call["id"] = serde_json::json!(id);
                    }
                    let mut part = serde_json::json!({ "functionCall": function_call });
                    if let Some(signature) = block
                        .get("thoughtSignature")
                        .or_else(|| block.get("thought_signature"))
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())
                    {
                        part["thoughtSignature"] = serde_json::json!(signature);
                    }
                    return Some(part);
                }
                if let Ok(Some(image)) = parse_image_block(block) {
                    if let Some(data) = image.base64 {
                        return Some(serde_json::json!({
                            "inline_data": {
                                "mime_type": image.media_type,
                                "data": data,
                            }
                        }));
                    }
                    if let Some(file_uri) = image.url {
                        return Some(serde_json::json!({
                            "file_data": {
                                "mime_type": image.media_type,
                                "file_uri": file_uri,
                            }
                        }));
                    }
                }
                if let Ok(Some(video)) = parse_video_block(block) {
                    if let Some(data) = video.base64 {
                        return Some(serde_json::json!({
                            "inline_data": {
                                "mime_type": video.media_type,
                                "data": data,
                            }
                        }));
                    }
                    if let Some(file_uri) = video.url {
                        return Some(serde_json::json!({
                            "file_data": {
                                "mime_type": video.media_type,
                                "file_uri": file_uri,
                            }
                        }));
                    }
                }
                if let Ok(Some(file)) = parse_file_block(block) {
                    if let Some(data) = file.base64 {
                        return Some(serde_json::json!({
                            "inline_data": {
                                "mime_type": file.media_type,
                                "data": data,
                            }
                        }));
                    }
                    if let Some(file_uri) = file.url.or(file.file_id) {
                        return Some(serde_json::json!({
                            "file_data": {
                                "mime_type": file.media_type,
                                "file_uri": file_uri,
                            }
                        }));
                    }
                }
                if let Some(text) = normalized_text_block(block) {
                    let mut part = serde_json::json!({
                        "text": text.get("text").and_then(|value| value.as_str()).unwrap_or_default(),
                    });
                    if let Some(signature) = block
                        .get("thoughtSignature")
                        .or_else(|| block.get("thought_signature"))
                        .or_else(|| {
                            block
                                .get("provider_metadata")
                                .and_then(|value| value.get("gemini"))
                                .and_then(|value| value.get("thought_signature"))
                        })
                        .and_then(|value| value.as_str())
                    {
                        if !signature.is_empty() {
                            part["thoughtSignature"] = serde_json::json!(signature);
                        }
                    }
                    return Some(part);
                }
                block.get("text")
                    .and_then(|value| value.as_str())
                    .map(|text| serde_json::json!({"text": text}))
            })
            .collect(),
        other => vec![serde_json::json!({"text": other.to_string()})],
    }
}

fn anthropic_file_block(original: &serde_json::Value, file: FileContent) -> serde_json::Value {
    let source = match (file.base64, file.url, file.file_id) {
        (Some(data), None, None) => serde_json::json!({
            "type": "base64",
            "media_type": file.media_type,
            "data": data,
        }),
        (None, Some(url), None) => serde_json::json!({
            "type": "url",
            "url": url,
        }),
        (None, None, Some(file_id)) => serde_json::json!({
            "type": "file",
            "file_id": file_id,
        }),
        _ => serde_json::json!({}),
    };
    let mut block = serde_json::json!({
        "type": file.kind.anthropic_block_type(),
        "source": source,
    });
    for key in ["title", "context", "citations", "cache_control"] {
        if let Some(value) = original.get(key) {
            block[key] = value.clone();
        }
    }
    block
}

fn openai_file_block(original: &serde_json::Value, file: FileContent) -> serde_json::Value {
    let mut block = serde_json::json!({
        "type": file.kind.harn_type(),
        "media_type": file.media_type,
    });
    if let Some(url) = file.url {
        block["url"] = serde_json::json!(url);
    }
    if let Some(base64) = file.base64 {
        block["base64"] = serde_json::json!(base64);
    }
    if let Some(file_id) = file.file_id {
        block["file_id"] = serde_json::json!(file_id);
    }
    copy_cache_control(original, &mut block);
    block
}

fn image_to_neutral_json(image: &ImageContent) -> serde_json::Value {
    let mut value = serde_json::json!({
        "type": "image",
        "media_type": image.media_type,
    });
    if let Some(url) = image.url.as_ref() {
        value["url"] = serde_json::json!(url);
    }
    if let Some(base64) = image.base64.as_ref() {
        value["base64"] = serde_json::json!(base64);
    }
    if let Some(detail) = image.detail.as_ref() {
        value["detail"] = serde_json::json!(detail);
    }
    value
}
