use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::{Compression, GzBuilder};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_GZIP_LEVEL: i64 = 6;
const DEFAULT_ZSTD_LEVEL: i64 = 3;
const DEFAULT_BROTLI_QUALITY: i64 = 11;
const DEFAULT_TAR_MODE: i64 = 0o644;
/// Default decompression bomb cap: refuse to expand any single archive
/// or stream past 100 MiB unless the caller explicitly opts in with
/// `max_decompressed_bytes`. Picked to comfortably cover normal
/// payloads (LLM responses, workflow bundles, screenshot tarballs) but
/// stay well under the smallest realistic memory budget.
pub(crate) const MAX_DECOMPRESSED_BYTES: u64 = 100 * 1024 * 1024;

/// Resolve a `max_decompressed_bytes` (alias `maxDecompressedBytes`)
/// option from the args dict at `index`, falling back to
/// [`MAX_DECOMPRESSED_BYTES`] when absent.
fn decompress_cap(args: &[VmValue], index: usize, builtin: &str) -> Result<u64, VmError> {
    let Some(value) = args.get(index) else {
        return Ok(MAX_DECOMPRESSED_BYTES);
    };
    let options = match value {
        VmValue::Nil => return Ok(MAX_DECOMPRESSED_BYTES),
        VmValue::Dict(options) => options.as_ref(),
        other => {
            return Err(builtin_error(
                builtin,
                format!("options must be a dict, got {}", other.type_name()),
            ));
        }
    };
    let raw = options
        .get("max_decompressed_bytes")
        .or_else(|| options.get("maxDecompressedBytes"));
    match raw {
        None | Some(VmValue::Nil) => Ok(MAX_DECOMPRESSED_BYTES),
        Some(VmValue::Int(value)) if *value > 0 => Ok(*value as u64),
        Some(VmValue::Int(_)) => Err(builtin_error(
            builtin,
            "max_decompressed_bytes must be a positive integer",
        )),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "max_decompressed_bytes must be a positive integer, got {}",
                other.type_name()
            ),
        )),
    }
}

/// Decompress `source` into a `Vec<u8>`, refusing to grow past `cap`.
/// Returns a clean error message instead of OOMing when the stream
/// (gzip/zstd/brotli) expands past the configured limit.
fn read_with_cap<R: Read>(source: &mut R, cap: u64, builtin: &str) -> Result<Vec<u8>, VmError> {
    let mut limited = source.take(cap.saturating_add(1));
    let mut output = Vec::new();
    limited
        .read_to_end(&mut output)
        .map_err(|error| builtin_error(builtin, error))?;
    if output.len() as u64 > cap {
        return Err(builtin_error(
            builtin,
            format!(
                "decompressed output exceeded max_decompressed_bytes ({cap}); refusing to \
                 allocate further to avoid a decompression-bomb OOM"
            ),
        ));
    }
    Ok(output)
}

fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

fn builtin_error(builtin: &str, message: impl std::fmt::Display) -> VmError {
    runtime_error(format!("{builtin}: {message}"))
}

fn expect_bytes<'a>(args: &'a [VmValue], index: usize, builtin: &str) -> Result<&'a [u8], VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_slice()),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "expected bytes at argument {}, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("missing argument {}", index + 1),
        )),
    }
}

fn expect_bytes_or_string(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<Vec<u8>, VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_ref().clone()),
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "expected bytes or string at argument {}, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("missing argument {}", index + 1),
        )),
    }
}

fn optional_int(
    args: &[VmValue],
    index: usize,
    default: i64,
    builtin: &str,
    param: &str,
) -> Result<i64, VmError> {
    match args.get(index) {
        Some(VmValue::Int(value)) => Ok(*value),
        Some(VmValue::Nil) | None => Ok(default),
        Some(other) => Err(builtin_error(
            builtin,
            format!("{param} must be an int, got {}", other.type_name()),
        )),
    }
}

fn expect_level(
    args: &[VmValue],
    index: usize,
    default: i64,
    range: std::ops::RangeInclusive<i64>,
    builtin: &str,
    param: &str,
) -> Result<i64, VmError> {
    let value = optional_int(args, index, default, builtin, param)?;
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(builtin_error(
            builtin,
            format!(
                "{param} must be between {} and {}, got {value}",
                range.start(),
                range.end()
            ),
        ))
    }
}

fn entry_field<'a>(entry: &'a VmValue, field: &str) -> Option<&'a VmValue> {
    match entry {
        VmValue::Dict(map) => map.get(field),
        VmValue::StructInstance { .. } => entry.struct_field(field),
        _ => None,
    }
}

