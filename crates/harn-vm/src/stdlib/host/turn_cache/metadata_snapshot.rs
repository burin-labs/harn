//! Typed, per-directory metadata snapshots for the turn cache.
//!
//! A host's namespace-free `project.metadata_get({ dir })` response is the
//! authoritative directory snapshot. Namespace reads project from that one
//! value, so context branches do not pay one host round-trip per namespace.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::value::{intern_key, DictMap, VmError, VmValue};

#[derive(Clone, Debug)]
struct MetadataRequest {
    directory: String,
    namespace: Option<String>,
}

impl MetadataRequest {
    fn parse(params: &DictMap) -> Result<Self, VmError> {
        let directory = match params.get("dir") {
            None | Some(VmValue::Nil) => String::new(),
            Some(VmValue::String(value)) => normalize_directory(value.as_str()),
            Some(value) => {
                return Err(VmError::TypeError(format!(
                    "project.metadata_get: dir must be string or nil, got {}",
                    value.type_name()
                )))
            }
        };
        let namespace = match params.get("namespace") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::String(value)) if value.is_empty() => None,
            Some(VmValue::String(value)) => Some(value.to_string()),
            Some(value) => {
                return Err(VmError::TypeError(format!(
                    "project.metadata_get: namespace must be string or nil, got {}",
                    value.type_name()
                )))
            }
        };
        Ok(Self {
            directory,
            namespace,
        })
    }

    fn bulk_params(&self) -> DictMap {
        DictMap::from_iter([(
            intern_key("dir"),
            VmValue::String(arcstr::ArcStr::from(self.directory.as_str())),
        )])
    }
}

/// Canonicalize wire aliases without confusing the legitimate root directory
/// (`""`) with an absent or failed result.
fn normalize_directory(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let absolute = normalized.starts_with('/');
    let components: Vec<&str> = normalized
        .split('/')
        .filter(|component| !matches!(*component, "" | "."))
        .collect();
    let joined = components.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `None` means the host successfully reported no directory metadata;
/// `Some(empty)` is a successful, present-but-empty snapshot. Transport errors
/// never become this type and therefore cannot masquerade as either state.
#[derive(Clone, Debug)]
struct MetadataSnapshot {
    namespaces: Option<DictMap>,
}

impl MetadataSnapshot {
    fn parse(value: VmValue) -> Result<Self, VmError> {
        let namespaces = match value {
            VmValue::Nil => None,
            VmValue::Dict(namespaces) => {
                for (namespace, fields) in namespaces.iter() {
                    if fields.as_dict().is_none() {
                        return Err(VmError::TypeError(format!(
                            "project.metadata_get: namespace {namespace:?} must contain a dict, got {}",
                            fields.type_name()
                        )));
                    }
                }
                Some((*namespaces).clone())
            }
            other => {
                return Err(VmError::TypeError(format!(
                    "project.metadata_get: bulk host result must be dict or nil, got {}",
                    other.type_name()
                )))
            }
        };
        Ok(Self { namespaces })
    }

    fn project(&self, namespace: Option<&str>) -> VmValue {
        match (&self.namespaces, namespace) {
            (None, _) => VmValue::Nil,
            (Some(namespaces), None) => VmValue::dict(namespaces.clone()),
            (Some(namespaces), Some(namespace)) => {
                namespaces.get(namespace).cloned().unwrap_or(VmValue::Nil)
            }
        }
    }
}

type LoadResult = Result<Option<Arc<MetadataSnapshot>>, VmError>;

struct MetadataLoad {
    result: watch::Sender<Option<LoadResult>>,
}

impl MetadataLoad {
    fn new() -> Self {
        let (result, _) = watch::channel(None);
        Self { result }
    }

    async fn wait(&self) -> LoadResult {
        let mut receiver = self.result.subscribe();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| {
                VmError::Runtime("project.metadata_get: shared host read was canceled".to_string())
            })?;
        }
    }

    fn complete(&self, result: LoadResult) {
        self.result.send_replace(Some(result));
    }
}

enum MetadataCacheEntry {
    Ready {
        epoch: u64,
        snapshot: Arc<MetadataSnapshot>,
    },
    Loading {
        epoch: u64,
        load: Arc<MetadataLoad>,
    },
}

