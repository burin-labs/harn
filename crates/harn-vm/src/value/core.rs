use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::{cell::RefCell, future::Future, pin::Pin};

use crate::mcp::VmMcpClientHandle;
use crate::BuiltinId;

use super::{VmAtomicHandle, VmChannelHandle, VmClosure, VmError, VmGenerator, VmRange};

/// An async builtin function for the VM.
pub type VmAsyncBuiltinFn =
    Rc<dyn Fn(Vec<VmValue>) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>>>>>;

/// Indexed runtime layout for a Harn struct instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    struct_name: String,
    field_names: Vec<String>,
    field_indexes: HashMap<String, usize>,
}

impl StructLayout {
    pub fn new(struct_name: impl Into<String>, field_names: Vec<String>) -> Self {
        let mut deduped = Vec::with_capacity(field_names.len());
        let mut field_indexes = HashMap::with_capacity(field_names.len());
        for field_name in field_names {
            if field_indexes.contains_key(&field_name) {
                continue;
            }
            let index = deduped.len();
            field_indexes.insert(field_name.clone(), index);
            deduped.push(field_name);
        }

        Self {
            struct_name: struct_name.into(),
            field_names: deduped,
            field_indexes,
        }
    }

    pub fn from_map(struct_name: impl Into<String>, fields: &BTreeMap<String, VmValue>) -> Self {
        Self::new(struct_name, fields.keys().cloned().collect())
    }

    pub fn struct_name(&self) -> &str {
        &self.struct_name
    }

    pub fn field_names(&self) -> &[String] {
        &self.field_names
    }

    pub fn field_index(&self, field_name: &str) -> Option<usize> {
        if self.field_names.len() <= 8 {
            return self
                .field_names
                .iter()
                .position(|candidate| candidate == field_name);
        }
        self.field_indexes.get(field_name).copied()
    }

    pub fn with_appended_field(&self, field_name: String) -> Self {
        if self.field_indexes.contains_key(&field_name) {
            return self.clone();
        }
        let mut field_names = self.field_names.clone();
        field_names.push(field_name);
        Self::new(self.struct_name.clone(), field_names)
    }
}

/// VM runtime value.
///
/// Rare compound payloads use shared pointers so cloning or moving common
/// values does not pay for the largest enum alternatives. Unsafe layouts such
/// as NaN boxing or tagged pointers are deliberately deferred until Harn has a
/// stronger object/heap story.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    String(Rc<str>),
    Bytes(Rc<Vec<u8>>),
    Bool(bool),
    Nil,
    List(Rc<Vec<VmValue>>),
    Dict(Rc<BTreeMap<String, VmValue>>),
    Closure(Rc<VmClosure>),
    /// Reference to a registered builtin function, used when a builtin name is
    /// referenced as a value (e.g. `snake_dict.rekey(snake_to_camel)`). The
    /// contained string is the builtin's registered name.
    BuiltinRef(Rc<str>),
    /// Compact builtin reference for callback positions. Carries the name for
    /// policy, diagnostics, and fallback if the ID cannot be used.
    BuiltinRefId {
        id: BuiltinId,
        name: Rc<str>,
    },
    Duration(u64),
    EnumVariant {
        enum_name: Rc<str>,
        variant: Rc<str>,
        fields: Rc<Vec<VmValue>>,
    },
    StructInstance {
        layout: Rc<StructLayout>,
        fields: Rc<Vec<Option<VmValue>>>,
    },
    TaskHandle(String),
    Channel(VmChannelHandle),
    Atomic(VmAtomicHandle),
    McpClient(VmMcpClientHandle),
    Set(Rc<Vec<VmValue>>),
    Generator(VmGenerator),
    Range(VmRange),
    /// Lazy iterator handle. Single-pass, fused. See `crate::vm::iter::VmIter`.
    Iter(Rc<RefCell<crate::vm::iter::VmIter>>),
    /// Two-element pair value. Produced by `pair(a, b)`, yielded by the
    /// Dict iterator source, and (later) by `zip` / `enumerate` combinators.
    /// Accessed via `.first` / `.second`, and destructurable in
    /// `for (a, b) in ...` loops.
    Pair(Rc<(VmValue, VmValue)>),
}

impl VmValue {
    pub fn enum_variant(
        enum_name: impl Into<Rc<str>>,
        variant: impl Into<Rc<str>>,
        fields: Vec<VmValue>,
    ) -> Self {
        VmValue::EnumVariant {
            enum_name: enum_name.into(),
            variant: variant.into(),
            fields: Rc::new(fields),
        }
    }

