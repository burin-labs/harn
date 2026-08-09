//! Capability handle threaded into every Harn script as the `harness`
//! parameter of `main`.
//!
//! `Harness` is the Harn-language analog of an explicit-capability handle: a
//! single value the runtime hands to a script's `main` so that stdio,
//! terminal, clock, filesystem, environment, randomness, network, process,
//! channels, system, secrets, and LLM catalog access become surface in the type system
//! instead of ambient globals. Each sub-handle (`stdio`, `term`, `clock`, `fs`,
//! `env`, `random`, `net`, `process`, `channels`, `system`, `secrets`, `llm`,
//! `tenant`, `auth`, `obs`) is a distinct named type that anchors the surface
//! for its capability slice.
//!
//! This module defines:
//!   * The runtime [`Harness`] value and its sub-handle wrappers.
//!   * [`Harness::real`], the production constructor that installs the backing
//!     state used by concrete sub-handle methods.
//!   * [`VmHarness`], the compact `VmValue` payload that carries the same
//!     state through the bytecode VM and distinguishes the root handle from
//!     its sub-handles via [`HarnessKind`].

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harn_clock::{Clock, PausedClock, RealClock};
use time::OffsetDateTime;

/// Runtime discriminator for the root grant or one typed capability handle.
///
/// [`harn_builtin_meta::CapabilityId`] owns the closed capability vocabulary;
/// this wrapper adds the root state without duplicating its field/type maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HarnessKind(Option<harn_builtin_meta::CapabilityId>);

#[allow(non_upper_case_globals)]
impl HarnessKind {
    pub const Root: Self = Self(None);
    pub const Stdio: Self = Self(Some(harn_builtin_meta::CapabilityId::Stdio));
    pub const Term: Self = Self(Some(harn_builtin_meta::CapabilityId::Term));
    pub const Clock: Self = Self(Some(harn_builtin_meta::CapabilityId::Clock));
    pub const Fs: Self = Self(Some(harn_builtin_meta::CapabilityId::Fs));
    pub const Env: Self = Self(Some(harn_builtin_meta::CapabilityId::Env));
    pub const Random: Self = Self(Some(harn_builtin_meta::CapabilityId::Random));
    pub const Net: Self = Self(Some(harn_builtin_meta::CapabilityId::Net));
    pub const Process: Self = Self(Some(harn_builtin_meta::CapabilityId::Process));
    pub const Channels: Self = Self(Some(harn_builtin_meta::CapabilityId::Channels));
    pub const System: Self = Self(Some(harn_builtin_meta::CapabilityId::System));
    pub const Secrets: Self = Self(Some(harn_builtin_meta::CapabilityId::Secrets));
    pub const Llm: Self = Self(Some(harn_builtin_meta::CapabilityId::Llm));
    pub const Agent: Self = Self(Some(harn_builtin_meta::CapabilityId::Agent));
    pub const Tenant: Self = Self(Some(harn_builtin_meta::CapabilityId::Tenant));
    pub const Auth: Self = Self(Some(harn_builtin_meta::CapabilityId::Auth));
    pub const Obs: Self = Self(Some(harn_builtin_meta::CapabilityId::Observability));
    pub const Verdict: Self = Self(Some(harn_builtin_meta::CapabilityId::Verdict));
    pub const Tools: Self = Self(Some(harn_builtin_meta::CapabilityId::Tools));
    pub const Ast: Self = Self(Some(harn_builtin_meta::CapabilityId::Ast));
    pub const CodeIndex: Self = Self(Some(harn_builtin_meta::CapabilityId::CodeIndex));
    pub const Computer: Self = Self(Some(harn_builtin_meta::CapabilityId::Computer));
    pub const Embed: Self = Self(Some(harn_builtin_meta::CapabilityId::Embed));
    pub const Memory: Self = Self(Some(harn_builtin_meta::CapabilityId::Memory));
    pub const Sqlite: Self = Self(Some(harn_builtin_meta::CapabilityId::Sqlite));
    pub const Postgres: Self = Self(Some(harn_builtin_meta::CapabilityId::Postgres));
    pub const FsWatch: Self = Self(Some(harn_builtin_meta::CapabilityId::FsWatch));
    pub const HostLease: Self = Self(Some(harn_builtin_meta::CapabilityId::HostLease));
    pub const Scanner: Self = Self(Some(harn_builtin_meta::CapabilityId::Scanner));
    pub const SecretStore: Self = Self(Some(harn_builtin_meta::CapabilityId::SecretStore));
    pub const TerminalSession: Self = Self(Some(harn_builtin_meta::CapabilityId::TerminalSession));
    pub const Rules: Self = Self(Some(harn_builtin_meta::CapabilityId::Rules));
    pub const Lint: Self = Self(Some(harn_builtin_meta::CapabilityId::Lint));
    pub const Runtime: Self = Self(Some(harn_builtin_meta::CapabilityId::Runtime));
    pub const Interaction: Self = Self(Some(harn_builtin_meta::CapabilityId::Interaction));
    pub const Project: Self = Self(Some(harn_builtin_meta::CapabilityId::Project));
    pub const Dashboard: Self = Self(Some(harn_builtin_meta::CapabilityId::Dashboard));
    pub const Workspace: Self = Self(Some(harn_builtin_meta::CapabilityId::Workspace));
    pub const MergeCaptain: Self = Self(Some(harn_builtin_meta::CapabilityId::MergeCaptain));
    pub const Session: Self = Self(Some(harn_builtin_meta::CapabilityId::Session));
    pub const Permission: Self = Self(Some(harn_builtin_meta::CapabilityId::Permission));
    pub const Text: Self = Self(Some(harn_builtin_meta::CapabilityId::Text));
    pub const Lsp: Self = Self(Some(harn_builtin_meta::CapabilityId::Lsp));
    pub const Credentials: Self = Self(Some(harn_builtin_meta::CapabilityId::Credentials));
    pub const PrMonitor: Self = Self(Some(harn_builtin_meta::CapabilityId::PrMonitor));
    pub const Workflow: Self = Self(Some(harn_builtin_meta::CapabilityId::Workflow));
    pub const Testing: Self = Self(Some(harn_builtin_meta::CapabilityId::Testing));

