use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{future::Future, pin::Pin};

use crate::harness::VmHarness;
use crate::mcp::VmMcpClientHandle;
use crate::BuiltinId;

use super::{
    VmAtomicHandle, VmChannelHandle, VmClosure, VmError, VmGenerator, VmRange, VmRngHandle, VmSet,
    VmStream, VmSyncPermitHandle, VmVerdictReceipt,
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

/// Thin, reference-counted, immutable UTF-8 string used by every string-shaped
/// [`VmValue`] variant (`String`, `BuiltinRef`, `TaskHandle`).
///
/// Unlike `Arc<str>` — whose fat pointer (data ptr + length) is 16 bytes and
/// set the whole-enum size floor — [`arcstr::ArcStr`] is a single word: the
/// length lives in the heap allocation alongside the refcount and bytes. That
/// is what lets `VmValue` shrink to 16 bytes (paired with boxing the other
/// oversized payloads). Cloning is a refcount bump, identical to `Arc<str>`;
/// the unsafe pointer arithmetic is encapsulated and fuzzed inside the vetted
/// `arcstr` crate, so the VM carries no hand-rolled unsafe for this.
pub type HarnStr = arcstr::ArcStr;

/// Backing store for [`VmValue::Dict`]: a persistent, ordered, structurally
/// shared map.
///
/// Replacing the former `BTreeMap` with `imbl::OrdMap` turns the copy-on-write
/// `Arc::make_mut` clone — performed on every dict mutation whenever the value
/// is aliased (on the stack, in another local, captured by a closure) — from an
/// O(n) deep copy of every key and entry into an O(log n) path copy. Ordering
/// and the read API (`get` / `iter` / `keys` / `values` / `contains_key` /
/// `range` / `len`) match `BTreeMap`, so dict reads are unchanged. The `Arc`
/// wrapper is retained so reference identity (`Arc::ptr_eq`) — used by the `===`
/// operator and `value_identity_key` — keeps its current semantics.
pub type DictMap = imbl::OrdMap<HarnStr, VmValue>;

/// Intern a dict key into a shared [`HarnStr`].
///
/// Agent workloads are dict-heavy and the same field names (`role`, `content`,
/// `arguments`, …) recur across thousands of message/JSON dicts. Interning
/// short keys lets every occurrence share one allocation (a refcount bump on
/// reuse) instead of allocating a fresh string per key. The table is *bounded*
/// — only keys up to [`MAX_INTERNED_KEY_LEN`] bytes are eligible, and once
/// [`MAX_INTERNED_KEYS`] distinct keys are cached no new entries are added — so
/// adversarial or high-cardinality keys (UUIDs, user input) fall back to a
/// plain allocation and can never grow the table without bound.
pub fn intern_key(key: &str) -> HarnStr {
    const MAX_INTERNED_KEY_LEN: usize = 64;
    const MAX_INTERNED_KEYS: usize = 8192;
    static INTERNED_KEYS: std::sync::LazyLock<parking_lot::Mutex<HashMap<Box<str>, HarnStr>>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

    if key.len() > MAX_INTERNED_KEY_LEN {
        return HarnStr::from(key);
    }
    let mut table = INTERNED_KEYS.lock();
    if let Some(existing) = table.get(key) {
        return existing.clone();
    }
    let interned = HarnStr::from(key);
    if table.len() < MAX_INTERNED_KEYS {
        table.insert(Box::from(key), interned.clone());
    }
    interned
}

/// Conversion into an interned dict key.
///
/// Lets [`VmValue::dict`] accept the maps callers already build —
/// `BTreeMap<String, _>` and the persistent [`DictMap`] (`OrdMap<HarnStr, _>`) —
/// while routing freshly-owned string keys through [`intern_key`] and passing an
/// already-shared [`HarnStr`] (e.g. from re-wrapping an existing dict) straight
/// through without re-interning.
pub trait IntoDictKey {
    fn into_dict_key(self) -> HarnStr;
}

impl IntoDictKey for String {
    fn into_dict_key(self) -> HarnStr {
        intern_key(&self)
    }
}

impl IntoDictKey for &str {
    fn into_dict_key(self) -> HarnStr {
        intern_key(self)
    }
}

impl IntoDictKey for HarnStr {
    fn into_dict_key(self) -> HarnStr {
        self
    }
}

/// Character count with a byte-length fast path for ASCII text.
///
/// Harn exposes string lengths as Unicode scalar counts. ASCII is one byte per
/// scalar, so cached string `count` / `len` paths can avoid a full iterator
/// scan without changing behavior for non-ASCII text.
pub fn string_char_count(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    }
}

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

    pub fn from_map(struct_name: impl Into<String>, fields: &crate::value::DictMap) -> Self {
        Self::new(
            struct_name,
            fields.keys().map(|key| key.to_string()).collect(),
        )
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
    pub enum_name: HarnStr,
    pub variant: HarnStr,
    pub fields: Shared<Vec<VmValue>>,
}

