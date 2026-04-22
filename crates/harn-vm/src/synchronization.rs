use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::value::{VmError, VmSyncPermitHandle, VmValue};

#[derive(Debug, Default)]
pub(crate) struct VmSyncRuntime {
    primitives: Mutex<BTreeMap<String, Arc<VmSyncPrimitive>>>,
}

#[derive(Debug)]
pub(crate) struct VmSyncPrimitive {
    kind: String,
    key: String,
    capacity: u32,
    semaphore: Arc<Semaphore>,
    metrics: Mutex<VmSyncMetrics>,
}

#[derive(Debug, Default, Clone)]
struct VmSyncMetrics {
    acquisition_count: u64,
    timeout_count: u64,
    cancellation_count: u64,
    release_count: u64,
    current_held: u64,
    current_queue_depth: u64,
    max_queue_depth: u64,
    total_wait_ms: u64,
    total_held_ms: u64,
}

#[derive(Debug)]
pub(crate) struct VmSyncLease {
    kind: String,
    key: String,
    permits: u32,
    acquired_at: Instant,
    primitive: Arc<VmSyncPrimitive>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    released: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct VmSyncHeldGuard {
    pub(crate) _permit: VmSyncPermitHandle,
    pub(crate) frame_depth: usize,
    pub(crate) env_scope_depth: usize,
}

impl VmSyncRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn primitive(
        &self,
        kind: &str,
        key: &str,
        capacity: u32,
    ) -> Result<Arc<VmSyncPrimitive>, VmError> {
        let id = format!("{kind}:{key}");
        let mut primitives = self
            .primitives
            .lock()
            .expect("sync primitive registry mutex poisoned");
        if let Some(existing) = primitives.get(&id) {
            if existing.capacity != capacity {
                return Err(VmError::Runtime(format!(
                    "sync {kind} '{key}' already exists with capacity {}, not {capacity}",
                    existing.capacity
                )));
            }
            return Ok(existing.clone());
        }
        let primitive = Arc::new(VmSyncPrimitive {
            kind: kind.to_string(),
            key: key.to_string(),
            capacity,
            semaphore: Arc::new(Semaphore::new(capacity as usize)),
            metrics: Mutex::new(VmSyncMetrics::default()),
        });
        primitives.insert(id, primitive.clone());
        Ok(primitive)
    }

    pub(crate) async fn acquire(
        &self,
        kind: &str,
        key: &str,
        capacity: u32,
        permits: u32,
        timeout_ms: Option<u64>,
        cancel_token: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Option<VmSyncPermitHandle>, VmError> {
        if permits == 0 {
            return Err(VmError::Runtime(format!(
                "sync {kind} '{key}' requires at least one permit"
            )));
        }
        if permits > capacity {
            return Err(VmError::Runtime(format!(
                "sync {kind} '{key}' requested {permits} permits from capacity {capacity}"
            )));
        }
        if cancel_token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            let primitive = self.primitive(kind, key, capacity)?;
            primitive.record_cancel();
            return Err(cancelled_vm_error());
        }

        let primitive = self.primitive(kind, key, capacity)?;
        let started = Instant::now();

        if timeout_ms == Some(0) {
            return match primitive.semaphore.clone().try_acquire_many_owned(permits) {
                Ok(permit) => Ok(Some(primitive.lease(permit, permits, started.elapsed()))),
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    primitive.record_timeout();
                    Ok(None)
                }
                Err(tokio::sync::TryAcquireError::Closed) => Err(VmError::Runtime(format!(
                    "sync {kind} '{key}' semaphore is closed"
                ))),
            };
        }

        primitive.record_queued();
        let acquire = primitive.semaphore.clone().acquire_many_owned(permits);
        tokio::pin!(acquire);