    pub const fn capability_id(self) -> Option<harn_builtin_meta::CapabilityId> {
        self.0
    }

    /// The Harn-language type name for this kind (`Harness`, `HarnessStdio`,
    /// etc.). Used by the typechecker primitive registration and by
    /// `VmValue::type_name`.
    pub const fn type_name(self) -> &'static str {
        match self.0 {
            None => "Harness",
            Some(capability) => capability.type_name(),
        }
    }

    /// Field name a parent `Harness` exposes for this sub-handle (e.g. the
    /// `stdio` in `harness.stdio`). Returns `None` for the root.
    pub const fn field_name(self) -> Option<&'static str> {
        match self.0 {
            None => None,
            Some(capability) => Some(capability.field_name()),
        }
    }

    /// Parse the field name a script uses to reach a sub-handle.
    pub fn from_field_name(name: &str) -> Option<Self> {
        harn_builtin_meta::CapabilityId::from_field_name(name)
            .map(|capability| Self(Some(capability)))
    }

    /// All sub-handle kinds, in the canonical field order.
    pub const SUB_HANDLES: &'static [HarnessKind] = &[
        HarnessKind::Stdio,
        HarnessKind::Term,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
        HarnessKind::Process,
        HarnessKind::Channels,
        HarnessKind::System,
        HarnessKind::Secrets,
        HarnessKind::Llm,
        HarnessKind::Agent,
        HarnessKind::Tenant,
        HarnessKind::Auth,
        HarnessKind::Obs,
        HarnessKind::Verdict,
        HarnessKind::Tools,
        HarnessKind::Ast,
        HarnessKind::CodeIndex,
        HarnessKind::Computer,
        HarnessKind::Embed,
        HarnessKind::Memory,
        HarnessKind::Sqlite,
        HarnessKind::Postgres,
        HarnessKind::FsWatch,
        HarnessKind::HostLease,
        HarnessKind::Scanner,
        HarnessKind::SecretStore,
        HarnessKind::TerminalSession,
        HarnessKind::Rules,
        HarnessKind::Lint,
        HarnessKind::Runtime,
        HarnessKind::Interaction,
        HarnessKind::Project,
        HarnessKind::Dashboard,
        HarnessKind::Workspace,
        HarnessKind::MergeCaptain,
        HarnessKind::Session,
        HarnessKind::Permission,
        HarnessKind::Text,
        HarnessKind::Lsp,
        HarnessKind::Credentials,
        HarnessKind::PrMonitor,
        HarnessKind::Workflow,
        HarnessKind::Testing,
    ];

    /// Every kind a Harn-script type annotation may reference.
    pub const ALL: &'static [HarnessKind] = &[
        HarnessKind::Root,
        HarnessKind::Stdio,
        HarnessKind::Term,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
        HarnessKind::Process,
        HarnessKind::Channels,
        HarnessKind::System,
        HarnessKind::Secrets,
        HarnessKind::Llm,
        HarnessKind::Agent,
        HarnessKind::Tenant,
        HarnessKind::Auth,
        HarnessKind::Obs,
        HarnessKind::Verdict,
        HarnessKind::Tools,
        HarnessKind::Ast,
        HarnessKind::CodeIndex,
        HarnessKind::Computer,
        HarnessKind::Embed,
        HarnessKind::Memory,
        HarnessKind::Sqlite,
        HarnessKind::Postgres,
        HarnessKind::FsWatch,
        HarnessKind::HostLease,
        HarnessKind::Scanner,
        HarnessKind::SecretStore,
        HarnessKind::TerminalSession,
        HarnessKind::Rules,
        HarnessKind::Lint,
        HarnessKind::Runtime,
        HarnessKind::Interaction,
        HarnessKind::Project,
        HarnessKind::Dashboard,
        HarnessKind::Workspace,
        HarnessKind::MergeCaptain,
        HarnessKind::Session,
        HarnessKind::Permission,
        HarnessKind::Text,
        HarnessKind::Lsp,
        HarnessKind::Credentials,
        HarnessKind::PrMonitor,
        HarnessKind::Workflow,
        HarnessKind::Testing,
    ];
}