impl VmEnumVariant {
    pub fn has_enum_name(&self, enum_name: &str) -> bool {
        self.enum_name.as_str() == enum_name
    }

    pub fn is_variant(&self, enum_name: &str, variant: &str) -> bool {
        self.has_enum_name(enum_name) && self.variant.as_str() == variant
    }
}

/// Boxed payload for [`VmValue::BuiltinRefId`].
///
/// Pairs the compact [`BuiltinId`] used for direct dispatch with the builtin's
/// registered name (kept for policy checks, diagnostics, and name-keyed
/// fallback). Stored behind a `Shared` pointer in the value so the `{ id, name
/// }` pair does not widen every `VmValue` to its 24-byte footprint.
#[derive(Debug, Clone)]
pub struct VmBuiltinRefId {
    pub id: BuiltinId,
    pub name: HarnStr,
}

/// Runtime layout + slots for a [`VmValue::StructInstance`].
///
/// Boxed behind a single `Shared` pointer so the `{ layout, fields }` pair —
/// two pointers, 16 bytes inline — does not set the whole-enum size. Cloning a
/// struct value is then a single refcount bump, and the variant fits in one
/// word like every other compound payload.
#[derive(Debug, Clone)]
pub struct StructInstanceData {
    pub layout: Shared<StructLayout>,
    pub fields: Shared<Vec<Option<VmValue>>>,
}

/// VM runtime value.
///
/// Rare compound payloads use shared pointers so stack/local-slot traffic is
/// bounded by the common scalar and pointer-sized value shapes. Every variant
/// is held to a single machine word (8 bytes): the oversized payloads —
/// `Range` (a 24-byte triple), `BuiltinRefId` (id + name), `Decimal` (16-byte
/// base-10 mantissa), and `StructInstance` (two pointers) — are boxed behind a
/// `Shared` pointer, and the string-shaped variants use the thin-pointer
/// [`HarnStr`] instead of a 16-byte `Arc<str>` fat pointer. That keeps
/// `VmValue` at 16 bytes (down from 24, and 32 before that) without inflating
/// the common `Int` / `Float` / `List` / `Dict` / `String` shapes the
/// interpreter moves on every push, pop, clone, and local-slot write. Unsafe
/// layouts such as NaN boxing or tagged pointers remain deferred; the thin
/// string's unsafe is encapsulated in the vetted `arcstr` crate.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    /// Exact base-10 decimal (96-bit mantissa, up to 28–29 significant digits)
    /// for money and other values where binary float rounding is unacceptable.
    /// Boxed behind a `Shared` pointer (`rust_decimal::Decimal` is 16 bytes, so
    /// inlining it would set the whole-enum size); cloning is a refcount bump.
    /// Constructed via the `decimal(value)` builtin; it is a distinct type from
    /// `Int`/`Float` for equality/ordering/hashing (a clean island) but
    /// promotes `Int` operands exactly in arithmetic. See `docs/src/decimal.md`.
    Decimal(Shared<rust_decimal::Decimal>),
    String(HarnStr),
    Bytes(Shared<Vec<u8>>),
    Bool(bool),
    Nil,
    List(Shared<Vec<VmValue>>),
    Dict(Shared<DictMap>),
    Closure(Shared<VmClosure>),
    /// Reference to a registered builtin function, used when a builtin name is
    /// referenced as a value (e.g. `snake_dict.rekey(snake_to_camel)`). The
    /// contained string is the builtin's registered name.
    BuiltinRef(HarnStr),
    /// Compact builtin reference for callback positions. The boxed
    /// [`VmBuiltinRefId`] carries the id plus the name for policy,
    /// diagnostics, and fallback if the ID cannot be used. Boxed so the
    /// `{ id, name }` pair does not widen every `VmValue`.
    BuiltinRefId(Shared<VmBuiltinRefId>),
    Duration(i64),
    EnumVariant(Shared<VmEnumVariant>),
    StructInstance(Shared<StructInstanceData>),
    TaskHandle(HarnStr),
    Channel(Shared<VmChannelHandle>),
    Atomic(Shared<VmAtomicHandle>),
    Rng(Shared<VmRngHandle>),
    SyncPermit(Shared<VmSyncPermitHandle>),
    McpClient(Shared<VmMcpClientHandle>),
    /// A host-minted proof-of-execution receipt — the payload of a positive
    /// `Verdict`. Constructed ONLY by the verdict issuance capability after the
    /// host validated a real evidence artifact; no `.harn` code can build it.
    VerdictReceipt(Shared<VmVerdictReceipt>),
    Set(Shared<VmSet>),
    Generator(Shared<VmGenerator>),
    Stream(Shared<VmStream>),
    /// Lazy numeric range. Boxed behind a `Shared` pointer so its 24-byte
    /// `start/end/inclusive` payload does not set the whole-enum size; cloning
    /// a range value is then a refcount bump.
    Range(Shared<VmRange>),
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