thread_local! {
    static METADATA_SNAPSHOTS: RefCell<HashMap<String, MetadataCacheEntry>> =
        RefCell::new(HashMap::new());
}

enum Admission {
    Ready(Arc<MetadataSnapshot>),
    Wait(Arc<MetadataLoad>),
    Lead(Arc<MetadataLoad>),
}

struct LeadGuard {
    directory: String,
    epoch: u64,
    load: Arc<MetadataLoad>,
    completed: bool,
}

impl LeadGuard {
    fn complete(mut self, result: LoadResult) {
        self.load.complete(result);
        self.completed = true;
    }
}

impl Drop for LeadGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        METADATA_SNAPSHOTS.with(|cache| {
            let mut cache = cache.borrow_mut();
            let is_this_load = matches!(
                cache.get(&self.directory),
                Some(MetadataCacheEntry::Loading { epoch, load })
                    if *epoch == self.epoch && Arc::ptr_eq(load, &self.load)
            );
            if is_this_load {
                cache.remove(&self.directory);
            }
        });
        self.load.complete(Err(VmError::Runtime(
            "project.metadata_get: shared host read was canceled".to_string(),
        )));
    }
}

pub(super) async fn cached_or<F, Fut>(
    params: &DictMap,
    epoch: u64,
    dispatch: F,
) -> Result<Option<VmValue>, VmError>
where
    F: FnOnce(DictMap) -> Fut,
    Fut: std::future::Future<Output = Result<Option<VmValue>, VmError>>,
{
    let request = MetadataRequest::parse(params)?;
    let admission = METADATA_SNAPSHOTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        match cache.get(&request.directory) {
            Some(MetadataCacheEntry::Ready {
                epoch: written,
                snapshot,
            }) if *written == epoch => Admission::Ready(snapshot.clone()),
            Some(MetadataCacheEntry::Loading {
                epoch: started,
                load,
            }) if *started == epoch => Admission::Wait(load.clone()),
            _ => {
                let load = Arc::new(MetadataLoad::new());
                cache.insert(
                    request.directory.clone(),
                    MetadataCacheEntry::Loading {
                        epoch,
                        load: load.clone(),
                    },
                );
                Admission::Lead(load)
            }
        }
    });

    let snapshot = match admission {
        Admission::Ready(snapshot) => Some(snapshot),
        Admission::Wait(load) => load.wait().await?,
        Admission::Lead(load) => {
            let guard = LeadGuard {
                directory: request.directory.clone(),
                epoch,
                load: load.clone(),
                completed: false,
            };
            let result = match dispatch(request.bulk_params()).await {
                Ok(Some(value)) => MetadataSnapshot::parse(value).map(Arc::new).map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            if let Ok(Some(snapshot)) = &result {
                if super::current_epoch() == epoch {
                    METADATA_SNAPSHOTS.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        let is_this_load = matches!(
                            cache.get(&request.directory),
                            Some(MetadataCacheEntry::Loading { epoch: started, load: active })
                                if *started == epoch && Arc::ptr_eq(active, &load)
                        );
                        if is_this_load {
                            cache.insert(
                                request.directory.clone(),
                                MetadataCacheEntry::Ready {
                                    epoch,
                                    snapshot: snapshot.clone(),
                                },
                            );
                        }
                    });
                }
            } else {
                METADATA_SNAPSHOTS.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    let is_this_load = matches!(
                        cache.get(&request.directory),
                        Some(MetadataCacheEntry::Loading { epoch: started, load: active })
                            if *started == epoch && Arc::ptr_eq(active, &load)
                    );
                    if is_this_load {
                        cache.remove(&request.directory);
                    }
                });
            }
            guard.complete(result.clone());
            result?
        }
    };
    Ok(snapshot.map(|snapshot| snapshot.project(request.namespace.as_deref())))
}

pub(super) fn lookup(params: &DictMap, epoch: u64) -> Option<VmValue> {
    let request = MetadataRequest::parse(params).ok()?;
    METADATA_SNAPSHOTS.with(|cache| match cache.borrow().get(&request.directory) {
        Some(MetadataCacheEntry::Ready {
            epoch: written,
            snapshot,
        }) if *written == epoch => Some(snapshot.project(request.namespace.as_deref())),
        _ => None,
    })
}

