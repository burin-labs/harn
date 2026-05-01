use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::sync::Once;
use std::thread;
use std::time::Duration;

use futures::StreamExt;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use tokio::sync::oneshot;

use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
use crate::llm::mock::{get_llm_mock_calls, push_llm_mock, LlmMock};
use crate::register_vm_stdlib;
use crate::triggers::event::{GitHubEventPayload, KnownProviderPayload};
use crate::triggers::registry::{
    install_manifest_triggers, resolve_live_trigger_binding, TriggerBindingSource,
    TriggerBindingSpec, TriggerHandlerSpec, TriggerPredicateSpec,
};
use crate::triggers::test_util::timing::{
    FILE_WATCH_FALLBACK_POLL, PROCESS_EXIT_GRACE, TEST_DEFAULT_TIMEOUT,
};
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TraceId, TriggerEvent};
use crate::TriggerPredicateBudget;
use crate::Vm;

use super::retry::TriggerRetryConfig;
use super::uri::{DispatchUri, DispatchUriError};
use super::{
    append_dispatch_cancel_request, install_test_inbox_dequeued_signal,
    install_test_inbox_subscribed_signal, AcquiredFlowControl, DispatchCancelRequest,
    DispatchStatus, DispatchWaitLease, Dispatcher, DispatcherRuntimeState, RetryPolicy,
    SingletonLease, DEFAULT_AUTONOMY_BUDGET_REVIEWER,
};

mod fixtures;
use fixtures::*;

mod dispatch;
mod flow_control;
mod predicate;
mod retry;
