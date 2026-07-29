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
}

#[derive(Debug, Default)]
pub(crate) struct CapabilityFixtureState {
    inner: Mutex<CapabilityFixtureScopes>,
}

#[derive(Debug, Default)]
struct CapabilityFixtureScopes {
    current: CapabilityFixtureInner,
    stack: Vec<CapabilityFixtureInner>,
}

#[derive(Debug, Default, Clone)]
struct CapabilityFixtureInner {
    enabled: bool,
    responses:
        BTreeMap<(harn_builtin_meta::CapabilityId, String), VecDeque<CapabilityFixtureResponse>>,
    calls: Vec<CapabilityFixtureCall>,
}

#[derive(Debug, Clone)]
struct CapabilityFixtureResponse {
    when: Option<crate::value::DictMap>,
    repeat: bool,
    result: Result<crate::VmValue, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CapabilityFixtureCall {
    pub(crate) capability: harn_builtin_meta::CapabilityId,
    pub(crate) method: String,
    pub(crate) args: Vec<crate::VmValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CapabilityDriverFixtureContract {
    pub(crate) capability: harn_builtin_meta::CapabilityId,
    pub(crate) method: &'static str,
}

/// Host-driver response seams whose enclosing capability remains VM-owned.
///
/// These are deliberately distinct from public harness methods: a fixture for
/// `interaction.approval_response` supplies a human decision while still
/// exercising Harn's request envelope, quorum, signing, waitpoint, and receipt
/// logic. Keep this registry closed so testing cannot invent ambient wire
/// operations.
pub(crate) const CAPABILITY_DRIVER_FIXTURES: &[CapabilityDriverFixtureContract] = &[
    CapabilityDriverFixtureContract {
        capability: harn_builtin_meta::CapabilityId::Interaction,
        method: "question_response",
    },
    CapabilityDriverFixtureContract {
        capability: harn_builtin_meta::CapabilityId::Interaction,
        method: "approval_response",
    },
    CapabilityDriverFixtureContract {
        capability: harn_builtin_meta::CapabilityId::Interaction,
        method: "dual_control_response",
    },
    CapabilityDriverFixtureContract {
        capability: harn_builtin_meta::CapabilityId::Interaction,
        method: "escalation_response",
    },
    CapabilityDriverFixtureContract {
        capability: harn_builtin_meta::CapabilityId::Embed,
        method: "text_response",
    },
];

pub(crate) fn is_capability_driver_fixture(
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
) -> bool {
    CAPABILITY_DRIVER_FIXTURES
        .iter()
        .any(|contract| contract.capability == capability && contract.method == method)
}

impl CapabilityFixtureState {
    pub(crate) fn clear(&self) {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        scopes.current = CapabilityFixtureInner {
            enabled: true,
            ..CapabilityFixtureInner::default()
        };
    }

    pub(crate) fn push_scope(&self) {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        let previous = std::mem::replace(
            &mut scopes.current,
            CapabilityFixtureInner {
                enabled: true,
                ..CapabilityFixtureInner::default()
            },
        );
        scopes.stack.push(previous);
    }

    pub(crate) fn pop_scope(&self) -> Result<(), crate::VmError> {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        let Some(previous) = scopes.stack.pop() else {
            return Err(crate::VmError::Runtime(
                "HarnessTesting.pop_scope called without a matching push_scope".to_string(),
            ));
        };
        scopes.current = previous;
        Ok(())
    }

    pub(crate) fn respond(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
        response: Result<crate::VmValue, String>,
        when: Option<crate::value::DictMap>,
        repeat: bool,
    ) {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        scopes.current.enabled = true;
        scopes
            .current
            .responses
            .entry((capability, method.to_string()))
            .or_default()
            .push_back(CapabilityFixtureResponse {
                when,
                repeat,
                result: response,
            });
    }

    pub(crate) fn dispatch(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
        args: &[crate::VmValue],
    ) -> Option<Result<crate::VmValue, crate::VmError>> {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        if !scopes.current.enabled {
            return None;
        }
        let key = (capability, method.to_string());
        if !scopes.current.responses.contains_key(&key) {
            return None;
        }
        scopes.current.calls.push(CapabilityFixtureCall {
            capability,
            method: method.to_string(),
            args: args.to_vec(),
        });
        let queue = scopes
            .current
            .responses
            .get_mut(&key)
            .expect("fixture key checked above");
        let selector_match = |fixture: &CapabilityFixtureResponse| {
            let Some(selector) = fixture.when.as_ref() else {
                return false;
            };
            let Some(actual) = args.first().and_then(crate::VmValue::as_dict) else {
                return false;
            };
            selector.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|value| crate::value::values_equal(value, expected))
            })
        };
        let matched = queue
            .iter()
            .position(selector_match)
            .or_else(|| queue.iter().position(|fixture| fixture.when.is_none()));
        match matched {
            Some(index) => {
                let fixture = if queue[index].repeat {
                    Some(queue[index].clone())
                } else {
                    queue.remove(index)
                };
                fixture.map(|fixture| {
                    fixture.result.map_err(|message| {
                        crate::VmError::Thrown(crate::VmValue::String(arcstr::ArcStr::from(
                            message,
                        )))
                    })
                })
            }
            None => Some(Err(crate::VmError::Runtime(format!(
                "no fixture for harness.{}.{method} matched arguments {}",
                capability.field_name(),
                crate::VmValue::List(std::sync::Arc::new(args.to_vec())).display()
            )))),
        }
    }

    pub(crate) fn calls(&self) -> Vec<CapabilityFixtureCall> {
        self.inner
            .lock()
            .expect("capability fixtures poisoned")
            .current
            .calls
            .clone()
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

/// process sub-handle: `spawn_captured`.
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

/// Compact `VmValue` payload for a `Harness` or any of its sub-handles.
///
/// All handle variants share one `Arc<HarnessInner>`; `kind` discriminates the
/// surface the VM exposes for property access and method dispatch.
#[derive(Clone)]
pub struct VmHarness {
    inner: Arc<HarnessInner>,
    kind: HarnessKind,
}

impl VmHarness {
    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    pub fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }

    /// Get the sub-handle reached by a field name (`stdio`, `clock`, etc.).
    /// Returns `None` when the receiver is itself a sub-handle or the field
    /// is unknown.
    pub fn sub_handle(&self, field: &str) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root {
            return None;
        }
        let kind = HarnessKind::from_field_name(field)?;
        self.sub_handle_kind(kind)
    }

    pub(crate) fn sub_handle_kind(&self, kind: HarnessKind) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root || kind == HarnessKind::Root {
            return None;
        }
        Some(VmHarness {
            inner: Arc::clone(&self.inner),
            kind,
        })
    }
}

