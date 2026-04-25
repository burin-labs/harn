//! CSV parse / stringify builtins.
//!
//! `csv_parse(text, opts?)` — returns either a list of lists (when no
//! header) or a list of dicts (when `headers: true`).
//! `csv_stringify(rows, opts?)` — accepts either rows-of-lists or
//! rows-of-dicts; with `headers: true` and dicts, the union of keys
//! becomes the header row (sorted for determinism).

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn opt_bool(opts: Option<&BTreeMap<String, VmValue>>, key: &str, default: bool) -> bool {
    opts.and_then(|d| match d.get(key) {
        Some(VmValue::Bool(b)) => Some(*b),
        _ => None,
    })
    .unwrap_or(default)
}

fn opt_char(opts: Option<&BTreeMap<String, VmValue>>, key: &str, default: u8) -> u8 {
    opts.and_then(|d| match d.get(key) {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.as_bytes()[0]),
        _ => None,
    })
    .unwrap_or(default)
}

pub(crate) fn register_csv_builtins(vm: &mut Vm) {
    vm.register_builtin("csv_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        let opts = args.get(1).and_then(|v| match v {
            VmValue::Dict(d) => Some(&**d),
            _ => None,
        });
        let has_headers = opt_bool(opts, "headers", false);
        let delimiter = opt_char(opts, "delimiter", b',');

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(has_headers)
            .delimiter(delimiter)
            .flexible(true)
            .from_reader(text.as_bytes());

        if has_headers {
            let headers = reader
                .headers()
                .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("csv_parse: {e}")))))?
                .clone();
            let mut rows: Vec<VmValue> = Vec::new();
            for record in reader.records() {
                let record = record.map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!("csv_parse: {e}"))))
                })?;
                let mut row = BTreeMap::new();
                for (i, h) in headers.iter().enumerate() {
                    let cell = record.get(i).unwrap_or("");
                    row.insert(h.to_string(), VmValue::String(Rc::from(cell)));
                }
                rows.push(VmValue::Dict(Rc::new(row)));
            }
            Ok(VmValue::List(Rc::new(rows)))
        } else {
            let mut rows: Vec<VmValue> = Vec::new();
            for record in reader.records() {
                let record = record.map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!("csv_parse: {e}"))))
                })?;
                let cells: Vec<VmValue> = record
                    .iter()
                    .map(|c| VmValue::String(Rc::from(c)))
                    .collect();
                rows.push(VmValue::List(Rc::new(cells)));
            }
            Ok(VmValue::List(Rc::new(rows)))
        }
    });

    vm.register_builtin("csv_stringify", |args, _out| {
        let Some(VmValue::List(rows)) = args.first() else {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "csv_stringify: expected a list of rows",
            ))));
        };
        let opts = args.get(1).and_then(|v| match v {
            VmValue::Dict(d) => Some(&**d),
            _ => None,
        });
        let want_headers = opt_bool(opts, "headers", false);
        let delimiter = opt_char(opts, "delimiter", b',');

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
                    VmError::Thrown(VmValue::String(Rc::from(format!("csv_stringify: {e}"))))
                })?;
            }
            for row in rows.iter() {
                let VmValue::Dict(d) = row else {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(
                        "csv_stringify: mixed list/dict rows are not supported",
                    ))));
                };
                let cells: Vec<String> = header
                    .iter()
                    .map(|k| d.get(k).map(|v| v.display()).unwrap_or_default())
                    .collect();
                wtr.write_record(&cells).map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!("csv_stringify: {e}"))))
                })?;
            }
        } else {
            for row in rows.iter() {
                let VmValue::List(cells) = row else {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(
                        "csv_stringify: each row must be a list of cells (or use dict rows)",
                    ))));
                };
                let cells: Vec<String> = cells.iter().map(|v| v.display()).collect();
                wtr.write_record(&cells).map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!("csv_stringify: {e}"))))
                })?;
            }
        }

        let bytes = wtr.into_inner().map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!("csv_stringify: {e}"))))
        })?;
        Ok(VmValue::String(Rc::from(
            String::from_utf8(bytes).unwrap_or_default(),
        )))
    });
}