/// Process-wide interned `Arc<str>` for every single-byte ASCII character.
///
/// Materializing source text into per-character string values — the supported
/// idiom for cursor-style scanners (`chars`, `char_at`, `s[i]`) — would
/// otherwise heap-allocate once per character. Source files are overwhelmingly
/// ASCII, so interning the 128 single-char strings lets those paths clone a
/// cheap `Arc` (a refcount bump) instead of allocating, keeping a full-file
/// scan linear with a low constant factor.
static ASCII_CHAR_STRINGS: std::sync::LazyLock<[HarnStr; 128]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|byte| {
        let mut buffer = [0u8; 4];
        HarnStr::from((byte as u8 as char).encode_utf8(&mut buffer))
    })
});

impl VmValue {
    /// Canonical `VmValue::String` constructor from anything string-like.
    ///
    /// Collapses the ubiquitous `VmValue::String(arcstr::ArcStr::from(..))`
    /// spelling to a single call and performs exactly one allocation via
    /// `Arc::<str>::from(&str)` regardless of whether the input is a `&str`,
    /// `String`, `&String`, or `Cow<str>`. Prefer this over hand-writing the
    /// `Arc::from` at call sites.
    pub fn string(value: impl AsRef<str>) -> Self {
        VmValue::String(HarnStr::from(value.as_ref()))
    }

    /// Canonical `VmValue::Decimal` constructor.
    ///
    /// Boxes the 16-byte [`rust_decimal::Decimal`] behind a `Shared` pointer so
    /// the value stays one word wide; see [`VmValue::Decimal`].
    pub fn decimal(value: rust_decimal::Decimal) -> Self {
        VmValue::Decimal(Shared::new(value))
    }

    /// Builds a `VmValue::String` holding a single character, reusing the
    /// interned ASCII table (see [`ASCII_CHAR_STRINGS`]) so the common ASCII
    /// path does not allocate.
    pub fn char_value(ch: char) -> Self {
        if ch.is_ascii() {
            return VmValue::String(ASCII_CHAR_STRINGS[ch as usize].clone());
        }
        let mut buffer = [0u8; 4];
        VmValue::String(HarnStr::from(ch.encode_utf8(&mut buffer)))
    }

    /// Materializes a string into a `VmValue::List` of single-character string
    /// values in one linear pass. Backs both the `chars` builtin and the
    /// `.chars()` method, and is the cursor-scanner-friendly counterpart to the
    /// O(n)-per-call `substring` / slice / `s[i]` operations on a `string`.
    pub fn chars_list(text: &str) -> Self {
        VmValue::List(Shared::new(text.chars().map(VmValue::char_value).collect()))
    }

    pub fn enum_variant(
        enum_name: impl Into<HarnStr>,
        variant: impl Into<HarnStr>,
        fields: Vec<VmValue>,
    ) -> Self {
        VmValue::EnumVariant(Shared::new(VmEnumVariant {
            enum_name: enum_name.into(),
            variant: variant.into(),
            fields: Shared::new(fields),
        }))
    }