fn entry_path(entry: &VmValue, builtin: &str, index: usize) -> Result<String, VmError> {
    match entry_field(entry, "path") {
        Some(VmValue::String(path)) if !path.is_empty() => Ok(path.to_string()),
        Some(VmValue::String(_)) => Err(builtin_error(
            builtin,
            format!("entry {} path must not be empty", index + 1),
        )),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "entry {} path must be a string, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("entry {} is missing path", index + 1),
        )),
    }
}

fn entry_content(entry: &VmValue, builtin: &str, index: usize) -> Result<Vec<u8>, VmError> {
    match entry_field(entry, "content") {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_ref().clone()),
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "entry {} content must be bytes or string, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("entry {} is missing content", index + 1),
        )),
    }
}

fn entry_mode(entry: &VmValue, builtin: &str, index: usize) -> Result<u32, VmError> {
    let mode = match entry_field(entry, "mode") {
        Some(VmValue::Int(mode)) => *mode,
        Some(VmValue::Nil) | None => DEFAULT_TAR_MODE,
        Some(other) => {
            return Err(builtin_error(
                builtin,
                format!(
                    "entry {} mode must be an int, got {}",
                    index + 1,
                    other.type_name()
                ),
            ));
        }
    };

    if (0..=0o7777).contains(&mode) {
        Ok(mode as u32)
    } else {
        Err(builtin_error(
            builtin,
            format!("entry {} mode must be between 0 and 4095", index + 1),
        ))
    }
}

fn expect_entries<'a>(args: &'a [VmValue], builtin: &str) -> Result<&'a [VmValue], VmError> {
    match args.first() {
        Some(VmValue::List(entries)) => Ok(entries.as_slice()),
        Some(other) => Err(builtin_error(
            builtin,
            format!("entries must be a list, got {}", other.type_name()),
        )),
        None => Err(builtin_error(builtin, "missing argument 1")),
    }
}

fn bytes_value(bytes: Vec<u8>) -> VmValue {
    VmValue::Bytes(std::sync::Arc::new(bytes))
}

fn entry_value(fields: BTreeMap<String, VmValue>) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(fields))
}

#[harn_builtin(
    sig = "gzip_encode(input: bytes | string, level?: int) -> bytes",
    category = "compression"
)]
fn gzip_encode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes_or_string(args, 0, "gzip_encode")?;
    let level = expect_level(args, 1, DEFAULT_GZIP_LEVEL, 0..=9, "gzip_encode", "level")?;
    let mut encoder: GzEncoder<Vec<u8>> = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::new(level as u32));
    encoder
        .write_all(&input)
        .map_err(|error| builtin_error("gzip_encode", error))?;
    encoder
        .finish()
        .map(bytes_value)
        .map_err(|error| builtin_error("gzip_encode", error))
}

#[harn_builtin(
    sig = "gzip_decode(input: bytes, options?: dict) -> bytes",
    category = "compression"
)]
fn gzip_decode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "gzip_decode")?;
    let cap = decompress_cap(args, 1, "gzip_decode")?;
    let mut decoder = GzDecoder::new(input);
    let output = read_with_cap(&mut decoder, cap, "gzip_decode")?;
    Ok(bytes_value(output))
}

#[harn_builtin(
    sig = "zstd_encode(input: bytes | string, level?: int) -> bytes",
    category = "compression"
)]
fn zstd_encode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes_or_string(args, 0, "zstd_encode")?;
    let level_range = zstd::compression_level_range();
    let level = expect_level(
        args,
        1,
        DEFAULT_ZSTD_LEVEL,
        (*level_range.start() as i64)..=(*level_range.end() as i64),
        "zstd_encode",
        "level",
    )? as i32;
    zstd::stream::encode_all(Cursor::new(input), level)
        .map(bytes_value)
        .map_err(|error| builtin_error("zstd_encode", error))
}

#[harn_builtin(
    sig = "zstd_decode(input: bytes, options?: dict) -> bytes",
    category = "compression"
)]
fn zstd_decode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "zstd_decode")?;
    let cap = decompress_cap(args, 1, "zstd_decode")?;
    let mut decoder = zstd::stream::Decoder::new(Cursor::new(input))
        .map_err(|error| builtin_error("zstd_decode", error))?;
    let output = read_with_cap(&mut decoder, cap, "zstd_decode")?;
    Ok(bytes_value(output))
}

#[harn_builtin(
    sig = "brotli_encode(input: bytes | string, quality?: int) -> bytes",
    category = "compression"
)]
fn brotli_encode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes_or_string(args, 0, "brotli_encode")?;
    let quality = expect_level(
        args,
        1,
        DEFAULT_BROTLI_QUALITY,
        0..=11,
        "brotli_encode",
        "quality",
    )? as u32;
    let mut reader = brotli::CompressorReader::new(Cursor::new(input), 4096, quality, 22);
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .map_err(|error| builtin_error("brotli_encode", error))?;
    Ok(bytes_value(output))
}

