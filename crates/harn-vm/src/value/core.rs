use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{future::Future, pin::Pin};

use crate::harness::VmHarness;
use crate::mcp::VmMcpClientHandle;
use crate::BuiltinId;

use super::{
    VmAtomicHandle, VmChannelHandle, VmClosure, VmError, VmGenerator, VmRange, VmRngHandle,
    VmStream, VmSyncPermitHandle,
};

/// An async builtin function for the VM.
///
/// Receives an explicit [`crate::vm::AsyncBuiltinCtx`] handle (threaded by the
/// dispatch loop + the `#[harn_builtin]` macro) so handlers mint child VMs and
/// forward output through the ctx they were given instead of relying on hidden
/// task state.
pub type VmAsyncBuiltinFn = Arc<
    dyn Fn(
            crate::vm::AsyncBuiltinCtx,
            Vec<VmValue>,
        ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send>>
        + Send
        + Sync,
>;

type Shared<T> = Arc<T>;

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

/// Runtime payload for a Harn enum variant.
#[derive(Debug, Clone)]
pub struct VmEnumVariant {
    pub enum_name: Shared<str>,
    pub variant: Shared<str>,
    pub fields: Shared<Vec<VmValue>>,
}

impl VmEnumVariant {
    pub fn has_enum_name(&self, enum_name: &str) -> bool {
        self.enum_name.as_ref() == enum_name
    }

    pub fn is_variant(&self, enum_name: &str, variant: &str) -> bool {
        self.has_enum_name(enum_name) && self.variant.as_ref() == variant
    }
}

/// VM runtime value.
///
/// Rare compound payloads use shared pointers so stack/local-slot traffic is
/// bounded by the common scalar and pointer-sized value shapes. Unsafe layouts
/// such as NaN boxing or tagged pointers are deliberately deferred until Harn
/// has a stronger object/heap story.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    String(Shared<str>),
    Bytes(Shared<Vec<u8>>),
    Bool(bool),
    Nil,
    List(Shared<Vec<VmValue>>),
    Dict(Shared<BTreeMap<String, VmValue>>),
    Closure(Shared<VmClosure>),
    /// Reference to a registered builtin function, used when a builtin name is
    /// referenced as a value (e.g. `snake_dict.rekey(snake_to_camel)`). The
    /// contained string is the builtin's registered name.
    BuiltinRef(Shared<str>),
    /// Compact builtin reference for callback positions. Carries the name for
    /// policy, diagnostics, and fallback if the ID cannot be used.
    BuiltinRefId {
        id: BuiltinId,
        name: Shared<str>,
    },
    Duration(i64),
    EnumVariant(Shared<VmEnumVariant>),
    StructInstance {
        layout: Shared<StructLayout>,
        fields: Shared<Vec<Option<VmValue>>>,
    },
    TaskHandle(Shared<str>),
    Channel(Shared<VmChannelHandle>),
    Atomic(Shared<VmAtomicHandle>),
    Rng(Shared<VmRngHandle>),
    SyncPermit(Shared<VmSyncPermitHandle>),
    McpClient(Shared<VmMcpClientHandle>),
    Set(Shared<Vec<VmValue>>),
    Generator(Shared<VmGenerator>),
    Stream(Shared<VmStream>),
    Range(VmRange),
    /// Lazy iterator handle. Single-pass, fused. See `crate::vm::iter::VmIter`.
    Iter(crate::vm::iter::VmIterHandle),
    /// Two-element pair value. Produced by `pair(a, b)`, yielded by the
    /// Dict iterator source, and (later) by `zip` / `enumerate` combinators.
    /// Accessed via `.first` / `.second`, and destructurable in
    /// `for (a, b) in ...` loops.
    Pair(Shared<(VmValue, VmValue)>),
    /// Capability handle threaded into `main(harness: Harness)`. The same
    /// variant carries the root handle and each typed sub-handle (`stdio`,
    /// `clock`, `fs`, `env`, `random`, `net`) so they share one value shape
    /// but stay distinguishable via `VmHarness::kind`.
    Harness(Shared<VmHarness>),
}

impl VmValue {
    pub fn enum_variant(
        enum_name: impl Into<Shared<str>>,
        variant: impl Into<Shared<str>>,
        fields: Vec<VmValue>,
    ) -> Self {
        VmValue::EnumVariant(Shared::new(VmEnumVariant {
            enum_name: enum_name.into(),
            variant: variant.into(),
            fields: Shared::new(fields),
        }))
    }

    pub fn task_handle(id: impl Into<Shared<str>>) -> Self {
        VmValue::TaskHandle(id.into())
    }

    pub fn channel(handle: VmChannelHandle) -> Self {
        VmValue::Channel(Shared::new(handle))
    }

    pub fn atomic(handle: VmAtomicHandle) -> Self {
        VmValue::Atomic(Shared::new(handle))
    }

    pub fn rng(handle: VmRngHandle) -> Self {
        VmValue::Rng(Shared::new(handle))
    }

    pub fn sync_permit(handle: VmSyncPermitHandle) -> Self {
        VmValue::SyncPermit(Shared::new(handle))
    }

    pub fn mcp_client(handle: VmMcpClientHandle) -> Self {
        VmValue::McpClient(Shared::new(handle))
    }

    pub fn generator(generator: VmGenerator) -> Self {
        VmValue::Generator(Shared::new(generator))
    }

    pub fn stream(stream: VmStream) -> Self {
        VmValue::Stream(Shared::new(stream))
    }

    pub fn harness(handle: VmHarness) -> Self {
        VmValue::Harness(Shared::new(handle))
    }

