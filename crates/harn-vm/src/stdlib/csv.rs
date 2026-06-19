//! CSV parse / stringify builtins.
//!
//! `csv_parse(text, opts?)` — returns either a list of lists (when no
//! header) or a list of dicts (when `headers: true`).
//! `csv_stringify(rows, opts?)` — accepts either rows-of-lists or
//! rows-of-dicts; with `headers: true` and dicts, the union of keys
//! becomes the header row (sorted for determinism).

use std::collections::{BTreeMap, BTreeSet};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn opt_bool(opts: Option<&crate::value::DictMap>, key: &str, default: bool) -> bool {
    opts.and_then(|d| match d.get(key) {
        Some(VmValue::Bool(b)) => Some(*b),
        _ => None,
    })
    .unwrap_or(default)
}

fn opt_delimiter(
    opts: Option<&crate::value::DictMap>,
    key: &str,
    default: u8,
    builtin: &str,
) -> Result<u8, VmError> {
    let Some(value) = opts.and_then(|d| d.get(key)) else {
        return Ok(default);
    };
    let VmValue::String(raw) = value else {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{builtin}: {key} must be a string"),
        ))));
    };
    let bytes = raw.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{builtin}: {key} must be exactly one ASCII character"),
        ))));
    }
    Ok(bytes[0])
}

pub(crate) fn register_csv_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] =
    &[&CSV_PARSE_IMPL_DEF, &CSV_STRINGIFY_IMPL_DEF];

#[harn_builtin(
    sig = "csv_parse(text: string?, options?: dict) -> list",
    category = "csv"
)]
fn csv_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let opts = args.get(1).and_then(|v| match v {
        VmValue::Dict(d) => Some(&**d),
        _ => None,
    });
    let has_headers = opt_bool(opts, "headers", false);
    let delimiter = opt_delimiter(opts, "delimiter", b',', "csv_parse")?;

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(text.as_bytes());

    if has_headers {
        let headers = reader
            .headers()
            .map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_parse: {e}"
                ))))
            })?
            .clone();
        let mut rows: Vec<VmValue> = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_parse: {e}"
                ))))
            })?;
            let mut row = BTreeMap::new();
            for (i, h) in headers.iter().enumerate() {
                let cell = record.get(i).unwrap_or("");
                row.insert(h.to_string(), VmValue::String(arcstr::ArcStr::from(cell)));
            }
            rows.push(VmValue::dict(row));
        }
        Ok(VmValue::List(std::sync::Arc::new(rows)))
    } else {
        let mut rows: Vec<VmValue> = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_parse: {e}"
                ))))
            })?;
            let cells: Vec<VmValue> = record
                .iter()
                .map(|c| VmValue::String(arcstr::ArcStr::from(c)))
                .collect();
            rows.push(VmValue::List(std::sync::Arc::new(cells)));
        }
        Ok(VmValue::List(std::sync::Arc::new(rows)))
    }
}

#[harn_builtin(
    sig = "csv_stringify(rows: list, options?: dict) -> string",
    category = "csv"
)]
fn csv_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::List(rows)) = args.first() else {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "csv_stringify: expected a list of rows",
        ))));
    };
    let opts = args.get(1).and_then(|v| match v {
        VmValue::Dict(d) => Some(&**d),
        _ => None,
    });
    let want_headers = opt_bool(opts, "headers", false);
    let delimiter = opt_delimiter(opts, "delimiter", b',', "csv_stringify")?;

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());

    // Detect the row shape from the first element.
    let dict_mode = matches!(rows.first(), Some(VmValue::Dict(_)));

    if dict_mode {
        // Compute the union of keys (sorted) for stable headers.
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for row in rows.iter() {
            if let VmValue::Dict(d) = row {
                for k in d.keys() {
                    keys.insert(k.clone());
                }
            }
        }
        let header: Vec<String> = keys.into_iter().collect();
        if want_headers {
            wtr.write_record(&header).map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_stringify: {e}"
                ))))
            })?;
        }
        for row in rows.iter() {
            let VmValue::Dict(d) = row else {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "csv_stringify: mixed list/dict rows are not supported",
                ))));
            };
            let cells: Vec<String> = header
                .iter()
                .map(|k| d.get(k).map(|v| v.display()).unwrap_or_default())
                .collect();
            wtr.write_record(&cells).map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_stringify: {e}"
                ))))
            })?;
        }
    } else {
        for row in rows.iter() {
            let VmValue::List(cells) = row else {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "csv_stringify: each row must be a list of cells (or use dict rows)",
                ))));
            };
            let cells: Vec<String> = cells.iter().map(|v| v.display()).collect();
            wtr.write_record(&cells).map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "csv_stringify: {e}"
                ))))
            })?;
        }
    }

    let bytes = wtr.into_inner().map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "csv_stringify: {e}"
        ))))
    })?;
    String::from_utf8(bytes)
        .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "csv_stringify: {error}"
            ))))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_csv_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn string(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn list(items: Vec<VmValue>) -> VmValue {
        VmValue::List(std::sync::Arc::new(items))
    }

    fn dict(items: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
        VmValue::dict(
            items
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<crate::value::DictMap>(),
        )
    }

    #[test]
    fn csv_stringify_rejects_non_ascii_delimiter() {
        let mut vm = vm();
        let rows = list(vec![list(vec![string("a"), string("b")])]);
        let options = dict([("delimiter", string("é"))]);
        let error = call(&mut vm, "csv_stringify", vec![rows, options])
            .expect_err("delimiter must be a single ASCII byte");
        assert!(
            error.to_string().contains("delimiter"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn csv_parse_rejects_multi_character_delimiter() {
        let mut vm = vm();
        let options = dict([("delimiter", string("||"))]);
        let error = call(&mut vm, "csv_parse", vec![string("a||b\n"), options])
            .expect_err("delimiter must be one character");
        assert!(
            error.to_string().contains("exactly one ASCII"),
            "unexpected error: {error}"
        );
    }
}