#[harn_builtin(
    sig = "brotli_decode(input: bytes, options?: dict) -> bytes",
    category = "compression"
)]
fn brotli_decode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "brotli_decode")?;
    let cap = decompress_cap(args, 1, "brotli_decode")?;
    let mut reader = brotli::Decompressor::new(Cursor::new(input), 4096);
    let output = read_with_cap(&mut reader, cap, "brotli_decode")?;
    Ok(bytes_value(output))
}

#[harn_builtin(sig = "tar_create(entries: list) -> bytes", category = "compression")]
fn tar_create_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let entries = expect_entries(args, "tar_create")?;
    let mut output = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut output);
        for (index, entry) in entries.iter().enumerate() {
            let path = entry_path(entry, "tar_create", index)?;
            let content = entry_content(entry, "tar_create", index)?;
            let mode = entry_mode(entry, "tar_create", index)?;

            let mut header = tar::Header::new_gnu();
            header.set_path(&path).map_err(|error| {
                builtin_error(
                    "tar_create",
                    format!("entry {} invalid path: {error}", index + 1),
                )
            })?;
            header.set_size(content.len() as u64);
            header.set_mode(mode);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder
                .append(&header, content.as_slice())
                .map_err(|error| builtin_error("tar_create", error))?;
        }
        builder
            .finish()
            .map_err(|error| builtin_error("tar_create", error))?;
    }
    Ok(bytes_value(output))
}

#[harn_builtin(
    sig = "tar_extract(input: bytes, options?: dict) -> list",
    category = "compression"
)]
fn tar_extract_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "tar_extract")?;
    let cap = decompress_cap(args, 1, "tar_extract")?;
    let mut archive = tar::Archive::new(Cursor::new(input));
    let entries = archive
        .entries()
        .map_err(|error| builtin_error("tar_extract", error))?;
    let mut output = Vec::new();
    let mut total_extracted: u64 = 0;

    for entry in entries {
        let mut entry = entry.map_err(|error| builtin_error("tar_extract", error))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        // tar headers carry the entry's declared size; reject obvious
        // attacker-shaped values (e.g. u64::MAX) BEFORE allocating, so
        // a maliciously-crafted header cannot trigger a Vec reservation
        // that triggers an allocator abort.
        let declared_size = entry
            .header()
            .size()
            .map_err(|error| builtin_error("tar_extract", error))?;
        if declared_size > cap || declared_size.saturating_add(total_extracted) > cap {
            return Err(builtin_error(
                "tar_extract",
                format!(
                    "tar entry declares size {declared_size} which exceeds \
                     max_decompressed_bytes ({cap}); refusing to extract"
                ),
            ));
        }
        let path = entry
            .path()
            .map_err(|error| builtin_error("tar_extract", error))?
            .to_str()
            .ok_or_else(|| builtin_error("tar_extract", "entry path is not valid UTF-8"))?
            .to_string();
        let mode = entry.header().mode().unwrap_or(DEFAULT_TAR_MODE as u32) as i64;
        // Even when the header says the entry is small, the actual
        // body could be larger (gnu tar quirk, corrupt archive). Guard
        // the read with the remaining cap so we never go past the
        // overall limit.
        let remaining = cap.saturating_sub(total_extracted);
        let content = read_with_cap(&mut entry, remaining, "tar_extract")?;
        total_extracted = total_extracted.saturating_add(content.len() as u64);

        let mut fields = BTreeMap::new();
        fields.insert("content".to_string(), bytes_value(content));
        fields.insert("mode".to_string(), VmValue::Int(mode));
        fields.put_str("path", path);
        output.push(entry_value(fields));
    }

    Ok(VmValue::List(std::sync::Arc::new(output)))
}

#[harn_builtin(sig = "zip_create(entries: list) -> bytes", category = "compression")]
fn zip_create_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let entries = expect_entries(args, "zip_create")?;
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for (index, entry) in entries.iter().enumerate() {
        let path = entry_path(entry, "zip_create", index)?;
        let content = entry_content(entry, "zip_create", index)?;
        writer
            .start_file(path, options)
            .map_err(|error| builtin_error("zip_create", error))?;
        writer
            .write_all(&content)
            .map_err(|error| builtin_error("zip_create", error))?;
    }

    let cursor = writer
        .finish()
        .map_err(|error| builtin_error("zip_create", error))?;
    Ok(bytes_value(cursor.into_inner()))
}

