use serde_json::json;

use super::state::Debugger;
use crate::protocol::{DapMessage, DapResponse, Source};

impl Debugger {
    pub(crate) fn handle_source(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let args = msg.arguments.as_ref();
        let source_reference = args
            .and_then(|a| a.get("sourceReference"))
            .and_then(|v| v.as_i64())
            .filter(|v| *v > 0);
        let source_path = args
            .and_then(|a| a.get("source"))
            .and_then(|s| s.get("path"))
            .and_then(|p| p.as_str())
            .map(str::to_string);

        let ref_path = source_reference.and_then(|id| self.source_refs.get(&id).cloned());
        let Some(path) = ref_path.or(source_path) else {
            return vec![self.dap_error(
                msg,
                "source",
                "source request requires source.path or sourceReference",
            )];
        };

        match self.source_content_for_path(&path) {
            Some(content) => {
                let seq = self.next_seq();
                vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "source",
                    Some(json!({
                        "content": content,
                        "mimeType": source_mime_type(&path),
                    })),
                )]
            }
            None => {
                vec![self.dap_error(msg, "source", &format!("source not available for '{path}'"))]
            }
        }
    }

    pub(crate) fn source_for_path(&mut self, path: &str) -> Source {
        let has_cached_source = (self.source_path.as_deref() == Some(path)
            && self.source_content.is_some())
            || self
                .vm
                .as_ref()
                .and_then(|vm| vm.debug_source_for_path(path))
                .is_some()
            || stdlib_source(path).is_some();
        let source_reference = if has_cached_source {
            Some(self.source_reference_for_path(path))
        } else {
            None
        };

        Source {
            name: std::path::Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .or_else(|| Some(path.to_string())),
            path: Some(path.to_string()),
            source_reference,
        }
    }

    fn source_reference_for_path(&mut self, path: &str) -> i64 {
        if let Some(id) = self.source_ref_by_path.get(path) {
            return *id;
        }
        let id = self.next_source_ref;
        self.next_source_ref += 1;
        self.source_ref_by_path.insert(path.to_string(), id);
        self.source_refs.insert(id, path.to_string());
        id
    }

    fn source_content_for_path(&self, path: &str) -> Option<String> {
        if self.source_path.as_deref() == Some(path) {
            if let Some(source) = &self.source_content {
                return Some(source.clone());
            }
        }

        if let Some(vm) = &self.vm {
            if let Some(source) = vm.debug_source_for_path(path) {
                return Some(source);
            }
        }

        if let Some(source) = stdlib_source(path) {
            return Some(source.to_string());
        }

        let fs_path = file_uri_to_path(path).unwrap_or_else(|| std::path::PathBuf::from(path));
        std::fs::read_to_string(fs_path).ok()
    }
}

fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let local = if rest == "localhost" {
        ""
    } else if rest.starts_with("localhost/") {
        &rest["localhost".len()..]
    } else {
        rest
    };
    let decoded = percent_decode(local)?;
    #[cfg(windows)]
    {
        let path = decoded.strip_prefix('/').unwrap_or(&decoded);
        Some(std::path::PathBuf::from(path))
    }
    #[cfg(not(windows))]
    {
        Some(std::path::PathBuf::from(decoded))
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            out.push((hex_value(hi)? << 4) | hex_value(lo)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn stdlib_source(path: &str) -> Option<&'static str> {
    let module = path
        .strip_prefix("<stdlib>/")
        .and_then(|s| s.strip_suffix(".harn"))?;
    harn_vm::stdlib_modules::get_stdlib_source(module)
}

fn source_mime_type(path: &str) -> &'static str {
    if path.ends_with(".harn") {
        "text/x-harn"
    } else if path.ends_with(".harn.prompt") {
        "text/x-harn-prompt"
    } else {
        "text/plain"
    }
}
