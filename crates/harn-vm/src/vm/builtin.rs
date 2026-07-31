use std::borrow::Cow;

use harn_builtin_meta::{BuiltinContract, BuiltinExposure};

/// Runtime kind for a registered VM builtin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmBuiltinKind {
    Sync,
    Async,
}

/// Lightweight arity metadata for registered builtins.
///
/// This is descriptive metadata only. Runtime behavior still flows through
/// the existing parser/typechecker and builtin handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmBuiltinArity {
    Exact(usize),
    Min(usize),
    Range { min: usize, max: usize },
    Variadic,
}

/// Discoverable metadata for a VM builtin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmBuiltinMetadata {
    name: Cow<'static, str>,
    kind: VmBuiltinKind,
    signature: Option<Cow<'static, str>>,
    arity: Option<VmBuiltinArity>,
    category: Option<Cow<'static, str>>,
    doc: Option<Cow<'static, str>>,
    contract: BuiltinContract,
}

impl VmBuiltinMetadata {
    pub fn sync(name: impl Into<Cow<'static, str>>) -> Self {
        Self::new(name, VmBuiltinKind::Sync)
    }

    pub fn async_builtin(name: impl Into<Cow<'static, str>>) -> Self {
        Self::new(name, VmBuiltinKind::Async)
    }

    pub const fn sync_static(name: &'static str) -> Self {
        Self::new_static(name, VmBuiltinKind::Sync)
    }

    pub const fn async_static(name: &'static str) -> Self {
        Self::new_static(name, VmBuiltinKind::Async)
    }

    fn new(name: impl Into<Cow<'static, str>>, kind: VmBuiltinKind) -> Self {
        Self {
            name: name.into(),
            kind,
            signature: None,
            arity: None,
            category: None,
            doc: None,
            contract: BuiltinContract::RUNTIME_INTERNAL,
        }
    }

    const fn new_static(name: &'static str, kind: VmBuiltinKind) -> Self {
        Self {
            name: Cow::Borrowed(name),
            kind,
            signature: None,
            arity: None,
            category: None,
            doc: None,
            contract: BuiltinContract::RUNTIME_INTERNAL,
        }
    }

    pub(crate) fn with_kind(mut self, kind: VmBuiltinKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn signature_static(mut self, signature: &'static str) -> Self {
        self.signature = Some(Cow::Borrowed(signature));
        self
    }

    pub fn signature_owned(mut self, signature: impl Into<Cow<'static, str>>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    pub fn arity(mut self, arity: VmBuiltinArity) -> Self {
        self.arity = Some(arity);
        self
    }

    pub fn category_static(mut self, category: &'static str) -> Self {
        self.category = Some(Cow::Borrowed(category));
        self
    }

    pub fn category_owned(mut self, category: impl Into<Cow<'static, str>>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn doc_static(mut self, doc: &'static str) -> Self {
        self.doc = Some(Cow::Borrowed(doc));
        self
    }

    pub fn doc_owned(mut self, doc: impl Into<Cow<'static, str>>) -> Self {
        self.doc = Some(doc.into());
        self
    }

    pub fn with_contract(mut self, contract: BuiltinContract) -> Self {
        assert!(
            contract.is_declared(),
            "dynamic builtin `{}` must supply a declared contract",
            self.name
        );
        self.contract = contract;
        self
    }

    pub fn name(&self) -> &str {
        self.name.as_ref()
    }

    pub fn kind(&self) -> VmBuiltinKind {
        self.kind
    }

    pub fn signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    pub fn arity_metadata(&self) -> Option<VmBuiltinArity> {
        self.arity
    }

    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    pub fn doc(&self) -> Option<&str> {
        self.doc.as_deref()
    }

    pub const fn contract(&self) -> BuiltinContract {
        self.contract
    }

    pub const fn is_source_visible(&self) -> bool {
        !matches!(
            self.contract.exposure,
            BuiltinExposure::RuntimeInternal | BuiltinExposure::Undeclared
        )
    }
}