    pub fn task_handle(id: impl Into<HarnStr>) -> Self {
        VmValue::TaskHandle(id.into())
    }

    /// Construct a boxed [`VmValue::Range`] from a [`VmRange`].
    pub fn range(range: VmRange) -> Self {
        VmValue::Range(Shared::new(range))
    }

    /// Construct a boxed [`VmValue::BuiltinRefId`] from its id and name.
    pub fn builtin_ref_id(id: BuiltinId, name: impl Into<HarnStr>) -> Self {
        VmValue::BuiltinRefId(Shared::new(VmBuiltinRefId {
            id,
            name: name.into(),
        }))
    }

    /// Construct a [`VmValue::Dict`] from any iterator of `(key, value)`
    /// entries. Accepts the `BTreeMap` that most builders still assemble (it is
    /// `IntoIterator<Item = (String, VmValue)>`) and collects it into the
    /// persistent [`DictMap`], so callers keep their familiar map-building code
    /// while the stored value gains structural sharing.
    pub fn dict<K: IntoDictKey>(entries: impl IntoIterator<Item = (K, VmValue)>) -> Self {
        VmValue::Dict(Shared::new(
            entries
                .into_iter()
                .map(|(k, v)| (k.into_dict_key(), v))
                .collect::<DictMap>(),
        ))
    }

    /// Construct a [`VmValue::Dict`] from an already-built [`DictMap`].
    pub fn dict_map(map: DictMap) -> Self {
        VmValue::Dict(Shared::new(map))
    }

    /// Construct a [`VmValue::Set`] from any iterator of values, deduplicating
    /// by structural equality and preserving first-seen insertion order.
    pub fn set(values: impl IntoIterator<Item = VmValue>) -> Self {
        VmValue::Set(Shared::new(values.into_iter().collect::<VmSet>()))
    }

    /// Construct a [`VmValue::Set`] from an already-built [`VmSet`].
    pub fn set_value(set: VmSet) -> Self {
        VmValue::Set(Shared::new(set))
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

    /// Mint a verdict receipt value. Intentionally the ONLY constructor, and it
    /// is called only from the verdict issuance capability (`harness.verdict`)
    /// after host validation — never from a `.harn`-reachable builtin.
    pub fn verdict_receipt(receipt: VmVerdictReceipt) -> Self {
        VmValue::VerdictReceipt(Shared::new(receipt))
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
        fields: crate::value::DictMap,
    ) -> Self {
        Self::struct_instance_from_map(struct_name.into().to_string(), fields)
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            VmValue::Bool(b) => *b,
            VmValue::Nil => false,
            VmValue::Int(n) => *n != 0,
            VmValue::Float(n) => *n != 0.0,
            VmValue::Decimal(d) => **d != rust_decimal::Decimal::ZERO,
            VmValue::String(s) => !s.is_empty(),
            VmValue::Bytes(bytes) => !bytes.is_empty(),
            VmValue::List(l) => !l.is_empty(),
            VmValue::Dict(d) => !d.is_empty(),
            VmValue::Closure(_) => true,
            VmValue::BuiltinRef(_) => true,
            VmValue::BuiltinRefId(_) => true,
            VmValue::Duration(ms) => *ms != 0,
            VmValue::EnumVariant(_) => true,
            VmValue::StructInstance(_) => true,
            VmValue::TaskHandle(_) => true,
            VmValue::Channel(_) => true,
            VmValue::Atomic(_) => true,
            VmValue::Rng(_) => true,
            VmValue::SyncPermit(_) => true,
            VmValue::McpClient(_) => true,
            VmValue::VerdictReceipt(_) => true,
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

    /// Every tag [`VmValue::type_name`] can return, excluding harness-object
    /// names (delegated to `HarnessValue::type_name`). Keep in lockstep with
    /// the match below AND with `harn_builtin_meta::runtime_type_tags::ALL`
    /// — a unit test asserts the latter, which is what keeps the
    /// typechecker's `type_of` narrowing honest.
    pub const ALL_TYPE_NAMES: &'static [&'static str] = &[
        "string",
        "bytes",
        "int",
        "float",
        "decimal",
        "bool",
        "nil",
        "list",
        "dict",
        "closure",
        "builtin",
        "duration",
        "enum",
        "struct",
        "task_handle",
        "channel",
        "atomic",
        "rng",
        "sync_permit",
        "mcp_client",
        "verdict_receipt",
        "set",
        "generator",
        "stream",
        "range",
        "iter",
        "pair",
    ];

    pub fn type_name(&self) -> &'static str {
        match self {
            VmValue::String(_) => "string",
            VmValue::Bytes(_) => "bytes",
            VmValue::Int(_) => "int",
            VmValue::Float(_) => "float",
            VmValue::Decimal(_) => "decimal",
            VmValue::Bool(_) => "bool",
            VmValue::Nil => "nil",
            VmValue::List(_) => "list",
            VmValue::Dict(_) => "dict",
            VmValue::Closure(_) => "closure",
            VmValue::BuiltinRef(_) => "builtin",
            VmValue::BuiltinRefId(_) => "builtin",
            VmValue::Duration(_) => "duration",
            VmValue::EnumVariant(_) => "enum",
            VmValue::StructInstance(_) => "struct",
            VmValue::TaskHandle(_) => "task_handle",
            VmValue::Channel(_) => "channel",
            VmValue::Atomic(_) => "atomic",
            VmValue::Rng(_) => "rng",
            VmValue::SyncPermit(_) => "sync_permit",
            VmValue::McpClient(_) => "mcp_client",
            VmValue::VerdictReceipt(_) => "verdict_receipt",
            VmValue::Set(_) => "set",
            VmValue::Generator(_) => "generator",
            VmValue::Stream(_) => "stream",
            VmValue::Range(_) => "range",
            VmValue::Iter(_) => "iter",
            VmValue::Pair(_) => "pair",
            VmValue::Harness(h) => h.type_name(),
        }
    }