impl fmt::Debug for VmHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmHarness")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretProvider;
    use async_trait::async_trait;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SecretCall {
        operation: &'static str,
        id: crate::secrets::SecretId,
        scope: crate::secrets::SecretScope,
        request_id: Option<String>,
        actor_subject: Option<String>,
        actor_kind: Option<String>,
        duration_ms: Option<u64>,
        grace_ms: Option<u64>,
        ttl_ms: Option<u64>,
    }

    #[derive(Clone, Default)]
    struct RecordingSecretProvider {
        inner: Arc<RecordingSecretProviderInner>,
    }

    #[derive(Default)]
    struct RecordingSecretProviderInner {
        versions: Mutex<BTreeMap<crate::secrets::SecretId, Vec<Vec<u8>>>>,
        calls: Mutex<Vec<SecretCall>>,
    }

    impl RecordingSecretProvider {
        fn calls(&self) -> Vec<SecretCall> {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .clone()
        }

        fn record(
            &self,
            operation: &'static str,
            id: &crate::secrets::SecretId,
            scope: &crate::secrets::SecretScope,
            audit: &crate::secrets::SecretAuditContext,
            duration_ms: Option<u64>,
            grace_ms: Option<u64>,
            ttl_ms: Option<u64>,
        ) {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .push(SecretCall {
                    operation,
                    id: id.clone(),
                    scope: scope.clone(),
                    request_id: audit.request_id.clone(),
                    actor_subject: audit.actor_subject.clone(),
                    actor_kind: audit.actor_kind.clone(),
                    duration_ms,
                    grace_ms,
                    ttl_ms,
                });
        }

        fn read_latest(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<(u64, Vec<u8>), crate::secrets::SecretError> {
            let versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions
                .get(id)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| crate::secrets::SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: id.clone(),
                })?;
            Ok((
                values.len() as u64,
                values.last().expect("non-empty").clone(),
            ))
        }

        fn write_version(
            &self,
            id: &crate::secrets::SecretId,
            value: &crate::secrets::SecretBytes,
        ) -> u64 {
            let mut versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions.entry(id.clone()).or_default();
            values.push(value.with_exposed(|bytes| bytes.to_vec()));
            values.len() as u64
        }
    }

    fn duration_ms(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    #[async_trait]
    impl crate::secrets::SecretProvider for RecordingSecretProvider {
        async fn get(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.read_latest(id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn put(
            &self,
            id: &crate::secrets::SecretId,
            value: crate::secrets::SecretBytes,
        ) -> Result<(), crate::secrets::SecretError> {
            self.write_version(id, &value);
            Ok(())
        }

        async fn rotate(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::RotationHandle, crate::secrets::SecretError> {
            let (from_version, value) = self.read_latest(id)?;
            let to_version =
                self.write_version(id, &crate::secrets::SecretBytes::from(value.as_slice()));
            Ok(crate::secrets::RotationHandle {
                provider: self.namespace().to_string(),
                id: id
                    .clone()
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                from_version: Some(from_version),
                to_version: Some(to_version),
            })
        }

        async fn list(
            &self,
            _prefix: &crate::secrets::SecretId,
        ) -> Result<Vec<crate::secrets::SecretMeta>, crate::secrets::SecretError> {
            Ok(Vec::new())
        }

        async fn read_scoped(
            &self,
            request: crate::secrets::SecretReadRequest,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.record(
                "read",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                None,
            );
            self.read_latest(&request.id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn write_scoped(
            &self,
            request: crate::secrets::SecretWriteRequest,
        ) -> Result<crate::secrets::SecretWriteReceipt, crate::secrets::SecretError> {
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "write",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                ttl_ms,
            );
            let version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretWriteReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                version: Some(version),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn rotate_scoped(
            &self,
            request: crate::secrets::SecretRotateRequest,
        ) -> Result<crate::secrets::SecretRotationReceipt, crate::secrets::SecretError> {
            let grace_ms = request.options.grace.map(duration_ms);
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "rotate",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                grace_ms,
                ttl_ms,
            );
            let from_version = self
                .inner
                .versions
                .lock()
                .expect("versions lock poisoned")
                .get(&request.id)
                .map(|values| values.len() as u64);
            let to_version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretRotationReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                scope: request.scope,
                from_version,
                to_version: Some(to_version),
                grace_until_unix_ms: grace_ms.map(|grace| 1_700_000_000_000_i64 + grace as i64),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn lease_scoped(
            &self,
            request: crate::secrets::SecretLeaseRequest,
        ) -> Result<crate::secrets::SecretLeaseGrant, crate::secrets::SecretError> {
            let duration = duration_ms(request.duration);
            self.record(
                "lease",
                &request.id,
                &request.scope,
                &request.audit,
                Some(duration),
                None,
                None,
            );
            let (version, value) = self.read_latest(&request.id)?;
            Ok(crate::secrets::SecretLeaseGrant {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                lease_id: format!("lease-{version}"),
                value: crate::secrets::SecretBytes::from(value),
                expires_at_unix_ms: 1_700_000_000_000_i64 + duration as i64,
            })
        }

        async fn delete_scoped(
            &self,
            request: crate::secrets::SecretDeleteRequest,
        ) -> Result<(), crate::secrets::SecretError> {
            self.record(
                "delete",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                None,
            );
            let removed = self
                .inner
                .versions
                .lock()
                .expect("versions lock poisoned")
                .remove(&request.id)
                .is_some();
            if removed {
                Ok(())
            } else {
                Err(crate::secrets::SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: request.id,
                })
            }
        }

        fn namespace(&self) -> &'static str {
            "recording"
        }

        fn supports_versions(&self) -> bool {
            true
        }
    }

    #[test]
    fn real_constructs_without_panic() {
        let _harness = Harness::real();
    }

    #[test]
    fn sub_handles_share_inner_state() {
        let harness = Harness::real();
        let stdio_inner = Arc::as_ptr(harness.stdio().inner());
        let clock_inner = Arc::as_ptr(harness.clock().inner());
        assert_eq!(stdio_inner, clock_inner, "sub-handles share Arc<Inner>");
    }

    #[test]
    fn kinds_round_trip_through_field_names() {
        for kind in HarnessKind::SUB_HANDLES {
            let field = kind.field_name().unwrap();
            assert_eq!(HarnessKind::from_field_name(field), Some(*kind));
        }
        assert!(HarnessKind::from_field_name("nope").is_none());
        assert!(HarnessKind::Root.field_name().is_none());
    }

    #[test]
    fn vm_harness_property_access_returns_sub_handle() {
        let root = match Harness::real().into_vm_value() {
            crate::value::VmValue::Harness(h) => h,
            other => panic!("expected Harness variant, got {}", other.type_name()),
        };
        let stdio = root.sub_handle("stdio").expect("stdio sub-handle");
        assert_eq!(stdio.kind(), HarnessKind::Stdio);
        assert!(stdio.sub_handle("clock").is_none(), "nested access denied");
        assert!(root.sub_handle("not_a_field").is_none());
    }

    #[test]
    fn test_constructor_clock_advances_under_paused_clock_advance() {
        let (harness, paused) = Harness::test();
        let clock = harness.clock();
        let start_wall = clock.clock().now_utc();
        assert_eq!(start_wall, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.clock().monotonic_ms(), 0);

        paused.advance(Duration::from_millis(1_500));
        assert_eq!(clock.clock().monotonic_ms(), 1_500);
        let after_wall = clock.clock().now_utc();
        assert_eq!(after_wall - start_wall, time::Duration::milliseconds(1_500));
    }

    #[test]
    fn with_paused_clock_pins_origin() {
        let origin = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let (harness, paused) = Harness::with_paused_clock(origin);
        assert_eq!(harness.clock().clock().now_utc(), origin);
        paused.advance(Duration::from_mins(1));
        assert_eq!(
            harness.clock().clock().now_utc() - origin,
            time::Duration::seconds(60)
        );
    }

    #[test]
    fn null_harness_records_deny_events_for_every_sub_handle() {
        let harness = Harness::null();
        for source in [
            r#"fn main(harness: Harness) { harness.stdio.println("blocked") }"#,
            r"fn main(harness: Harness) { harness.term.width() }",
            r"fn main(harness: Harness) { harness.clock.now_ms() }",
            r#"fn main(harness: Harness) { harness.fs.read_text("/x") }"#,
            r#"fn main(harness: Harness) { harness.env.get("KEY") }"#,
            r"fn main(harness: Harness) { harness.random.u64() }",
            r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#,
            r#"fn main(harness: Harness) { harness.process.spawn_captured({cmd: "printf", args: ["x"]}) }"#,
            r"fn main(harness: Harness) { harness.system.cpu() }",
            r#"fn main(harness: Harness) { harness.secrets.read("blocked") }"#,
            r"fn main(harness: Harness) { harness.llm.catalog() }",
            r"fn main(harness: Harness) { harness.tenant.id() }",
            r"fn main(harness: Harness) { harness.auth.subject() }",
            r#"fn main(harness: Harness) { harness.obs.log("blocked", "info", {}) }"#,
        ] {
            let error = run_harness_source(source, harness.clone()).expect_err("call denied");
            assert!(
                error.contains("NullHarness denied"),
                "unexpected deny error: {error}"
            );
        }

        let events = harness.deny_events();
        let observed: Vec<_> = events
            .iter()
            .map(|event| (event.sub_handle, event.method.as_str()))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "println"),
                (HarnessKind::Term, "width"),
                (HarnessKind::Clock, "now_ms"),
                (HarnessKind::Fs, "read_text"),
                (HarnessKind::Env, "get"),
                (HarnessKind::Random, "u64"),
                (HarnessKind::Net, "get"),
                (HarnessKind::Process, "spawn_captured"),
                (HarnessKind::System, "cpu"),
                (HarnessKind::Secrets, "read"),
                (HarnessKind::Llm, "catalog"),
                (HarnessKind::Tenant, "id"),
                (HarnessKind::Auth, "subject"),
                (HarnessKind::Obs, "log"),
            ]
        );
        assert_eq!(events[0].args, vec!["blocked"]);
        assert_eq!(events[3].args, vec!["/x"]);
    }

    #[test]
    fn auth_sub_handle_reads_bound_principal() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "k_123".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["admin:dlq:write", "read:events"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("operator".to_string()),
        });
        let source = r#"