/// Per-harness clock router used by explicit test controls.
///
/// The override belongs to one `HarnessInner`; tests can pin and advance time
/// without installing thread-local state that unrelated VMs could observe.
#[derive(Debug)]
struct HarnessClockRouter {
    base: Arc<dyn Clock>,
    override_clock: Mutex<Option<Arc<PausedClock>>>,
}

impl HarnessClockRouter {
    fn new(base: Arc<dyn Clock>) -> Self {
        Self {
            base,
            override_clock: Mutex::new(None),
        }
    }

    fn active(&self) -> Arc<dyn Clock> {
        self.override_clock
            .lock()
            .expect("harness clock override poisoned")
            .as_ref()
            .map(|clock| Arc::clone(clock) as Arc<dyn Clock>)
            .unwrap_or_else(|| Arc::clone(&self.base))
    }

    fn set_unix_ms(&self, unix_ms: i64) -> Result<(), crate::VmError> {
        let nanos = i128::from(unix_ms).checked_mul(1_000_000).ok_or_else(|| {
            crate::VmError::TypeError("HarnessTesting.clock_set timestamp overflow".to_string())
        })?;
        let wall = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|error| {
            crate::VmError::TypeError(format!(
                "HarnessTesting.clock_set timestamp is out of range: {error}"
            ))
        })?;
        *self
            .override_clock
            .lock()
            .expect("harness clock override poisoned") = Some(PausedClock::new(wall));
        Ok(())
    }

    fn advance_ms(&self, milliseconds: i64) -> Result<i64, crate::VmError> {
        let milliseconds = u64::try_from(milliseconds).map_err(|_| {
            crate::VmError::TypeError(
                "HarnessTesting.clock_advance expects non-negative milliseconds".to_string(),
            )
        })?;
        let clock = self
            .override_clock
            .lock()
            .expect("harness clock override poisoned")
            .clone()
            .ok_or_else(|| {
                crate::VmError::Runtime(
                    "HarnessTesting.clock_advance requires clock_set first".to_string(),
                )
            })?;
        clock.advance(Duration::from_millis(milliseconds));
        Ok(harn_clock::now_wall_ms(clock.as_ref()))
    }

    fn clear_override(&self) {
        *self
            .override_clock
            .lock()
            .expect("harness clock override poisoned") = None;
    }

    async fn wait_for_advance(&self, duration: Duration) {
        self.active().sleep(duration).await;
    }
}

#[async_trait]
impl Clock for HarnessClockRouter {
    fn now_utc(&self) -> OffsetDateTime {
        self.active().now_utc()
    }

    fn monotonic_ms(&self) -> i64 {
        self.active().monotonic_ms()
    }

    async fn sleep(&self, duration: Duration) {
        let clock = self.active();
        if self
            .override_clock
            .lock()
            .expect("harness clock override poisoned")
            .is_some()
        {
            if let Some(paused) = self
                .override_clock
                .lock()
                .expect("harness clock override poisoned")
                .clone()
            {
                paused.advance(duration);
                return;
            }
        }
        clock.sleep(duration).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        let clock = self.active();
        if let Some(paused) = self
            .override_clock
            .lock()
            .expect("harness clock override poisoned")
            .clone()
        {
            let now = paused.now_utc();
            if deadline > now {
                paused.advance_time(deadline - now);
            }
            return;
        }
        clock.sleep_until_utc(deadline).await;
    }
}

/// Shared, refcounted state backing every sub-handle of a single `Harness`.
///
/// Method implementations (in `crate::vm::methods::harness`) borrow this to
/// reach the concrete OS-backed primitives. Wrapped in `Arc` so handles are
/// `Send + Sync` for VM contexts that move work onto other tasks.
pub struct HarnessInner {
    clock: Arc<dyn Clock>,
    clock_control: Arc<HarnessClockRouter>,
    mode: HarnessMode,
    /// Per-harness `harness.net.*` access policy. `None` means the
    /// handle inherits the legacy unrestricted behaviour (subject to
    /// the process-wide `crate::egress` allowlist, if configured).
    /// See `Harness::with_net_policy` and `crate::harness_net`.
    net_policy: Option<crate::harness_net::NetPolicy>,
    /// Optional provider backing `harness.secrets.*`. Runtime embedders install
    /// the managed provider that owns custody, audit, leases, and rotation.
    secret_provider: Option<Arc<dyn crate::secrets::SecretProvider>>,
    /// `true` once a request denied under `OnViolation::Quarantine`
    /// has fired. Sticky for the lifetime of the underlying
    /// `Arc<HarnessInner>` so downstream consumers can pin on the
    /// signal even after the originating call has returned. The flag
    /// is per-`Arc` (i.e. per-`Harness` build) so unrelated harnesses
    /// stay independent.
    quarantined: Mutex<bool>,
    fixtures: Arc<CapabilityFixtureState>,
}