    /// Borrows the string contents without allocating when the value is
    /// already a string. Non-string values are rendered with `display()`,
    /// matching the coercion callers apply at string boundaries. Hot string
    /// builtins (regex, split, contains) use this to avoid cloning the
    /// subject text on every call.
    pub fn as_str_cow(&self) -> std::borrow::Cow<'_, str> {
        match self {
            VmValue::String(s) => std::borrow::Cow::Borrowed(s.as_str()),
            other => std::borrow::Cow::Owned(other.display()),
        }
    }

    /// Borrows the boxed struct payload (layout + field slots) when this value
    /// is a struct instance. The single accessor most match sites use instead
    /// of destructuring the now-boxed variant.
    pub fn struct_data(&self) -> Option<&StructInstanceData> {
        match self {
            VmValue::StructInstance(data) => Some(data),
            _ => None,
        }
    }

    pub fn struct_name(&self) -> Option<&str> {
        match self {
            VmValue::StructInstance(data) => Some(data.layout.struct_name()),
            _ => None,
        }
    }

    pub fn struct_field(&self, field_name: &str) -> Option<&VmValue> {
        match self {
            VmValue::StructInstance(data) => data
                .layout
                .field_index(field_name)
                .and_then(|index| data.fields.get(index))
                .and_then(Option::as_ref),
            _ => None,
        }
    }

    pub fn struct_fields_map(&self) -> Option<crate::value::DictMap> {
        match self {
            VmValue::StructInstance(data) => Some(struct_fields_to_map(&data.layout, &data.fields)),
            _ => None,
        }
    }

    pub fn struct_instance_from_map(
        struct_name: impl Into<String>,
        fields: crate::value::DictMap,
    ) -> Self {
        let layout = Shared::new(StructLayout::from_map(struct_name, &fields));
        let slots = layout
            .field_names()
            .iter()
            .map(|name| fields.get(name.as_str()).cloned())
            .collect();
        VmValue::StructInstance(Shared::new(StructInstanceData {
            layout,
            fields: Shared::new(slots),
        }))
    }

    pub fn struct_instance_with_layout(
        struct_name: impl Into<String>,
        field_names: Vec<String>,
        field_values: crate::value::DictMap,
    ) -> Self {
        let layout = Shared::new(StructLayout::new(struct_name, field_names));
        let fields = layout
            .field_names()
            .iter()
            .map(|name| field_values.get(name.as_str()).cloned())
            .collect();
        VmValue::StructInstance(Shared::new(StructInstanceData {
            layout,
            fields: Shared::new(fields),
        }))
    }

    pub fn struct_instance_with_property(&self, field_name: &str, value: VmValue) -> Option<Self> {
        let VmValue::StructInstance(data) = self else {
            return None;
        };
        let (layout, fields) = (&data.layout, &data.fields);

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

        Some(VmValue::StructInstance(Shared::new(StructInstanceData {
            layout,
            fields: Shared::new(new_fields),
        })))
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
            // Render the decimal at its stored scale (e.g. `1.50` stays `1.50`),
            // which is what money formatting expects. Equality normalizes scale,
            // so `1.5` and `1.50` are still equal even though they display
            // differently.
            VmValue::Decimal(d) => {
                let _ = write!(out, "{d}");
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
                crate::value::recursion::guard_recursion(|| {
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        item.write_display(out);
                    }
                });
                out.push(']');
            }
            VmValue::Dict(map) => {
                out.push('{');
                crate::value::recursion::guard_recursion(|| {
                    for (i, (k, v)) in map.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(k);
                        out.push_str(": ");
                        v.write_display(out);
                    }
                });
                out.push('}');
            }
            VmValue::Closure(c) => {
                let names: Vec<&str> = c.func.param_names().collect();
                let _ = write!(out, "<fn({})>", names.join(", "));
            }
            VmValue::BuiltinRef(name) => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::BuiltinRefId(r) => {
                let _ = write!(out, "<builtin {}>", r.name);
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
                    crate::value::recursion::guard_recursion(|| {
                        for (i, v) in enum_variant.fields.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            v.write_display(out);
                        }
                    });
                    out.push(')');
                }
            }
            VmValue::StructInstance(data) => {
                let (layout, fields) = (&data.layout, &data.fields);
                let _ = write!(out, "{} {{", layout.struct_name());
                crate::value::recursion::guard_recursion(|| {
                    for (i, (k, v)) in struct_fields_to_map(layout, fields).iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(k);
                        out.push_str(": ");
                        v.write_display(out);
                    }
                });
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
            // Authority-free: the display MUST NOT leak the receipt payload
            // (hash/run identity), because display feeds the lenient JSON and
            // structural-hash fallbacks. It is an opaque marker only.
            VmValue::VerdictReceipt(_) => {
                out.push_str("<verdict_receipt>");
            }
            VmValue::Set(items) => {
                out.push_str("set(");
                crate::value::recursion::guard_recursion(|| {
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        item.write_display(out);
                    }
                });
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
                crate::value::recursion::guard_recursion(|| {
                    p.0.write_display(out);
                    out.push_str(", ");
                    p.1.write_display(out);
                });
                out.push(')');
            }
        }
    }

    /// Get the value as a [`DictMap`] reference, if it's a Dict.
    pub fn as_dict(&self) -> Option<&DictMap> {
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
) -> crate::value::DictMap {
    layout
        .field_names()
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            fields
                .get(index)
                .and_then(Option::as_ref)
                .map(|value| (intern_key(name), value.clone()))
        })
        .collect()
}

/// Sync builtin function for the VM.
pub type VmBuiltinFn =
    Arc<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + Send + Sync>;

#[cfg(test)]
mod runtime_type_tag_tests {
    use super::VmValue;

    /// The canonical tag registry in `harn-builtin-meta` is what the
    /// typechecker's `type_of` narrowing trusts; this assertion is the link
    /// that keeps it in lockstep with what the runtime actually produces.
    #[test]
    fn type_name_tags_match_canonical_registry() {
        let canonical = harn_builtin_meta::runtime_type_tags::ALL;
        for tag in VmValue::ALL_TYPE_NAMES {
            assert!(
                canonical.contains(tag),
                "VmValue::type_name tag `{tag}` missing from harn_builtin_meta::runtime_type_tags::ALL"
            );
        }
        for tag in canonical {
            assert!(
                VmValue::ALL_TYPE_NAMES.contains(tag),
                "canonical tag `{tag}` is not produced by VmValue::type_name; remove it or update ALL_TYPE_NAMES"
            );
        }
    }
}