fn main(harness: Harness) {
  __io_println(harness.auth.is_authenticated())
  __io_println(harness.auth.subject())
  __io_println(harness.auth.scheme())
  __io_println(harness.auth.kind())
  __io_println(harness.auth.has_scope("admin:dlq:write"))
  __io_println(harness.auth.has_scope("missing:scope"))
  __io_println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "true\nk_123\napikey\noperator\ntrue\nfalse\n2\n");
    }

    #[test]
    fn auth_sub_handle_without_principal_reports_anonymous() {
        // No `enter_auth_principal` guard — the dispatch is unauthenticated,
        // so the presence/scope getters degrade rather than error and
        // `subject()` raises the canonical Auth error.
        let source = r#"
fn main(harness: Harness) {
  if harness.auth.is_authenticated() { __io_println("auth") } else { __io_println("anon") }
  __io_println(harness.auth.has_scope("x"))
  __io_println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "anon\nfalse\n0\n");

        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.auth.subject() }",
            Harness::real(),
        )
        .expect_err("subject() requires a bound principal");
        assert!(
            error.contains("no principal bound"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn secrets_sub_handle_uses_provider_scope_and_audit_context() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        use crate::harness_tenant::enter_tenant;
        use crate::observability::request_id::enter_request_id;

        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));
        let _tenant = enter_tenant(crate::TenantId::new("tenant-a"));
        let _request = enter_request_id("req-499");
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "api-key-1".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["secrets:read", "secrets:write"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("tenant_api_key".to_string()),
        });

        let source = r#"
