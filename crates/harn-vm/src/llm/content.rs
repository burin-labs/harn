//! Provider-neutral multimodal content helpers.
//!
//! Harn scripts represent image inputs as content blocks:
//! `{type: "image", url?: string, base64?: string, media_type: string, detail?: "low"|"high"|"auto"}`.
//! Provider serializers translate that one shape into their native wire format.

use std::rc::Rc;

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
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "llm_call image content requires exactly one of url or base64",
            ))));
        }
        let media_type = block
            .get("media_type")
            .or_else(|| block.get("mime_type"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(Rc::from(
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
                return Err(VmError::Thrown(VmValue::String(Rc::from(
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

pub(crate) fn parse_image_block(
    block: &serde_json::Value,
) -> Result<Option<ImageContent>, VmError> {
    ImageContent::from_block(block)
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
        Some("text") | Some("output_text") => Some(serde_json::json!({
            "type": "text",
            "text": block.get("text").and_then(|value| value.as_str()).unwrap_or_default(),
        })),
        _ => None,
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
                    out.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": image_url,
                    }));
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
                serde_json::Value::Array(vec![serde_json::json!({
                    "type": "image_url",
                    "image_url": image_url,
                })])
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
                if let Some(text) = normalized_text_block(block) {
                    return Some(serde_json::json!({
                        "text": text.get("text").and_then(|value| value.as_str()).unwrap_or_default(),
                    }));
                }
                block.get("text")
                    .and_then(|value| value.as_str())
                    .map(|text| serde_json::json!({"text": text}))
            })
            .collect(),
        other => vec![serde_json::json!({"text": other.to_string()})],
    }
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