        let result = if let Some(timeout_ms) = timeout_ms {
            let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
            tokio::pin!(timeout);
            let mut cancel_poll = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    permit = &mut acquire => break permit.map(Some),
                    _ = &mut timeout => break Ok(None),
                    _ = cancel_poll.tick(), if cancel_token.is_some() => {
                        if cancel_token
                            .as_ref()
                            .is_some_and(|token| token.load(Ordering::SeqCst))
                        {
                            primitive.record_dequeued();
                            primitive.record_cancel();
                            return Err(cancelled_vm_error());
                        }
                    }
                }
            }
        } else {
            let mut cancel_poll = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    permit = &mut acquire => break permit.map(Some),
                    _ = cancel_poll.tick(), if cancel_token.is_some() => {
                        if cancel_token
                            .as_ref()
                            .is_some_and(|token| token.load(Ordering::SeqCst))
                        {
                            primitive.record_dequeued();
                            primitive.record_cancel();
                            return Err(cancelled_vm_error());
                        }
                    }
                }
            }
        };

        primitive.record_dequeued();
        match result {
            Ok(Some(permit)) => Ok(Some(primitive.lease(permit, permits, started.elapsed()))),
            Ok(None) => {
                primitive.record_timeout();
                Ok(None)
            }
            Err(_) => Err(VmError::Runtime(format!(
                "sync {kind} '{key}' semaphore is closed"
            ))),
        }
    }

    pub(crate) fn metrics(&self, kind: Option<&str>, key: Option<&str>) -> VmValue {
        let primitives = self
            .primitives
            .lock()
            .expect("sync primitive registry mutex poisoned");
        if let (Some(kind), Some(key)) = (kind, key) {
            let id = format!("{kind}:{key}");
            return primitives
                .get(&id)
                .map(|primitive| primitive.metrics_dict())
                .unwrap_or(VmValue::Nil);
        }

        let mut rows = Vec::new();
        for primitive in primitives.values() {
            if kind.is_none_or(|wanted| wanted == primitive.kind)
                && key.is_none_or(|wanted| wanted == primitive.key)
            {
                rows.push(primitive.metrics_dict());
            }
        }
        VmValue::List(Rc::new(rows))
    }
}

impl VmSyncPrimitive {
    fn lease(
        self: &Arc<Self>,
        permit: OwnedSemaphorePermit,
        permits: u32,
        wait: Duration,
    ) -> VmSyncPermitHandle {
        {
            let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
            metrics.acquisition_count += 1;
            metrics.current_held += permits as u64;
            metrics.total_wait_ms += wait.as_millis() as u64;
        }
        VmSyncPermitHandle {
            lease: Arc::new(VmSyncLease {
                kind: self.kind.clone(),
                key: self.key.clone(),
                permits,
                acquired_at: Instant::now(),
                primitive: self.clone(),
                permit: Mutex::new(Some(permit)),
                released: AtomicBool::new(false),
            }),
        }
    }

    fn record_queued(&self) {
        let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
        metrics.current_queue_depth += 1;
        metrics.max_queue_depth = metrics.max_queue_depth.max(metrics.current_queue_depth);
    }

    fn record_dequeued(&self) {
        let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
        metrics.current_queue_depth = metrics.current_queue_depth.saturating_sub(1);
    }

    fn record_timeout(&self) {
        let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
        metrics.timeout_count += 1;
    }

    fn record_cancel(&self) {
        let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
        metrics.cancellation_count += 1;
    }

    fn record_release(&self, permits: u32, held: Duration) {
        let mut metrics = self.metrics.lock().expect("sync metrics mutex poisoned");
        metrics.release_count += 1;
        metrics.current_held = metrics.current_held.saturating_sub(permits as u64);
        metrics.total_held_ms += held.as_millis() as u64;
    }