    pub fn struct_instance(
        struct_name: impl Into<Rc<str>>,
        fields: BTreeMap<String, VmValue>,
    ) -> Self {
        Self::struct_instance_from_map(struct_name.into().to_string(), fields)
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            VmValue::Bool(b) => *b,
            VmValue::Nil => false,
            VmValue::Int(n) => *n != 0,
            VmValue::Float(n) => *n != 0.0,
            VmValue::String(s) => !s.is_empty(),
            VmValue::Bytes(bytes) => !bytes.is_empty(),
            VmValue::List(l) => !l.is_empty(),
            VmValue::Dict(d) => !d.is_empty(),
            VmValue::Closure(_) => true,
            VmValue::BuiltinRef(_) => true,
            VmValue::BuiltinRefId { .. } => true,
            VmValue::Duration(ms) => *ms > 0,
            VmValue::EnumVariant { .. } => true,
            VmValue::StructInstance { .. } => true,
            VmValue::TaskHandle(_) => true,
            VmValue::Channel(_) => true,
            VmValue::Atomic(_) => true,
            VmValue::McpClient(_) => true,
            VmValue::Set(s) => !s.is_empty(),
            VmValue::Generator(_) => true,
            // Match Python semantics: range objects are always truthy,
            // even the empty range (analogous to generators / iterators).
            VmValue::Range(_) => true,
            VmValue::Iter(_) => true,
            VmValue::Pair(_) => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            VmValue::String(_) => "string",
            VmValue::Bytes(_) => "bytes",
            VmValue::Int(_) => "int",
            VmValue::Float(_) => "float",
            VmValue::Bool(_) => "bool",
            VmValue::Nil => "nil",
            VmValue::List(_) => "list",
            VmValue::Dict(_) => "dict",
            VmValue::Closure(_) => "closure",
            VmValue::BuiltinRef(_) => "builtin",
            VmValue::BuiltinRefId { .. } => "builtin",
            VmValue::Duration(_) => "duration",
            VmValue::EnumVariant { .. } => "enum",
            VmValue::StructInstance { .. } => "struct",
            VmValue::TaskHandle(_) => "task_handle",
            VmValue::Channel(_) => "channel",
            VmValue::Atomic(_) => "atomic",
            VmValue::McpClient(_) => "mcp_client",
            VmValue::Set(_) => "set",
            VmValue::Generator(_) => "generator",
            VmValue::Range(_) => "range",
            VmValue::Iter(_) => "iter",
            VmValue::Pair(_) => "pair",
        }
    }

    pub fn struct_name(&self) -> Option<&str> {
        match self {
            VmValue::StructInstance { layout, .. } => Some(layout.struct_name()),
            _ => None,
        }
    }

    pub fn struct_field(&self, field_name: &str) -> Option<&VmValue> {
        match self {
            VmValue::StructInstance { layout, fields } => layout
                .field_index(field_name)
                .and_then(|index| fields.get(index))
                .and_then(Option::as_ref),
            _ => None,
        }
    }

    pub fn struct_fields_map(&self) -> Option<BTreeMap<String, VmValue>> {
        match self {
            VmValue::StructInstance { layout, fields } => {
                Some(struct_fields_to_map(layout, fields))
            }
            _ => None,
        }
    }

    pub fn struct_instance_from_map(
        struct_name: impl Into<String>,
        fields: BTreeMap<String, VmValue>,
    ) -> Self {
        let layout = Rc::new(StructLayout::from_map(struct_name, &fields));
        let slots = layout
            .field_names()
            .iter()
            .map(|name| fields.get(name).cloned())
            .collect();
        VmValue::StructInstance {
            layout,
            fields: Rc::new(slots),
        }
    }

    pub fn struct_instance_with_layout(
        struct_name: impl Into<String>,
        field_names: Vec<String>,
        field_values: BTreeMap<String, VmValue>,
    ) -> Self {
        let layout = Rc::new(StructLayout::new(struct_name, field_names));
        let fields = layout
            .field_names()
            .iter()
            .map(|name| field_values.get(name).cloned())
            .collect();
        VmValue::StructInstance {
            layout,
            fields: Rc::new(fields),
        }
    }

    pub fn struct_instance_with_property(
        &self,
        field_name: String,
        value: VmValue,
    ) -> Option<Self> {
        let VmValue::StructInstance { layout, fields } = self else {
            return None;
        };

        let mut new_fields = fields.as_ref().clone();
        let layout = match layout.field_index(&field_name) {
            Some(index) => {
                if index >= new_fields.len() {
                    new_fields.resize(index + 1, None);
                }
                new_fields[index] = Some(value);
                Rc::clone(layout)
            }
            None => {
                let new_layout = Rc::new(layout.with_appended_field(field_name));
                new_fields.push(Some(value));
                new_layout
            }
        };

        Some(VmValue::StructInstance {
            layout,
            fields: Rc::new(new_fields),
        })
    }

    pub fn display(&self) -> String {
        let mut out = String::new();
        self.write_display(&mut out);
        out
    }

    /// Writes the display representation directly into `out`,
    /// avoiding intermediate Vec<String> allocations for collections.
    pub fn write_display(&self, out: &mut String) {
        use std::fmt::Write;

        match self {
            VmValue::Int(n) => {
                let _ = write!(out, "{n}");
            }
            VmValue::Float(n) => {
                if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                    let _ = write!(out, "{n:.1}");
                } else {
                    let _ = write!(out, "{n}");
                }
            }
            VmValue::String(s) => out.push_str(s),
            VmValue::Bytes(bytes) => {
                const MAX_PREVIEW_BYTES: usize = 32;

                out.push_str("b\"");
                for byte in bytes.iter().take(MAX_PREVIEW_BYTES) {
                    let _ = write!(out, "{byte:02x}");
                }
                if bytes.len() > MAX_PREVIEW_BYTES {
                    let _ = write!(out, "...+{}", bytes.len() - MAX_PREVIEW_BYTES);
                }
                out.push('"');
            }
            VmValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            VmValue::Nil => out.push_str("nil"),
            VmValue::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_display(out);
                }
                out.push(']');
            }
            VmValue::Dict(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(": ");
                    v.write_display(out);
                }
                out.push('}');
            }
            VmValue::Closure(c) => {
                let _ = write!(out, "<fn({})>", c.func.params.join(", "));
            }
            VmValue::BuiltinRef(name) => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::BuiltinRefId { name, .. } => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::Duration(ms) => {
                if *ms >= 3_600_000 && ms % 3_600_000 == 0 {
                    let _ = write!(out, "{}h", ms / 3_600_000);
                } else if *ms >= 60_000 && ms % 60_000 == 0 {
                    let _ = write!(out, "{}m", ms / 60_000);
                } else if *ms >= 1000 && ms % 1000 == 0 {
                    let _ = write!(out, "{}s", ms / 1000);
                } else {
                    let _ = write!(out, "{}ms", ms);
                }
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    let _ = write!(out, "{enum_name}.{variant}");
                } else {
                    let _ = write!(out, "{enum_name}.{variant}(");
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        v.write_display(out);
                    }
                    out.push(')');
                }
            }
            VmValue::StructInstance { layout, fields } => {
                let _ = write!(out, "{} {{", layout.struct_name());
                for (i, (k, v)) in struct_fields_to_map(layout, fields).iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(": ");
                    v.write_display(out);
                }
                out.push('}');
            }
            VmValue::TaskHandle(id) => {
                let _ = write!(out, "<task:{id}>");
            }
            VmValue::Channel(ch) => {
                let _ = write!(out, "<channel:{}>", ch.name);
            }
            VmValue::Atomic(a) => {
                let _ = write!(out, "<atomic:{}>", a.value.load(Ordering::SeqCst));
            }
            VmValue::McpClient(c) => {
                let _ = write!(out, "<mcp_client:{}>", c.name);
            }
            VmValue::Set(items) => {
                out.push_str("set(");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_display(out);
                }
                out.push(')');
            }
            VmValue::Generator(g) => {
                if g.done.get() {
                    out.push_str("<generator (done)>");
                } else {
                    out.push_str("<generator>");
                }
            }
            // Print form mirrors source syntax: `1 to 5` / `0 to 3 exclusive`.
            // `.to_list()` is the explicit path to materialize for display.
            VmValue::Range(r) => {
                let _ = write!(out, "{} to {}", r.start, r.end);
                if !r.inclusive {
                    out.push_str(" exclusive");
                }
            }
            VmValue::Iter(h) => {
                if matches!(&*h.borrow(), crate::vm::iter::VmIter::Exhausted) {
                    out.push_str("<iter (exhausted)>");
                } else {
                    out.push_str("<iter>");
                }
            }
            VmValue::Pair(p) => {
                out.push('(');
                p.0.write_display(out);
                out.push_str(", ");
                p.1.write_display(out);
                out.push(')');
            }
        }
    }

    /// Get the value as a BTreeMap reference, if it's a Dict.
    pub fn as_dict(&self) -> Option<&BTreeMap<String, VmValue>> {
        if let VmValue::Dict(d) = self {
            Some(d)
        } else {
            None
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        if let VmValue::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        if let VmValue::Bytes(bytes) = self {
            Some(bytes.as_slice())
        } else {
            None
        }
    }
}

pub fn struct_fields_to_map(
    layout: &StructLayout,
    fields: &[Option<VmValue>],
) -> BTreeMap<String, VmValue> {
    layout
        .field_names()
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            fields
                .get(index)
                .and_then(Option::as_ref)
                .map(|value| (name.clone(), value.clone()))
        })
        .collect()
}

/// Sync builtin function for the VM.
pub type VmBuiltinFn = Rc<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError>>;