fn main(harness: Harness) {
  const scope = {kind: "workspace", id: "workspace-a"}
  const written = harness.secrets.write("github.token", "v1", scope, 5000)
  __io_println(written.provider)
  __io_println(written.scope.kind)
  __io_println(written.scope.id)
  __io_println(written.id.namespace)
  __io_println(written.version)
  __io_println(harness.secrets.read("github.token", scope))
  const rotated = harness.secrets.rotate("github.token", { -> "v2" }, scope, {grace_ms: 250, ttl_ms: 7500})
  __io_println(rotated.from_version)
  __io_println(rotated.to_version)
  const grant = harness.secrets.lease("github.token", 1000, scope)
  __io_println(grant.value)
  __io_println(grant.scope.id)
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(
            output,
            "recording\nworkspace\nworkspace-a\nharn.workspace.workspace-a\n1\nv1\n1\n2\nv2\nworkspace-a\n"
        );

        let calls = provider.calls();
        assert_eq!(
            calls.iter().map(|call| call.operation).collect::<Vec<_>>(),
            vec!["write", "read", "rotate", "lease"]
        );
        for call in &calls {
            assert_eq!(
                call.scope,
                crate::secrets::SecretScope::workspace("workspace-a")
            );
            assert_eq!(call.request_id.as_deref(), Some("req-499"));
            assert_eq!(call.actor_subject.as_deref(), Some("api-key-1"));
            assert_eq!(call.actor_kind.as_deref(), Some("tenant_api_key"));
        }
        assert_eq!(calls[0].ttl_ms, Some(5_000));
        assert_eq!(calls[2].grace_ms, Some(250));
        assert_eq!(calls[2].ttl_ms, Some(7_500));
        assert_eq!(calls[3].duration_ms, Some(1_000));
    }

    #[test]
    fn secrets_sub_handle_accepts_absolute_connector_secret_ids() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        let source = r#"