    fn metrics_dict(&self) -> VmValue {
        let metrics = self
            .metrics
            .lock()
            .expect("sync metrics mutex poisoned")
            .clone();
        let mut dict = BTreeMap::new();
        dict.insert(
            "kind".to_string(),
            VmValue::String(Rc::from(self.kind.as_str())),
        );
        dict.insert(
            "key".to_string(),
            VmValue::String(Rc::from(self.key.as_str())),
        );
        dict.insert("capacity".to_string(), VmValue::Int(self.capacity as i64));
        dict.insert(
            "acquisition_count".to_string(),
            VmValue::Int(metrics.acquisition_count as i64),
        );
        dict.insert(
            "timeout_count".to_string(),
            VmValue::Int(metrics.timeout_count as i64),
        );
        dict.insert(
            "cancellation_count".to_string(),
            VmValue::Int(metrics.cancellation_count as i64),
        );
        dict.insert(
            "release_count".to_string(),
            VmValue::Int(metrics.release_count as i64),
        );
        dict.insert(
            "current_held".to_string(),
            VmValue::Int(metrics.current_held as i64),
        );
        dict.insert(
            "current_queue_depth".to_string(),
            VmValue::Int(metrics.current_queue_depth as i64),
        );
        dict.insert(
            "max_queue_depth".to_string(),
            VmValue::Int(metrics.max_queue_depth as i64),
        );
        dict.insert(
            "total_wait_ms".to_string(),
            VmValue::Int(metrics.total_wait_ms as i64),
        );
        dict.insert(
            "total_held_ms".to_string(),
            VmValue::Int(metrics.total_held_ms as i64),
        );
        VmValue::Dict(Rc::new(dict))
    }
}

impl VmSyncLease {
    pub(crate) fn release(&self) -> bool {
        if self.released.swap(true, Ordering::SeqCst) {
            return false;
        }
        let _permit = self
            .permit
            .lock()
            .expect("sync lease mutex poisoned")
            .take();
        self.primitive
            .record_release(self.permits, self.acquired_at.elapsed());
        true
    }

    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for VmSyncLease {
    fn drop(&mut self) {
        self.release();
    }
}

fn cancelled_vm_error() -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(
        "kind:cancelled:VM cancelled by host",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(cancelled: bool) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::new(std::sync::atomic::AtomicBool::new(cancelled))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mutex_acquire_timeout_is_deterministic() {
        let runtime = VmSyncRuntime::new();
        let _held = runtime
            .acquire("mutex", "t", 1, 1, None, None)
            .await
            .unwrap()
            .unwrap();

        let timed_out = runtime
            .acquire("mutex", "t", 1, 1, Some(0), None)
            .await
            .unwrap();

        assert!(timed_out.is_none());
        let metrics = runtime.metrics(Some("mutex"), Some("t"));
        let metrics = metrics.as_dict().unwrap();
        assert_eq!(metrics.get("timeout_count").unwrap().display(), "1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_observes_pre_cancelled_token() {
        let runtime = VmSyncRuntime::new();
        let err = runtime
            .acquire("mutex", "cancelled", 1, 1, None, Some(token(true)))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("kind:cancelled"));
        let metrics = runtime.metrics(Some("mutex"), Some("cancelled"));
        let metrics = metrics.as_dict().unwrap();
        assert_eq!(metrics.get("cancellation_count").unwrap().display(), "1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn semaphore_releases_capacity_on_drop() {
        let runtime = VmSyncRuntime::new();
        let held = runtime
            .acquire("semaphore", "pool", 2, 2, None, None)
            .await
            .unwrap()
            .unwrap();
        assert!(runtime
            .acquire("semaphore", "pool", 2, 1, Some(0), None)
            .await
            .unwrap()
            .is_none());

        drop(held);

        assert!(runtime
            .acquire("semaphore", "pool", 2, 1, Some(0), None)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gate_uses_fifo_admission() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let runtime = Arc::new(VmSyncRuntime::new());
                let held = runtime
                    .acquire("gate", "runner", 1, 1, None, None)
                    .await
                    .unwrap()
                    .unwrap();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

                for id in [1, 2] {
                    let runtime = runtime.clone();
                    let tx = tx.clone();
                    tokio::task::spawn_local(async move {
                        let permit = runtime
                            .acquire("gate", "runner", 1, 1, None, None)
                            .await
                            .unwrap()
                            .unwrap();
                        tx.send(id).unwrap();
                        drop(permit);
                    });
                }

                tokio::task::yield_now().await;
                drop(held);

                assert_eq!(rx.recv().await, Some(1));
                assert_eq!(rx.recv().await, Some(2));
            })
            .await;
    }
}
