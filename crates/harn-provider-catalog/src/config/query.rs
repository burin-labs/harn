//! Pure projections of catalog and capability records.

use super::{AuthEnv, ModelDef, ProvidersConfig};
use crate::capabilities::Capabilities;

pub fn sorted_model_entries_with_config(config: &ProvidersConfig) -> Vec<(String, ModelDef)> {
    let mut entries: Vec<_> = config
        .models
        .iter()
        .map(|(id, model)| (id.clone(), model.clone()))
        .collect();
    entries.sort_by(|(id_a, model_a), (id_b, model_b)| {
        model_a
            .provider
            .cmp(&model_b.provider)
            .then_with(|| id_a.cmp(id_b))
    });
    entries
}

pub fn auth_env_names(auth_env: &AuthEnv) -> Vec<String> {
    match auth_env {
        AuthEnv::None => Vec::new(),
        AuthEnv::Single(name) => vec![name.clone()],
        AuthEnv::Multiple(names) => names.clone(),
    }
}

pub fn capability_tags_from_capabilities(caps: &Capabilities) -> Vec<String> {
    let mut tags = vec!["streaming".to_string()];
    if caps.native_tools || caps.text_tool_wire_format_supported {
        tags.push("tools".into());
    }
    if !caps.tool_search.is_empty() {
        tags.push("tool_search".into());
    }
    if caps.vision || caps.vision_supported {
        tags.push("vision".into());
    }
    if caps.audio {
        tags.push("audio".into());
    }
    if caps.pdf {
        tags.push("pdf".into());
    }
    if caps.video {
        tags.push("video".into());
    }
    if caps.files_api_supported {
        tags.push("files".into());
    }
    if caps.batch_api {
        tags.push("batch".into());
    }
    if caps.prompt_caching {
        tags.push("prompt_caching".into());
    }
    if !caps.thinking_modes.is_empty() {
        tags.push("thinking".into());
    }
    if caps.interleaved_thinking_supported
        || caps
            .thinking_modes
            .iter()
            .any(|mode| mode == "adaptive" || mode == "effort")
    {
        tags.push("extended_thinking".into());
    }
    if caps.structured_output.is_some() || caps.json_schema.is_some() {
        tags.push("structured_output".into());
    }
    tags
}