pub(super) fn store(params: &DictMap, value: &VmValue, epoch: u64) {
    let Ok(request) = MetadataRequest::parse(params) else {
        return;
    };
    // A namespace projection is not a complete directory snapshot and cannot
    // safely answer sibling reads.
    if request.namespace.is_some() {
        return;
    }
    let Ok(snapshot) = MetadataSnapshot::parse(value.clone()) else {
        return;
    };
    METADATA_SNAPSHOTS.with(|cache| {
        cache.borrow_mut().insert(
            request.directory,
            MetadataCacheEntry::Ready {
                epoch,
                snapshot: Arc::new(snapshot),
            },
        );
    });
}

pub(super) fn reset_local() {
    METADATA_SNAPSHOTS.with(|cache| cache.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{cached_or, normalize_directory};
    use crate::value::{intern_key, DictMap, VmError, VmValue};

    fn params(directory: &str, namespace: Option<&str>) -> DictMap {
        let mut params = DictMap::from_iter([(
            intern_key("dir"),
            VmValue::String(arcstr::ArcStr::from(directory)),
        )]);
        if let Some(namespace) = namespace {
            params.insert(
                intern_key("namespace"),
                VmValue::String(arcstr::ArcStr::from(namespace)),
            );
        }
        params
    }

    fn snapshot() -> VmValue {
        VmValue::dict(DictMap::from_iter([
            (
                intern_key("facts"),
                VmValue::dict(DictMap::from_iter([(
                    intern_key("owner"),
                    VmValue::String(arcstr::ArcStr::from("facts-owner")),
                )])),
            ),
            (
                intern_key("test"),
                VmValue::dict(DictMap::from_iter([(
                    intern_key("owner"),
                    VmValue::String(arcstr::ArcStr::from("test-owner")),
                )])),
            ),
        ]))
    }

    fn run_local<F>(future: F)
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(tokio::task::LocalSet::new().run_until(future));
    }

    #[test]
    fn directory_wire_aliases_have_one_identity_without_losing_root() {
        assert_eq!(normalize_directory(""), "");
        assert_eq!(normalize_directory("."), "");
        assert_eq!(normalize_directory("./"), "");
        assert_eq!(normalize_directory("src//nested/./"), "src/nested");
        assert_eq!(
            normalize_directory("src/child/../nested"),
            "src/child/../nested"
        );
        assert_eq!(normalize_directory("../outside"), "../outside");
        assert_eq!(normalize_directory("/"), "/");
        assert_eq!(normalize_directory("/workspace/./src"), "/workspace/src");
        assert_eq!(
            normalize_directory(" dir with spaces "),
            " dir with spaces "
        );
    }

    #[test]
    fn concurrent_namespace_reads_singleflight_one_bulk_snapshot() {
        let _guard = super::super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        super::super::reset();
        let epoch = super::super::current_epoch();
        let calls = Arc::new(AtomicUsize::new(0));
        run_local(async move {
            let facts_params = params("./src/nested/", Some("facts"));
            let test_params = params("src//nested", Some("test"));
            let facts_calls = calls.clone();
            let facts = cached_or(&facts_params, epoch, move |bulk| async move {
                assert_eq!(
                    bulk.get("dir").map(VmValue::display).as_deref(),
                    Some("src/nested")
                );
                assert!(bulk.get("namespace").is_none());
                facts_calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(Some(snapshot()))
            });
            let test_calls = calls.clone();
            let test = cached_or(&test_params, epoch, move |_| async move {
                test_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(snapshot()))
            });

            let (facts, test) = tokio::join!(facts, test);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                facts
                    .expect("facts projection")
                    .expect("handled")
                    .as_dict()
                    .and_then(|fields| fields.get("owner"))
                    .map(VmValue::display)
                    .as_deref(),
                Some("facts-owner")
            );
            assert_eq!(
                test.expect("test projection")
                    .expect("handled")
                    .as_dict()
                    .and_then(|fields| fields.get("owner"))
                    .map(VmValue::display)
                    .as_deref(),
                Some("test-owner")
            );
        });
    }

    #[test]
    fn empty_absent_malformed_and_transport_states_remain_distinct() {
        let _guard = super::super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        run_local(async {
            let root = params("", None);

            super::super::reset();
            let epoch = super::super::current_epoch();
            let empty = cached_or(&root, epoch, |_| async {
                Ok(Some(VmValue::dict(DictMap::new())))
            })
            .await
            .expect("successful empty snapshot")
            .expect("bridge handled");
            assert!(empty.as_dict().is_some_and(DictMap::is_empty));
            let warm_empty = cached_or(&params(".", None), epoch, |_| async {
                panic!("root alias should hit the successful empty snapshot")
            })
            .await
            .expect("warm empty snapshot")
            .expect("bridge handled");
            assert!(warm_empty.as_dict().is_some_and(DictMap::is_empty));

            super::super::reset();
            let epoch = super::super::current_epoch();
            let absent = cached_or(&root, epoch, |_| async { Ok(Some(VmValue::Nil)) })
                .await
                .expect("successful absence")
                .expect("bridge handled");
            assert!(matches!(absent, VmValue::Nil));
            let warm_absent = cached_or(&root, epoch, |_| async {
                panic!("successful absence should be cached")
            })
            .await
            .expect("warm absence")
            .expect("bridge handled");
            assert!(matches!(warm_absent, VmValue::Nil));

            super::super::reset();
            let epoch = super::super::current_epoch();
            let calls = Arc::new(AtomicUsize::new(0));
            let first_calls = calls.clone();
            let malformed = cached_or(&root, epoch, move |_| async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(VmValue::String(arcstr::ArcStr::from(
                    "not-a-snapshot",
                ))))
            })
            .await;
            assert!(matches!(malformed, Err(VmError::TypeError(_))));
            let second_calls = calls.clone();
            cached_or(&root, epoch, move |_| async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(snapshot()))
            })
            .await
            .expect("malformed result must leave a cold cache");
            assert_eq!(calls.load(Ordering::SeqCst), 2);

            super::super::reset();
            let epoch = super::super::current_epoch();
            let calls = Arc::new(AtomicUsize::new(0));
            let first_calls = calls.clone();
            let failed = cached_or(&root, epoch, move |_| async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Err(VmError::Runtime("transport failed".to_string()))
            })
            .await;
            assert!(matches!(failed, Err(VmError::Runtime(_))));
            let second_calls = calls.clone();
            cached_or(&root, epoch, move |_| async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(snapshot()))
            })
            .await
            .expect("transport failure must leave a cold cache");
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn canceled_leader_releases_waiters_and_allows_retry() {
        let _guard = super::super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        super::super::reset();
        let epoch = super::super::current_epoch();
        run_local(async move {
            let request = params("src", Some("facts"));
            let started = Arc::new(tokio::sync::Notify::new());
            let started_wait = started.notified();
            let leader_started = started.clone();
            let leader_params = request.clone();
            let leader = tokio::task::spawn_local(async move {
                cached_or(&leader_params, epoch, move |_| async move {
                    leader_started.notify_one();
                    std::future::pending::<Result<Option<VmValue>, VmError>>().await
                })
                .await
            });
            started_wait.await;

            let unexpected_dispatches = Arc::new(AtomicUsize::new(0));
            let waiter_dispatches = unexpected_dispatches.clone();
            let waiter_params = request.clone();
            let waiter = tokio::task::spawn_local(async move {
                cached_or(&waiter_params, epoch, move |_| async move {
                    waiter_dispatches.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(snapshot()))
                })
                .await
            });
            tokio::task::yield_now().await;
            assert_eq!(unexpected_dispatches.load(Ordering::SeqCst), 0);
            leader.abort();
            assert!(leader.await.is_err());
            assert!(matches!(
                waiter.await.expect("waiter task"),
                Err(VmError::Runtime(_))
            ));

            let retried = cached_or(&request, epoch, |_| async { Ok(Some(snapshot())) })
                .await
                .expect("retry after cancellation")
                .expect("bridge handled");
            assert!(retried.as_dict().is_some());
        });
    }
}