impl fmt::Debug for HarnessInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessInner")
            .field("clock", &"<dyn Clock>")
            .field("mode", &self.mode)
            .field("net_policy", &self.net_policy)
            .field(
                "secret_provider",
                &self
                    .secret_provider
                    .as_ref()
                    .map(|provider| provider.namespace().to_string()),
            )
            .field("quarantined", &self.is_quarantined())
            .field("fixtures", &self.fixtures)
            .finish()
    }
}

impl HarnessInner {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub(crate) fn set_test_clock(&self, unix_ms: i64) -> Result<(), crate::VmError> {
        self.clock_control.set_unix_ms(unix_ms)
    }

    pub(crate) fn advance_test_clock(&self, milliseconds: i64) -> Result<i64, crate::VmError> {
        self.clock_control.advance_ms(milliseconds)
    }

    pub(crate) fn clear_test_clock(&self) {
        self.clock_control.clear_override();
    }

    pub(crate) async fn wait_for_clock_advance(&self, duration: Duration) {
        self.clock_control.wait_for_advance(duration).await;
    }

    pub(crate) fn mode(&self) -> &HarnessMode {
        &self.mode
    }

    pub fn net_policy(&self) -> Option<&crate::harness_net::NetPolicy> {
        self.net_policy.as_ref()
    }

    pub fn secret_provider(&self) -> Option<&Arc<dyn crate::secrets::SecretProvider>> {
        self.secret_provider.as_ref()
    }

    pub(crate) fn mark_quarantined(&self) {
        if let Ok(mut guard) = self.quarantined.lock() {
            *guard = true;
        }
    }

    pub fn is_quarantined(&self) -> bool {
        self.quarantined.lock().map(|guard| *guard).unwrap_or(false)
    }

    pub(crate) fn fixtures(&self) -> &CapabilityFixtureState {
        &self.fixtures
    }

    pub(crate) fn fixtures_arc(&self) -> Arc<CapabilityFixtureState> {
        Arc::clone(&self.fixtures)
    }
}

#[derive(Debug)]
pub(crate) enum HarnessMode {
    Real,
    Null(NullHarnessState),
    Mock(Arc<MockHarnessState>),
}

#[derive(Debug, Default)]
pub(crate) struct NullHarnessState {
    deny_events: Mutex<Vec<DenyEvent>>,
}

impl NullHarnessState {
    pub(crate) fn record_deny(
        &self,
        sub_handle: HarnessKind,
        method: &str,
        args: &[crate::VmValue],
    ) {
        self.deny_events
            .lock()
            .expect("deny events poisoned")
            .push(DenyEvent::new(
                sub_handle,
                method,
                args.iter().map(crate::VmValue::display).collect(),
            ));
    }

