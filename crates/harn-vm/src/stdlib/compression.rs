use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::rc::Rc;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::{Compression, GzBuilder};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_GZIP_LEVEL: i64 = 6;
const DEFAULT_ZSTD_LEVEL: i64 = 3;
const DEFAULT_BROTLI_QUALITY: i64 = 11;
const DEFAULT_TAR_MODE: i64 = 0o644;

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
    VmValue::Bytes(Rc::new(bytes))
}

fn entry_value(fields: BTreeMap<String, VmValue>) -> VmValue {
    VmValue::Dict(Rc::new(fields))
}

fn gzip_encode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

fn gzip_decode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "gzip_decode")?;
    let mut decoder = GzDecoder::new(input);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|error| builtin_error("gzip_decode", error))?;
    Ok(bytes_value(output))
}

fn zstd_encode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

fn zstd_decode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "zstd_decode")?;
    zstd::stream::decode_all(Cursor::new(input))
        .map(bytes_value)
        .map_err(|error| builtin_error("zstd_decode", error))
}

fn brotli_encode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

fn brotli_decode_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "brotli_decode")?;
    let mut reader = brotli::Decompressor::new(Cursor::new(input), 4096);
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .map_err(|error| builtin_error("brotli_decode", error))?;
    Ok(bytes_value(output))
}

fn tar_create_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

fn tar_extract_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "tar_extract")?;
    let mut archive = tar::Archive::new(Cursor::new(input));
    let entries = archive
        .entries()
        .map_err(|error| builtin_error("tar_extract", error))?;
    let mut output = Vec::new();

    for entry in entries {
        let mut entry = entry.map_err(|error| builtin_error("tar_extract", error))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| builtin_error("tar_extract", error))?
            .to_str()
            .ok_or_else(|| builtin_error("tar_extract", "entry path is not valid UTF-8"))?
            .to_string();
        let mode = entry.header().mode().unwrap_or(DEFAULT_TAR_MODE as u32) as i64;
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|error| builtin_error("tar_extract", error))?;

        let mut fields = BTreeMap::new();
        fields.insert("content".to_string(), bytes_value(content));
        fields.insert("mode".to_string(), VmValue::Int(mode));
        fields.insert("path".to_string(), VmValue::String(Rc::from(path)));
        output.push(entry_value(fields));
    }

    Ok(VmValue::List(Rc::new(output)))
}

fn zip_create_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
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

fn zip_extract_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let input = expect_bytes(args, 0, "zip_extract")?;
    let cursor = Cursor::new(input);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|error| builtin_error("zip_extract", error))?;
    let mut output = Vec::new();

    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| builtin_error("zip_extract", error))?;
        if file.is_dir() {
            continue;
        }
        let path = file.name().to_string();
        let mut content = Vec::new();
        file.read_to_end(&mut content)
            .map_err(|error| builtin_error("zip_extract", error))?;

        let mut fields = BTreeMap::new();
        fields.insert("content".to_string(), bytes_value(content));
        fields.insert("path".to_string(), VmValue::String(Rc::from(path)));
        output.push(entry_value(fields));
    }

    Ok(VmValue::List(Rc::new(output)))
}

pub(crate) fn register_compression_builtins(vm: &mut Vm) {
    vm.register_builtin("gzip_encode", |args, _out| gzip_encode_builtin(args));
    vm.register_builtin("gzip_decode", |args, _out| gzip_decode_builtin(args));
    vm.register_builtin("zstd_encode", |args, _out| zstd_encode_builtin(args));
    vm.register_builtin("zstd_decode", |args, _out| zstd_decode_builtin(args));
    vm.register_builtin("brotli_encode", |args, _out| brotli_encode_builtin(args));
    vm.register_builtin("brotli_decode", |args, _out| brotli_decode_builtin(args));
    vm.register_builtin("tar_create", |args, _out| tar_create_builtin(args));
    vm.register_builtin("tar_extract", |args, _out| tar_extract_builtin(args));
    vm.register_builtin("zip_create", |args, _out| zip_create_builtin(args));
    vm.register_builtin("zip_extract", |args, _out| zip_extract_builtin(args));
}

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
        VmValue::String(Rc::from(value))
    }

    #[test]
    fn gzip_round_trips_strings_to_bytes() {
        let mut vm = vm();
        let encoded = call(&mut vm, "gzip_encode", vec![text("hello"), VmValue::Int(1)]).unwrap();
        let decoded = call(&mut vm, "gzip_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.as_bytes().unwrap(), b"hello");
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
            vec![VmValue::List(Rc::new(vec![entry_value(fields)]))],
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
