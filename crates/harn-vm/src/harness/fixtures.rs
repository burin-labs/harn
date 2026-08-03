// Capability fixture state backing `harness.testing.*`.
//
// A fixture is authority to return a canned answer for one capability member,
// scoped to a push/pop pair so nested test scopes cannot leak responses into
// each other. Included from `harness.rs`, which owns the `Harness` value these
// fixtures hang off.

#[derive(Debug, Default)]
pub(crate) struct CapabilityFixtureState {
    inner: Mutex<CapabilityFixtureScopes>,
    http_mocks: crate::http::HttpMockRegistry,
}

#[derive(Debug, Default)]
struct CapabilityFixtureScopes {
    current: CapabilityFixtureInner,
    stack: Vec<CapabilityFixtureInner>,
}

#[derive(Debug, Default, Clone)]
struct CapabilityFixtureInner {
    enabled: bool,
    responses: BTreeMap<(String, String), VecDeque<CapabilityFixtureResponse>>,
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
    pub(crate) capability: String,
    pub(crate) member: String,
    pub(crate) args: Vec<crate::VmValue>,
    pub(crate) host_operation: bool,
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
        drop(scopes);
        crate::stdlib::host::fixtured_operations::clear_fixtured_host_operations();
        self.http_mocks.clear();
    }

    pub(crate) fn http_mocks(&self) -> &crate::http::HttpMockRegistry {
        &self.http_mocks
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
        crate::stdlib::host::fixtured_operations::push_fixtured_host_operation_scope();
    }

    pub(crate) fn pop_scope(&self) -> Result<(), crate::VmError> {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        let Some(previous) = scopes.stack.pop() else {
            return Err(crate::VmError::Runtime(
                "HarnessTesting.pop_scope called without a matching push_scope".to_string(),
            ));
        };
        scopes.current = previous;
        crate::stdlib::host::fixtured_operations::pop_fixtured_host_operation_scope();
        Ok(())
    }

    pub(crate) fn respond(
        &self,
        capability: &str,
        member: &str,
        response: Result<crate::VmValue, String>,
        when: Option<crate::value::DictMap>,
        repeat: bool,
    ) {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        scopes.current.enabled = true;
        // A fixtured operation must also be visible to `host_has`, or scripts
        // that gate their host call on the capability manifest skip the call
        // and never reach the fixture.
        crate::stdlib::host::fixtured_operations::record_fixtured_host_operation(capability, member);
        scopes
            .current
            .responses
            .entry((capability.to_string(), member.to_string()))
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
        self.dispatch_target(capability.field_name(), method, args, false)
    }

    pub(crate) fn dispatch_host(
        &self,
        capability: &str,
        operation: &str,
        params: &crate::value::DictMap,
    ) -> Option<Result<crate::VmValue, crate::VmError>> {
        self.dispatch_target(
            capability,
            operation,
            &[crate::VmValue::dict(params.clone())],
            true,
        )
    }

    fn dispatch_target(
        &self,
        capability: &str,
        member: &str,
        args: &[crate::VmValue],
        host_operation: bool,
    ) -> Option<Result<crate::VmValue, crate::VmError>> {
        let mut scopes = self.inner.lock().expect("capability fixtures poisoned");
        if !scopes.current.enabled {
            return None;
        }
        let key = (capability.to_string(), member.to_string());
        if !scopes.current.responses.contains_key(&key) {
            return None;
        }
        scopes.current.calls.push(CapabilityFixtureCall {
            capability: capability.to_string(),
            member: member.to_string(),
            args: args.to_vec(),
            host_operation,
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
                "no fixture for {capability}.{member} matched arguments {}",
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