    pub fn struct_instance(
        struct_name: impl Into<Shared<str>>,
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
            VmValue::Duration(ms) => *ms != 0,
            VmValue::EnumVariant(_) => true,
            VmValue::StructInstance { .. } => true,
            VmValue::TaskHandle(_) => true,
            VmValue::Channel(_) => true,
            VmValue::Atomic(_) => true,
            VmValue::Rng(_) => true,
            VmValue::SyncPermit(_) => true,
            VmValue::McpClient(_) => true,
            VmValue::Set(s) => !s.is_empty(),
            VmValue::Generator(_) => true,
            VmValue::Stream(_) => true,
            // Match Python semantics: range objects are always truthy,
            // even the empty range (analogous to generators / iterators).
            VmValue::Range(_) => true,
            VmValue::Iter(_) => true,
            VmValue::Pair(_) => true,
            VmValue::Harness(_) => true,
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
            VmValue::EnumVariant(_) => "enum",
            VmValue::StructInstance { .. } => "struct",
            VmValue::TaskHandle(_) => "task_handle",
            VmValue::Channel(_) => "channel",
            VmValue::Atomic(_) => "atomic",
            VmValue::Rng(_) => "rng",
            VmValue::SyncPermit(_) => "sync_permit",
            VmValue::McpClient(_) => "mcp_client",
            VmValue::Set(_) => "set",
            VmValue::Generator(_) => "generator",
            VmValue::Stream(_) => "stream",
            VmValue::Range(_) => "range",
            VmValue::Iter(_) => "iter",
            VmValue::Pair(_) => "pair",
            VmValue::Harness(h) => h.type_name(),
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
        let layout = Shared::new(StructLayout::from_map(struct_name, &fields));
        let slots = layout
            .field_names()
            .iter()
            .map(|name| fields.get(name).cloned())
            .collect();
        VmValue::StructInstance {
            layout,
            fields: Shared::new(slots),
        }
    }

    pub fn struct_instance_with_layout(
        struct_name: impl Into<String>,
        field_names: Vec<String>,
        field_values: BTreeMap<String, VmValue>,
    ) -> Self {
        let layout = Shared::new(StructLayout::new(struct_name, field_names));
        let fields = layout
            .field_names()
            .iter()
            .map(|name| field_values.get(name).cloned())
            .collect();
        VmValue::StructInstance {
            layout,
            fields: Shared::new(fields),
        }
    }

    pub fn struct_instance_with_property(&self, field_name: &str, value: VmValue) -> Option<Self> {
        let VmValue::StructInstance { layout, fields } = self else {
            return None;
        };

        let mut new_fields = fields.as_ref().clone();
        let layout = match layout.field_index(field_name) {
            Some(index) => {
                if index >= new_fields.len() {
                    new_fields.resize(index + 1, None);
                }
                new_fields[index] = Some(value);
                Shared::clone(layout)
            }
            None => {
                let new_layout = Shared::new(layout.with_appended_field(field_name.to_string()));
                new_fields.push(Some(value));
                new_layout
            }
        };

        Some(VmValue::StructInstance {
            layout,
            fields: Shared::new(new_fields),
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
                let names: Vec<&str> = c.func.param_names().collect();
                let _ = write!(out, "<fn({})>", names.join(", "));
            }
            VmValue::BuiltinRef(name) => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::BuiltinRefId { name, .. } => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::Duration(ms) => {
                let sign = if *ms < 0 { "-" } else { "" };
                let abs_ms = ms.unsigned_abs();
                if abs_ms >= 604_800_000 && abs_ms % 604_800_000 == 0 {
                    let _ = write!(out, "{}{}w", sign, abs_ms / 604_800_000);
                } else if abs_ms >= 86_400_000 && abs_ms % 86_400_000 == 0 {
                    let _ = write!(out, "{}{}d", sign, abs_ms / 86_400_000);
                } else if abs_ms >= 3_600_000 && abs_ms % 3_600_000 == 0 {
                    let _ = write!(out, "{}{}h", sign, abs_ms / 3_600_000);
                } else if abs_ms >= 60_000 && abs_ms % 60_000 == 0 {
                    let _ = write!(out, "{}{}m", sign, abs_ms / 60_000);
                } else if abs_ms >= 1000 && abs_ms % 1000 == 0 {
                    let _ = write!(out, "{}{}s", sign, abs_ms / 1000);
                } else {
                    let _ = write!(out, "{sign}{abs_ms}ms");
                }
            }
            VmValue::EnumVariant(enum_variant) => {
                if enum_variant.fields.is_empty() {
                    let _ = write!(out, "{}.{}", enum_variant.enum_name, enum_variant.variant);
                } else {
                    let _ = write!(out, "{}.{}(", enum_variant.enum_name, enum_variant.variant);
                    for (i, v) in enum_variant.fields.iter().enumerate() {
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
            VmValue::Rng(_) => {
                out.push_str("<rng>");
            }
            VmValue::SyncPermit(p) => {
                let _ = write!(out, "<sync_permit:{}:{}>", p.kind(), p.key());
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
                if g.is_done() {
                    out.push_str("<generator (done)>");
                } else {
                    out.push_str("<generator>");
                }
            }
            VmValue::Stream(s) => {
                if s.is_done() {
                    out.push_str("<stream (done)>");
                } else {
                    out.push_str("<stream>");
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
                if matches!(&*h.lock(), crate::vm::iter::VmIter::Exhausted) {
                    out.push_str("<iter (exhausted)>");
                } else {
                    out.push_str("<iter>");
                }
            }
            VmValue::Harness(h) => {
                let _ = write!(out, "<{}>", h.type_name());
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
pub type VmBuiltinFn =
    Arc<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + Send + Sync>;
