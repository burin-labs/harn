use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::oneshot;

use crate::event_log::{install_memory_for_current_thread, EventLog, Topic};
use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
use crate::llm::mock::{get_llm_mock_calls, push_llm_mock, LlmMock};
use crate::register_vm_stdlib;
use crate::triggers::event::{GitHubEventPayload, KnownProviderPayload};
use crate::triggers::registry::{
    install_manifest_triggers, resolve_live_trigger_binding, TriggerBindingSource,
    TriggerBindingSpec, TriggerHandlerSpec, TriggerPredicateSpec,
};
use crate::triggers::test_util::timing::TEST_DEFAULT_TIMEOUT;
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TraceId, TriggerEvent};
use crate::TriggerPredicateBudget;
use crate::Vm;

fn install_test_event_log() -> Arc<crate::event_log::AnyEventLog> {
    install_memory_for_current_thread(
        crate::runtime_limits::RuntimeLimits::DEFAULT.default_event_log_queue_depth,
    )
}

use super::retry::TriggerRetryConfig;
use super::uri::{DispatchUri, DispatchUriError};
use super::{
    append_dispatch_cancel_request, install_test_inbox_dequeued_signal, AcquiredFlowControl,
    DispatchCancelRequest, DispatchStatus, DispatchWaitLease, Dispatcher, DispatcherRuntimeState,
    RetryPolicy, SingletonLease, DEFAULT_AUTONOMY_BUDGET_REVIEWER,
};

mod fixtures;
use fixtures::*;

mod dispatch;
mod flow_control;
mod lazy_callable;
mod predicate;
mod retry;