#[harn_builtin(
    sig = "zip_extract(input: bytes, options?: dict) -> list",
    category = "compression"
)]
fn zip_extract_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "zip_extract")?;
    let cap = decompress_cap(args, 1, "zip_extract")?;
    let cursor = Cursor::new(input);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|error| builtin_error("zip_extract", error))?;
    let mut output = Vec::new();
    let mut total_extracted: u64 = 0;

    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| builtin_error("zip_extract", error))?;
        if file.is_dir() {
            continue;
        }
        // Reject entries that claim more uncompressed bytes than the
        // configured cap up-front (and reject the cumulative-overflow
        // case as well), so a zip bomb can't drain memory.
        let declared_size = file.size();
        if declared_size > cap || declared_size.saturating_add(total_extracted) > cap {
            return Err(builtin_error(
                "zip_extract",
                format!(
                    "zip entry declares uncompressed size {declared_size} which exceeds \
                     max_decompressed_bytes ({cap}); refusing to extract"
                ),
            ));
        }
        let path = file.name().to_string();
        let remaining = cap.saturating_sub(total_extracted);
        let content = read_with_cap(&mut file, remaining, "zip_extract")?;
        total_extracted = total_extracted.saturating_add(content.len() as u64);

        let mut fields = BTreeMap::new();
        fields.insert("content".to_string(), bytes_value(content));
        fields.put_str("path", path);
        output.push(entry_value(fields));
    }

    Ok(VmValue::List(std::sync::Arc::new(output)))
}

pub(crate) fn register_compression_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &GZIP_ENCODE_BUILTIN_DEF,
    &GZIP_DECODE_BUILTIN_DEF,
    &ZSTD_ENCODE_BUILTIN_DEF,
    &ZSTD_DECODE_BUILTIN_DEF,
    &BROTLI_ENCODE_BUILTIN_DEF,
    &BROTLI_DECODE_BUILTIN_DEF,
    &TAR_CREATE_BUILTIN_DEF,
    &TAR_EXTRACT_BUILTIN_DEF,
    &ZIP_CREATE_BUILTIN_DEF,
    &ZIP_EXTRACT_BUILTIN_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_compression_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn text(value: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(value))
    }

    #[test]
    fn gzip_round_trips_strings_to_bytes() {
        let mut vm = vm();
        let encoded = call(&mut vm, "gzip_encode", vec![text("hello"), VmValue::Int(1)]).unwrap();
        let decoded = call(&mut vm, "gzip_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.as_bytes().unwrap(), b"hello");
    }

    #[test]
    fn gzip_decode_rejects_output_past_cap() {
        let mut vm = vm();
        // 64 KiB of zeros compresses to a few hundred bytes (~64x); cap
        // it at 1 KiB and confirm the decoder refuses, rather than
        // happily returning the full output.
        let payload = vec![0u8; 64 * 1024];
        let encoded = call(
            &mut vm,
            "gzip_encode",
            vec![
                VmValue::Bytes(std::sync::Arc::new(payload)),
                VmValue::Int(6),
            ],
        )
        .unwrap();
        let mut options = BTreeMap::new();
        options.insert("max_decompressed_bytes".to_string(), VmValue::Int(1024));
        let result = call(
            &mut vm,
            "gzip_decode",
            vec![encoded, VmValue::Dict(std::sync::Arc::new(options))],
        );
        let err = result.expect_err("gzip_decode should reject output past cap");
        let message = match err {
            VmError::Runtime(message) => message,
            other => panic!("expected Runtime error, got {other:?}"),
        };
        assert!(
            message.contains("max_decompressed_bytes"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn zstd_decode_rejects_output_past_cap() {
        let mut vm = vm();
        let payload = vec![0u8; 64 * 1024];
        let encoded = call(
            &mut vm,
            "zstd_encode",
            vec![
                VmValue::Bytes(std::sync::Arc::new(payload)),
                VmValue::Int(3),
            ],
        )
        .unwrap();
        let mut options = BTreeMap::new();
        options.insert("max_decompressed_bytes".to_string(), VmValue::Int(1024));
        let result = call(
            &mut vm,
            "zstd_decode",
            vec![encoded, VmValue::Dict(std::sync::Arc::new(options))],
        );
        assert!(result.is_err(), "zstd_decode should reject output past cap");
    }

    #[test]
    fn tar_round_trip_preserves_mode() {
        let mut fields = BTreeMap::new();
        fields.insert("path".to_string(), text("bin/run"));
        fields.insert("content".to_string(), text("echo hi"));
        fields.insert("mode".to_string(), VmValue::Int(0o755));

        let mut vm = vm();
        let archive = call(
            &mut vm,
            "tar_create",
            vec![VmValue::List(std::sync::Arc::new(vec![entry_value(
                fields,
            )]))],
        )
        .unwrap();
        let extracted = call(&mut vm, "tar_extract", vec![archive]).unwrap();
        let VmValue::List(entries) = extracted else {
            panic!("expected list");
        };
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_dict().unwrap();
        assert_eq!(entry["path"].display(), "bin/run");
        assert_eq!(entry["content"].as_bytes().unwrap(), b"echo hi");
        assert_eq!(entry["mode"].as_int(), Some(0o755));
    }
}
