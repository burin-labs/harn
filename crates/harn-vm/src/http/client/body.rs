use crate::value::{VmError, VmValue};

use super::{ensure_response_body_within_limit, vm_error};

pub(in crate::http) enum ResponseBody {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<String> for ResponseBody {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

#[derive(Clone, Copy)]
pub(super) enum ResponseMode {
    Text,
    Bytes,
}

impl ResponseMode {
    pub(super) fn parse(options: &crate::value::DictMap) -> Result<Self, VmError> {
        match options.get("response_type") {
            None => Ok(Self::Text),
            Some(VmValue::String(value)) if value.as_str() == "text" => Ok(Self::Text),
            Some(VmValue::String(value)) if value.as_str() == "bytes" => Ok(Self::Bytes),
            Some(_) => Err(vm_error("http: response_type must be text or bytes")),
        }
    }

    pub(super) fn decode(self, bytes: Vec<u8>) -> Result<ResponseBody, VmError> {
        match self {
            Self::Bytes => Ok(ResponseBody::Bytes(bytes)),
            Self::Text => String::from_utf8(bytes)
                .map(ResponseBody::Text)
                .map_err(|error| {
                    vm_error(format!("http: response body is not valid UTF-8: {error}"))
                }),
        }
    }
}

pub(super) async fn read_response_body_limited(
    mut response: reqwest::Response,
    max_bytes: usize,
    mode: ResponseMode,
) -> Result<ResponseBody, VmError> {
    if response
        .content_length()
        .is_some_and(|len| len > max_bytes as u64)
    {
        return Err(vm_error(format!(
            "http: response body exceeded max_response_bytes ({max_bytes})"
        )));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        vm_error(format!(
            "http: failed to read response body: {}",
            crate::egress::redact_reqwest_error(&error)
        ))
    })? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| vm_error("http: response body length overflow"))?;
        ensure_response_body_within_limit(next_len, max_bytes)?;
        body.extend_from_slice(&chunk);
    }
    mode.decode(body)
}