fn main(harness: Harness) {
  harness.secrets.write("google_workspace/access-token", "token-v1")
  __io_println(harness.secrets.read("google_workspace/access-token"))
  harness.secrets.write("harn-secret://google_workspace/refresh-token", "refresh-v1")
  __io_println(harness.secrets.read("harn-secret://google_workspace/refresh-token"))
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(output, "token-v1\nrefresh-v1\n");

        let calls = provider.calls();
        assert_eq!(
            calls.iter().map(|call| call.id.clone()).collect::<Vec<_>>(),
            vec![
                crate::secrets::connector_access_token_id("google_workspace"),
                crate::secrets::connector_access_token_id("google_workspace"),
                crate::secrets::connector_refresh_token_id("google_workspace"),
                crate::secrets::connector_refresh_token_id("google_workspace"),
            ]
        );
    }

    // End-to-end fixture for the `std/oauth/storage` `secrets({provider})`
    // backend, driven entirely in-process against the fake secret host —
    // no network, no real keyring. Exercises roundtrip, rotating-refresh
    // preservation, delete, and `harn connect` composition (connector-shaped
    // scopes string + preserved connector metadata).
    #[test]
    fn secrets_backed_oauth_storage_roundtrips_and_preserves_refresh() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        let source = r#"
import { secrets } from "std/oauth/storage"

fn assert_true(label, cond) {
  if !cond { throw label }
}