    pub(crate) fn deny_events(&self) -> Vec<DenyEvent> {
        self.deny_events
            .lock()
            .expect("deny events poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyEvent {
    pub sub_handle: HarnessKind,
    pub method: String,
    pub args: Vec<String>,
}

impl DenyEvent {
    fn new(sub_handle: HarnessKind, method: &str, args: Vec<String>) -> Self {
        Self {
            sub_handle,
            method: method.to_string(),
            args,
        }
    }
}

#[derive(Debug)]
pub(crate) struct MockHarnessState {
    calls: Mutex<Vec<HarnessCall>>,
    clock: Arc<PausedClock>,
    env: BTreeMap<String, String>,
    fs_reads: BTreeMap<String, Vec<u8>>,
    net_gets: BTreeMap<String, String>,
    random_u64: Mutex<VecDeque<u64>>,
    capability_responses:
        Mutex<BTreeMap<(harn_builtin_meta::CapabilityId, String), VecDeque<crate::VmValue>>>,
    stdin_lines: Mutex<VecDeque<String>>,
    stdio: Mutex<String>,
    stderr: Mutex<String>,
}

impl MockHarnessState {
    pub(crate) fn record_call(
        &self,
        sub_handle: HarnessKind,
        method: &str,
        args: &[crate::VmValue],
    ) {
        self.calls
            .lock()
            .expect("calls poisoned")
            .push(HarnessCall::new(
                sub_handle,
                method,
                args.iter().map(crate::VmValue::display).collect(),
            ));
    }

    pub(crate) fn calls(&self) -> Vec<HarnessCall> {
        self.calls.lock().expect("calls poisoned").clone()
    }

    pub(crate) fn env_get(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    pub(crate) fn fs_read(&self, path: &str) -> Option<&[u8]> {
        self.fs_reads.get(path).map(Vec::as_slice)
    }

    pub(crate) fn net_get(&self, url: &str) -> Option<&str> {
        self.net_gets.get(url).map(String::as_str)
    }

    pub(crate) fn next_random_u64(&self) -> Option<u64> {
        let mut values = self.random_u64.lock().expect("random values poisoned");
        values.pop_front()
    }

    pub(crate) fn capability_response(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> Option<crate::VmValue> {
        self.capability_responses
            .lock()
            .expect("capability responses poisoned")
            .get_mut(&(capability, method.to_string()))
            .and_then(VecDeque::pop_front)
    }

    /// Whether a canned response is still queued for this capability method.
    ///
    /// `capability_response` consumes one response per call, so dispatch needs
    /// this to decide whether the mock owns a method without spending the
    /// answer it is deciding about.
    pub(crate) fn has_capability_response(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> bool {
        self.capability_responses
            .lock()
            .expect("capability responses poisoned")
            .get(&(capability, method.to_string()))
            .is_some_and(|queued| !queued.is_empty())
    }

    pub(crate) fn advance_clock(&self, duration: std::time::Duration) {
        self.clock.advance(duration);
    }

    pub(crate) fn push_stdio(&self, text: &str) {
        self.stdio
            .lock()
            .expect("stdio buffer poisoned")
            .push_str(text);
    }

    pub(crate) fn stdio(&self) -> String {
        self.stdio.lock().expect("stdio buffer poisoned").clone()
    }

    pub(crate) fn push_stderr(&self, text: &str) {
        self.stderr
            .lock()
            .expect("stderr buffer poisoned")
            .push_str(text);
    }

    pub(crate) fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr buffer poisoned").clone()
    }

    pub(crate) fn pop_stdin_line(&self) -> Option<String> {
        self.stdin_lines
            .lock()
            .expect("stdin queue poisoned")
            .pop_front()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCall {
    pub sub_handle: HarnessKind,
    pub method: String,
    pub args: Vec<String>,
}

impl HarnessCall {
    fn new(sub_handle: HarnessKind, method: &str, args: Vec<String>) -> Self {
        Self {
            sub_handle,
            method: method.to_string(),
            args,
        }
    }
}

#[derive(Debug)]
pub struct MockHarnessBuilder {
    clock: Arc<PausedClock>,
    env: BTreeMap<String, String>,
    fs_reads: BTreeMap<String, Vec<u8>>,
    net_gets: BTreeMap<String, String>,
    random_u64: Vec<u64>,
    capability_responses: BTreeMap<(harn_builtin_meta::CapabilityId, String), Vec<crate::VmValue>>,
    stdin_lines: Vec<String>,
}

impl MockHarnessBuilder {
    fn new() -> Self {
        Self {
            clock: paused_clock_at_unix_ms(0),
            env: BTreeMap::new(),
            fs_reads: BTreeMap::new(),
            net_gets: BTreeMap::new(),
            random_u64: Vec::new(),
            capability_responses: BTreeMap::new(),
            stdin_lines: Vec::new(),
        }
    }

    pub fn clock_at_unix_ms(mut self, unix_ms: i64) -> Self {
        self.clock = paused_clock_at_unix_ms(unix_ms);
        self
    }

    pub fn clock_at(mut self, origin: OffsetDateTime) -> Self {
        self.clock = PausedClock::new(origin);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn fs_read(mut self, path: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        self.fs_reads.insert(path.into(), data.into());
        self
    }

    pub fn net_get(mut self, url: impl Into<String>, body: impl Into<String>) -> Self {
        self.net_gets.insert(url.into(), body.into());
        self
    }

    pub fn random_u64(mut self, value: u64) -> Self {
        self.random_u64.push(value);
        self
    }

    /// Queue an exact return value for a capability method that does not have
    /// a purpose-built fixture helper. Responses are consumed FIFO and belong
    /// to this harness instance; no process registry or thread-local mock
    /// state participates.
    pub fn capability_response(
        mut self,
        capability: harn_builtin_meta::CapabilityId,
        method: impl Into<String>,
        value: crate::VmValue,
    ) -> Self {
        self.capability_responses
            .entry((capability, method.into()))
            .or_default()
            .push(value);
        self
    }

    /// Queue a line that `harness.stdio.read_line()` or
    /// `harness.stdio.prompt(...)` will return next. Lines are dequeued
    /// FIFO; once the queue is empty subsequent reads surface EOF
    /// (`nil` for the unstructured form, `{ok: false, status: "eof"}`
    /// for the structured form).
    pub fn stdin_line(mut self, line: impl Into<String>) -> Self {
        self.stdin_lines.push(line.into());
        self
    }

    pub fn build(self) -> Harness {
        let clock = self.clock;
        Harness::with_mode(
            clock.clone() as Arc<dyn Clock>,
            HarnessMode::Mock(Arc::new(MockHarnessState {
                calls: Mutex::new(Vec::new()),
                clock,
                env: self.env,
                fs_reads: self.fs_reads,
                net_gets: self.net_gets,
                random_u64: Mutex::new(self.random_u64.into()),
                capability_responses: Mutex::new(
                    self.capability_responses
                        .into_iter()
                        .map(|(key, values)| (key, values.into()))
                        .collect(),
                ),
                stdin_lines: Mutex::new(self.stdin_lines.into()),
                stdio: Mutex::new(String::new()),
                stderr: Mutex::new(String::new()),
            })),
        )
    }
}

/// The runtime handle threaded into `main(harness: Harness)`.
///
/// Cheap to clone; sub-handles share the same `Arc` inner state.
#[derive(Debug, Clone)]
pub struct Harness {
    inner: Arc<HarnessInner>,
}

impl Harness {
    /// Build the production handle wired to wall-clock time.
    ///
    /// Test-time overrides are scoped to this handle and are reachable only
    /// through `harness.testing`; production construction never consults
    /// thread-local clock state.
    pub fn real() -> Self {
        Self::with_mode(Arc::new(RealClock::new()), HarnessMode::Real)
    }

    /// Build a deny-by-default test handle. Every sub-handle method records a
    /// [`DenyEvent`] and fails with a categorized VM error.
    pub fn null() -> Self {
        Self::with_mode(
            paused_clock_at_unix_ms(0) as Arc<dyn Clock>,
            HarnessMode::Null(NullHarnessState::default()),
        )
    }

    /// Build a record/replay test handle backed by a paused clock.
    pub fn mock() -> MockHarnessBuilder {
        MockHarnessBuilder::new()
    }

    /// Build a handle wired to a caller-supplied clock. Most callers want
    /// [`Self::test`] (which constructs the `PausedClock` for you);
    /// reach for this when an existing `Arc<dyn Clock>` is already in
    /// hand — e.g. a `RecordedClock` wrapper.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::with_mode(clock, HarnessMode::Real)
    }

    /// Construct a `Harness` from a pre-built `Arc<HarnessInner>`.
    /// Used by VM method dispatch when it needs to re-wrap a sub-handle's
    /// inner state into a root `Harness` (e.g. to invoke
    /// [`Self::with_net_policy`] from inside the method dispatcher).
    pub fn from_inner(inner: Arc<HarnessInner>) -> Self {
        Self { inner }
    }

    fn with_mode(clock: Arc<dyn Clock>, mode: HarnessMode) -> Self {
        let clock_control = Arc::new(HarnessClockRouter::new(clock));
        let clock: Arc<dyn Clock> = clock_control.clone();
        let inner = Arc::new(HarnessInner {
            clock,
            clock_control,
            mode,
            net_policy: None,
            secret_provider: None,
            quarantined: Mutex::new(false),
            fixtures: Arc::new(CapabilityFixtureState::default()),
        });
        Self { inner }
    }

    /// Attach a per-harness `harness.net.*` access policy.
    ///
    /// Returns a new `Harness` value whose sub-handles share a fresh
    /// `Arc<HarnessInner>`. Existing handles built off the prior inner
    /// keep operating without the policy — so calling
    /// `harness.with_net_policy(...)` does NOT retroactively gate
    /// references to `harness` held elsewhere. Per issue #1913.
    ///
    /// The clock and mode are propagated verbatim. Mock canned
    /// responses (`net_gets`, `random_u64`, etc.) live behind the
    /// shared `HarnessMode::Mock` payload, so the new handle observes
    /// the same recorded calls and the same canned responses as the
    /// source handle.
    pub fn with_net_policy(&self, policy: crate::harness_net::NetPolicy) -> Self {
        let clock = Arc::clone(&self.inner.clock);
        let clock_control = Arc::clone(&self.inner.clock_control);
        let mode = self.clone_mode_for_child();
        // See `with_mode` for the rationale on this suppression.
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(HarnessInner {
            clock,
            clock_control,
            mode,
            net_policy: Some(policy),
            secret_provider: self.inner.secret_provider.clone(),
            quarantined: Mutex::new(self.is_quarantined()),
            fixtures: Arc::clone(&self.inner.fixtures),
        });
        Self { inner }
    }

    /// Attach a provider for `harness.secrets.*`.
    ///
    /// The provider is intentionally embedder-supplied. Harn owns the typed
    /// method contract; the host owns custody details such as KMS wrapping,
    /// lease storage, audit sinks, and scope policy.
    pub fn with_secret_provider(&self, provider: Arc<dyn crate::secrets::SecretProvider>) -> Self {
        let clock = Arc::clone(&self.inner.clock);
        let clock_control = Arc::clone(&self.inner.clock_control);
        let mode = self.clone_mode_for_child();
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(HarnessInner {
            clock,
            clock_control,
            mode,
            net_policy: self.inner.net_policy.clone(),
            secret_provider: Some(provider),
            quarantined: Mutex::new(self.is_quarantined()),
            fixtures: Arc::clone(&self.inner.fixtures),
        });
        Self { inner }
    }

    fn clone_mode_for_child(&self) -> HarnessMode {
        match &self.inner.mode {
            HarnessMode::Real => HarnessMode::Real,
            HarnessMode::Null(_) => HarnessMode::Null(NullHarnessState::default()),
            HarnessMode::Mock(state) => HarnessMode::Mock(Arc::clone(state)),
        }
    }

    /// `true` if the harness has been marked quarantined by an
    /// `OnViolation::Quarantine` deny event.
    pub fn is_quarantined(&self) -> bool {
        self.inner.is_quarantined()
    }

    pub fn deny_events(&self) -> Vec<DenyEvent> {
        match self.inner.mode() {
            HarnessMode::Null(state) => state.deny_events(),
            HarnessMode::Real | HarnessMode::Mock(_) => Vec::new(),
        }
    }

    pub fn calls(&self) -> Vec<HarnessCall> {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.calls(),
            HarnessMode::Real | HarnessMode::Null(_) => Vec::new(),
        }
    }

    pub fn captured_stdio(&self) -> String {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.stdio(),
            HarnessMode::Real | HarnessMode::Null(_) => String::new(),
        }
    }

    pub fn captured_stderr(&self) -> String {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.stderr(),
            HarnessMode::Real | HarnessMode::Null(_) => String::new(),
        }
    }

    /// Build a deterministic test handle wired to a fresh
    /// [`PausedClock`] pinned at the Unix epoch.
    ///
    /// Returns the harness paired with the underlying `PausedClock` so
    /// tests can drive virtual time through `PausedClock::advance`
    /// while passing the same `Harness` value into the VM. The two
    /// share the underlying `Arc<dyn Clock>`, so the harness reflects
    /// every advance immediately.
    ///
    /// Pairs with [`PausedClock::advance`] / [`PausedClock::set`] — see
    /// [`Self::with_paused_clock`] for picking a non-epoch origin.
    pub fn test() -> (Self, Arc<PausedClock>) {
        Self::with_paused_clock(OffsetDateTime::UNIX_EPOCH)
    }

    /// Like [`Self::test`], but pins the paused clock's wall origin to
    /// `origin`. Lets tests anchor virtual time to a meaningful date
    /// without manually advancing past the epoch first.
    pub fn with_paused_clock(origin: OffsetDateTime) -> (Self, Arc<PausedClock>) {
        let paused = PausedClock::new(origin);
        let as_dyn: Arc<dyn Clock> = paused.clone();
        (Self::with_clock(as_dyn), paused)
    }

    /// Field access for `harness.stdio`.
    pub fn stdio(&self) -> HarnessStdio {
        HarnessStdio {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.term`.
    pub fn term(&self) -> HarnessTerm {
        HarnessTerm {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.clock`.
    pub fn clock(&self) -> HarnessClock {
        HarnessClock {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.fs`.
    pub fn fs(&self) -> HarnessFs {
        HarnessFs {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.env`.
    pub fn env(&self) -> HarnessEnv {
        HarnessEnv {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.random`.
    pub fn random(&self) -> HarnessRandom {
        HarnessRandom {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.net`.
    pub fn net(&self) -> HarnessNet {
        HarnessNet {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.process`.
    pub fn process(&self) -> HarnessProcess {
        HarnessProcess {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.channels`.
    pub fn channels(&self) -> HarnessChannels {
        HarnessChannels {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.system`.
    pub fn system(&self) -> HarnessSystem {
        HarnessSystem {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.secrets`.
    pub fn secrets(&self) -> HarnessSecrets {
        HarnessSecrets {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.llm`.
    pub fn llm(&self) -> HarnessLlm {
        HarnessLlm {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.tenant`.
    pub fn tenant(&self) -> HarnessTenant {
        HarnessTenant {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.auth`.
    pub fn auth(&self) -> HarnessAuth {
        HarnessAuth {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.obs`.
    pub fn obs(&self) -> HarnessObs {
        HarnessObs {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.testing`.
    pub fn testing(&self) -> HarnessTesting {
        HarnessTesting {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.memory`.
    pub fn memory(&self) -> HarnessMemory {
        HarnessMemory {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn sqlite(&self) -> HarnessSqlite {
        HarnessSqlite {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn postgres(&self) -> HarnessPostgres {
        HarnessPostgres {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn agent(&self) -> HarnessAgent {
        HarnessAgent {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Lower this handle into the `VmValue::Harness` payload.
    pub fn into_vm_value(self) -> crate::value::VmValue {
        crate::value::VmValue::harness(VmHarness {
            inner: self.inner,
            kind: HarnessKind::Root,
        })
    }
}

fn paused_clock_at_unix_ms(unix_ms: i64) -> Arc<PausedClock> {
    let nanos = (unix_ms as i128).saturating_mul(1_000_000);
    let origin =
        OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    PausedClock::new(origin)
}

pub(crate) fn vm_string(value: impl Into<String>) -> crate::VmValue {
    crate::VmValue::String(arcstr::ArcStr::from(value.into()))
}

impl Default for Harness {
    fn default() -> Self {
        Self::real()
    }
}

/// stdio sub-handle: `print`, `println`, `eprint`, `eprintln`, `prompt`,
/// `read_line`.
#[derive(Debug, Clone)]
pub struct HarnessStdio {
    inner: Arc<HarnessInner>,
}

/// term sub-handle: `width`, `height`, `read_password`.
#[derive(Debug, Clone)]
pub struct HarnessTerm {
    inner: Arc<HarnessInner>,
}

/// clock sub-handle: `now`, `monotonic_now`, `sleep`.
#[derive(Debug, Clone)]
pub struct HarnessClock {
    inner: Arc<HarnessInner>,
}

impl HarnessClock {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        self.inner.clock()
    }
}

include!("harness/fixtures.rs");
include!("harness/value.rs");
include!("harness/tests.rs");
/// fs sub-handle: `read_file`, `write_file`, `exists`, `list_dir`,
/// `delete_file`, ...
#[derive(Debug, Clone)]
pub struct HarnessFs {
    inner: Arc<HarnessInner>,
}

/// env sub-handle: `get`, `set`, `vars`.
#[derive(Debug, Clone)]
pub struct HarnessEnv {
    inner: Arc<HarnessInner>,
}

/// random sub-handle: `u64`, `range`, `f64`, ...
#[derive(Debug, Clone)]
pub struct HarnessRandom {
    inner: Arc<HarnessInner>,
}

/// net sub-handle: `http_get`, `http_post`, ...
#[derive(Debug, Clone)]
pub struct HarnessNet {
    inner: Arc<HarnessInner>,
}

/// process sub-handle: structured process execution and shell discovery.
#[derive(Debug, Clone)]
pub struct HarnessProcess {
    inner: Arc<HarnessInner>,
}

/// Durable transcript channel sub-handle.
#[derive(Debug, Clone)]
pub struct HarnessChannels {
    inner: Arc<HarnessInner>,
}

/// system sub-handle: `cpu`, `memory`, `gpus`, `temperature`, `platform`,
/// `processes`. Read-only host introspection — no side effects on the host
/// system. Gated by the harness handle so scripts running under
/// `Harness::null()` or restricted policies cannot fingerprint the runner
/// without an explicit grant (issue #1912 / epic #1765).
#[derive(Debug, Clone)]
pub struct HarnessSystem {
    inner: Arc<HarnessInner>,
}

/// secrets sub-handle: `read`, `write`, `rotate`, `lease`.
#[derive(Debug, Clone)]
pub struct HarnessSecrets {
    inner: Arc<HarnessInner>,
}

/// llm sub-handle: `catalog`, `providers`.
#[derive(Debug, Clone)]
pub struct HarnessLlm {
    inner: Arc<HarnessInner>,
}

/// tenant sub-handle: `id`, `try_id`. Surfaces the ambient `TenantId`
/// bound by the dispatching host (see [`crate::harness_tenant`]). No
/// host state — the methods consult a thread-local stack — but the
/// handle still rides the shared `Arc<HarnessInner>` so null/mock-mode
/// gating in [`crate::vm::methods::harness`] applies uniformly.
#[derive(Debug, Clone)]
pub struct HarnessTenant {
    inner: Arc<HarnessInner>,
}

/// auth sub-handle: `is_authenticated`, `subject` / `try_subject`,
/// `scheme` / `try_scheme`, `kind`, `scopes`, `has_scope`. Surfaces the
/// ambient authenticated principal bound by the dispatching host (see
/// [`crate::harness_auth`]). Like [`HarnessTenant`] it holds no host
/// state — the methods consult a thread-local stack — but rides the
/// shared `Arc<HarnessInner>` so null/mock-mode gating in
/// [`crate::vm::methods::harness`] applies uniformly.
#[derive(Debug, Clone)]
pub struct HarnessAuth {
    inner: Arc<HarnessInner>,
}

/// obs sub-handle: `span` / `start_span` / `end_span` / `counter` /
/// `histogram` / `gauge` / `log` / `request_id`. Wraps the existing
/// `__obs_*` builtins and the request_id ambient pushed by the
/// dispatching host (see [`crate::observability::request_id`]) behind a
/// typed surface so handlers don't reach into the lower-level builtins
/// directly. Backend selection / exporter wiring still lives in
/// [`crate::events`] (OTel sink) and `std/observability` (`configure`,
/// backend factories) — the sub-handle is the *emit-side* surface that
/// every harn-serve primitive shares.
#[derive(Debug, Clone)]
pub struct HarnessObs {
    inner: Arc<HarnessInner>,
}

/// Per-harness deterministic fixture control. Responses and call records are
/// owned by this Harness instance rather than a process registry or
/// thread-local scope.
#[derive(Debug, Clone)]
pub struct HarnessTesting {
    inner: Arc<HarnessInner>,
}

/// Durable memory sub-handle. Keeping this distinct from [`HarnessEmbed`]
/// prevents a caller that can compute vectors from implicitly gaining
/// persistent read/write authority.
#[derive(Debug, Clone)]
pub struct HarnessMemory {
    inner: Arc<HarnessInner>,
}

#[derive(Debug, Clone)]
pub struct HarnessSqlite {
    inner: Arc<HarnessInner>,
}

#[derive(Debug, Clone)]
pub struct HarnessPostgres {
    inner: Arc<HarnessInner>,
}

#[derive(Debug, Clone)]
pub struct HarnessAgent {
    inner: Arc<HarnessInner>,
}

macro_rules! sub_handle_inner {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $ty {
                #[allow(dead_code)]
                pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
                    &self.inner
                }
            }
        )*
    };
}
sub_handle_inner!(
    HarnessStdio,
    HarnessTerm,
    HarnessFs,
    HarnessEnv,
    HarnessRandom,
    HarnessNet,
    HarnessProcess,
    HarnessChannels,
    HarnessSystem,
    HarnessSecrets,
    HarnessLlm,
    HarnessTenant,
    HarnessAuth,
    HarnessObs,
    HarnessMemory,
    HarnessSqlite,
    HarnessPostgres,
    HarnessAgent,
    HarnessTesting,
);

impl HarnessClock {
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }
}