fn main(harness: Harness) {
  const store = secrets({provider: "github"})

  // Absent key -> nil (NotFound is swallowed by the backend).
  assert_true("missing-nil", store.get("github") == nil)

  // Roundtrip: the client default storage_key is the provider id.
  store.set(
    "github",
    {access_token: "a1", refresh_token: "r1", expires_at_unix: 100, scopes: ["repo", "read:user"]},
  )
  const t1 = store.get("github")
  assert_true("t1-access", t1.access_token == "a1")
  assert_true("t1-refresh", t1.refresh_token == "r1")
  assert_true("t1-scopes", join(t1.scopes, ",") == "repo,read:user")

  // Update omitting the refresh token must NOT drop it.
  store.set("github", {access_token: "a2", expires_at_unix: 200})
  const t2 = store.get("github")
  assert_true("t2-access", t2.access_token == "a2")
  assert_true("t2-refresh-preserved", t2.refresh_token == "r1")

  // A newly-issued refresh token rotates the stored one.
  store.set("github", {access_token: "a3", refresh_token: "r2"})
  assert_true("t3-refresh-rotated", store.get("github").refresh_token == "r2")

  // Delete clears the credential.
  store.delete("github")
  assert_true("deleted-nil", store.get("github") == nil)

  // Compose with `harn connect`: seed the canonical <provider>/oauth-token
  // secret with a connector-shaped payload (scopes as a space-joined string
  // plus connector-only metadata).
  const seed = json_stringify(
    {
      access_token: "conn-a",
      refresh_token: "conn-r",
      scopes: "repo read:user",
      token_endpoint: "https://github.com/login/oauth/access_token",
      client_id: "cid",
    },
  )
  harness.secrets.write("github/oauth-token", seed)

  const c1 = store.get("github")
  assert_true("compose-access", c1.access_token == "conn-a")
  assert_true("compose-scopes-normalized", join(c1.scopes, ",") == "repo,read:user")

  // A refresh writing a fresh token must preserve the connector metadata.
  store.set("github", {access_token: "conn-a2", refresh_token: "conn-r2", expires_at_unix: 300})
  const reparsed = json_parse(harness.secrets.read("github/oauth-token"))
  assert_true("compose-access2", reparsed.access_token == "conn-a2")
  assert_true(
    "compose-endpoint-preserved",
    reparsed.token_endpoint == "https://github.com/login/oauth/access_token",
  )
  assert_true("compose-client-id-preserved", reparsed.client_id == "cid")

  __io_println("secrets-storage-ok")
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(output, "secrets-storage-ok\n");

        // The token blob is stored under the connector's canonical id.
        assert!(
            provider
                .calls()
                .iter()
                .any(|call| call.id == crate::secrets::connector_oauth_token_id("github")),
            "expected writes under the canonical github/oauth-token id"
        );
    }

    #[test]
    fn secrets_sub_handle_denies_runtime_reserved_provenance_namespace() {
        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));

        for source in [
            r#"
fn main(harness: Harness) {
  harness.secrets.read("harn-cli.ed25519.seed", {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.write("harn-cli.ed25519.seed", "forged", {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.rotate("harn-cli.ed25519.seed", { -> "forged" }, {kind: "provenance"})
}
"#,
            r#"
fn main(harness: Harness) {
  harness.secrets.lease("harn-cli.ed25519.seed", 1000, {kind: "provenance"})
}
"#,
        ] {
            let error =
                run_harness_source(source, harness.clone()).expect_err("reserved secret denied");
            assert!(
                error.contains("reserved for Harn runtime provenance signing"),
                "unexpected error: {error}"
            );
        }

        assert!(
            provider.calls().is_empty(),
            "reserved namespace denial must happen before provider dispatch"
        );
    }

    #[test]
    fn mock_harness_replays_canned_responses_and_records_calls() {
        let harness = Harness::mock()
            .clock_at_unix_ms(1_700_000_000_000)
            .env("KEY", "value")
            .fs_read("/x", b"data".to_vec())
            .random_u64(42)
            .net_get("https://example.test", "body")
            .build();

        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.print("partial ")
  harness.stdio.println("line")
  __io_println(harness.term.width())
  __io_println(harness.term.height())
  __io_println(harness.clock.now_ms())
  harness.clock.sleep_ms(250)
  __io_println(harness.clock.now_ms())
  __io_println(harness.clock.monotonic_ms())
  __io_println(harness.env.get("KEY"))
  __io_println(harness.fs.read_text("/x"))
  __io_println(harness.fs.exists("/missing"))
  __io_println(harness.random.u64())
  __io_println(harness.net.get("https://example.test"))
  __io_println(sha256_hex(""))
  __io_println(len(harness.llm.catalog()) > 0)
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(harness.captured_stdio(), "partial line\n");
        assert_eq!(
            output,
            "80\n24\n1700000000000\n1700000000250\n250\nvalue\ndata\nfalse\n42\nbody\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\ntrue\n"
        );
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "print".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Term, "width".to_string()),
                (HarnessKind::Term, "height".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Clock, "sleep_ms".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Clock, "monotonic_ms".to_string()),
                (HarnessKind::Env, "get".to_string()),
                (HarnessKind::Fs, "read_text".to_string()),
                (HarnessKind::Fs, "exists".to_string()),
                (HarnessKind::Random, "u64".to_string()),
                (HarnessKind::Net, "get".to_string()),
                (HarnessKind::Llm, "catalog".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_owns_generic_capability_responses_without_global_registries() {
        let harness = Harness::mock()
            .capability_response(
                harn_builtin_meta::CapabilityId::Process,
                "run",
                crate::stdlib::json_to_vm_value(&serde_json::json!({"ok": true})),
            )
            .capability_response(
                harn_builtin_meta::CapabilityId::Interaction,
                "ask",
                crate::VmValue::String(arcstr::ArcStr::from("yes")),
            )
            .capability_response(
                harn_builtin_meta::CapabilityId::Project,
                "scan",
                crate::stdlib::json_to_vm_value(&serde_json::json!({"language": "rust"})),
            )
            .build();

        run_harness_source(
            r#"
pipeline default(harness: Harness) {
  const process = harness.process.run({argv: ["never-executed"]})
  const answer = harness.interaction.ask("continue?")
  const project = harness.project.scan("/never-read")
  harness.stdio.println(process.ok)
  harness.stdio.println(answer)
  harness.stdio.println(project.language)
}
"#,
            harness.clone(),
        )
        .expect("every effect is served by the harness instance");

        assert_eq!(harness.captured_stdio(), "true\nyes\nrust\n");
        let observed = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Process, "run".to_string()),
                (HarnessKind::Interaction, "ask".to_string()),
                (HarnessKind::Project, "scan".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_records_repeated_cached_harness_method_calls() {
        let harness = Harness::mock().env("KEY", "value").build();

        run_harness_source(
            r#"
fn main(harness: Harness) {
  let i = 0
  while i < 3 {
    const _ = harness.clock.elapsed()
    const value = harness.env.get_or("KEY", "")
    harness.stdio.println(value)
    i = i + 1
  }
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(harness.captured_stdio(), "value\nvalue\nvalue\n");
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_replays_random_values_fifo() {
        let harness = Harness::mock()
            .random_u64(7)
            .random_u64(11)
            .random_u64(u64::MAX)
            .build();

        let output = run_harness_source(
            r"
fn main(harness: Harness) {
  __io_println(harness.random.u64())
  __io_println(harness.random.u64())
  __io_println(harness.random.u64())
}
",
            harness,
        )
        .expect("mock random succeeds");

        assert_eq!(output, "7\n11\n9223372036854775807\n");
    }

    #[test]
    fn mock_harness_reports_missing_canned_responses() {
        let cases = [
            (
                r#"fn main(harness: Harness) { harness.fs.read_text("/missing") }"#,
                "MockHarness has no fs_read response for /missing",
            ),
            (
                r"fn main(harness: Harness) { harness.random.u64() }",
                "MockHarness has no random_u64 response",
            ),
            (
                r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
                "MockHarness has no net_get response for https://missing.test",
            ),
            (
                r#"fn main(harness: Harness) { harness.process.spawn_captured({cmd: "printf", args: ["x"]}) }"#,
                "MockHarness has no process spawn response",
            ),
        ];

        for (source, expected) in cases {
            let error = run_harness_source(source, Harness::mock().build())
                .expect_err("missing mock response fails");
            assert!(
                error.contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    }

    #[test]
    fn mock_harness_records_failed_calls() {
        let harness = Harness::mock().build();
        let error = run_harness_source(
            r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
            harness.clone(),
        )
        .expect_err("missing mock response fails");

        assert!(error.contains("MockHarness has no net_get response"));
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Net,
                method: "get".to_string(),
                args: vec!["https://missing.test".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_captures_stderr_separately_from_stdout() {
        let harness = Harness::mock().build();
        run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println("stdout line")
  harness.stdio.eprint("err ")
  harness.stdio.eprintln("trail")
}
"#,
            harness.clone(),
        )
        .expect("stderr capture run succeeds");
        assert_eq!(harness.captured_stdio(), "stdout line\n");
        assert_eq!(harness.captured_stderr(), "err trail\n");
    }

    #[test]
    fn mock_harness_replays_stdin_lines_for_read_and_prompt() {
        let harness = Harness::mock()
            .stdin_line("first")
            .stdin_line("second")
            .build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.stdio.read_line())
  harness.stdio.println(harness.stdio.prompt("answer: "))
  const eof = harness.stdio.read_line({trim: false})
  harness.stdio.println(eof.status)
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");
        // All stdio writes route to the mock capture buffer; vm.output stays empty.
        assert_eq!(output, "");
        assert_eq!(harness.captured_stdio(), "first\nanswer: second\neof\n");
    }

    #[test]
    fn mock_harness_replays_password_input_without_stdout_echo() {
        let harness = Harness::mock().stdin_line("secret").build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  __io_println(harness.term.read_password("password: "))
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");

        assert_eq!(output, "secret\n");
        assert_eq!(harness.captured_stdio(), "");
        assert_eq!(harness.captured_stderr(), "password: ");
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Term,
                method: "read_password".to_string(),
                args: vec!["password: ".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_rejects_wrong_argument_types() {
        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.fs.read_text(1) }",
            Harness::mock().build(),
        )
        .expect_err("wrong argument type fails");

        // A wrong-typed argument is rejected by one of two layers, and which
        // one fires depends on whether the process-global builtin-signature
        // registry was already populated (by any prior `register_vm_stdlib`
        // call in the test binary) when `compile_source` ran:
        //   - empty registry  -> the harness runtime guard (`string_arg`)
        //   - populated registry -> static type-check at compile time, which
        //     matches the `read_text` method against the same-named stdlib
        //     `read_text(path: string)` signature.
        // Both correctly reject the int, so accept either message.
        let runtime_rejection =
            error.contains("HarnessFs.read_text expects string argument 1, got int");
        let static_rejection = error.contains("argument 1 `path`: expected string, found int");
        assert!(
            runtime_rejection || static_rejection,
            "expected a string/int type rejection for read_text, got: {error}"
        );
    }

    #[test]
    fn real_harness_fs_write_outside_workspace_roots_surfaces_cap_201() {
        use crate::orchestration::{
            clear_execution_policy_stacks, push_execution_policy, CapabilityPolicy, SandboxProfile,
        };
        clear_execution_policy_stacks();
        let temp = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![temp.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        push_execution_policy(policy);
        let outside = std::env::temp_dir().join("harn_e4_4_cap_201_outside.txt");
        let source = format!(
            r#"fn main(harness: Harness) {{ harness.fs.write_text("{}", "x") }}"#,
            outside.to_string_lossy().replace('\\', "/"),
        );
        let error = run_harness_source(&source, Harness::real())
            .expect_err("write outside workspace_roots must reject");
        clear_execution_policy_stacks();
        assert!(
            error.contains("HARN-CAP-201"),
            "expected HARN-CAP-201 prefix, got: {error}"
        );
        assert!(
            error.contains("sandbox violation"),
            "deny should keep the underlying sandbox-rejection message, got: {error}"
        );
    }

    #[test]
    fn runtime_effect_receipt_comes_from_the_capability_contract() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effects = rt.block_on(async {
            let source = r#"fn main(harness: Harness) { harness.stdio.println("record me") }"#;
            harn_builtin_registry::install_builtin_manifest(crate::stdlib::all_builtin_manifest());
            let chunk = crate::compile_source(source).expect("compile");
            let mut vm = crate::Vm::new();
            crate::stdlib::register_vm_stdlib(&mut vm);
            vm.set_harness(Harness::mock().build());
            vm.execute(&chunk).await.expect("execute");
            vm.executed_effects()
        });

        assert_eq!(
            effects,
            vec![crate::orchestration::EffectRecord::new(
                crate::orchestration::EffectKind::Stdio,
                crate::orchestration::EffectScope::Write,
            )
            .with_resource("stdout")]
        );
    }

    fn run_harness_source(source: &str, harness: Harness) -> Result<String, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    let chunk = crate::compile_source(source)?;
                    let mut vm = crate::Vm::new();
                    crate::stdlib::register_vm_stdlib(&mut vm);
                    vm.set_harness(harness);
                    vm.execute(&chunk)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(vm.output().to_string())
                })
                .await
        })
    }
}
