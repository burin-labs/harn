use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::value::{DeadlockError, VmChannelCloseState, VmChannelHandle, VmError};
use crate::VmValue;

#[derive(Debug, Default)]
pub(crate) struct VmWaitForGraph {
    inner: Mutex<WaitForState>,
}

#[derive(Debug, Default)]
struct WaitForState {
    tasks: BTreeMap<String, TaskEntry>,
    next_token: u64,
}

#[derive(Debug, Default)]
struct TaskEntry {
    active_count: usize,
    wait: Option<WaitRecord>,
}

#[derive(Debug, Clone)]
struct WaitRecord {
    token: u64,
    kind: WaitKind,
}

#[derive(Debug, Clone)]
enum WaitKind {
    Tasks(BTreeSet<String>),
    ChannelSend(ChannelTarget),
    ChannelReceive(Vec<ChannelTarget>),
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelTarget {
    id: String,
    name: String,
    sender: Arc<tokio::sync::mpsc::Sender<VmValue>>,
    _receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
    close: Arc<VmChannelCloseState>,
}

#[derive(Debug, Clone, Copy)]
enum ChannelDirection {
    Send,
    Receive,
}

#[derive(Debug)]
pub(crate) struct TaskActivityGuard {
    graph: Arc<VmWaitForGraph>,
    task_id: String,
}

#[derive(Debug)]
pub(crate) struct WaitGuard {
    graph: Arc<VmWaitForGraph>,
    task_id: String,
    token: u64,
    previous: Option<WaitRecord>,
}

impl VmWaitForGraph {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_task(self: &Arc<Self>, task_id: impl Into<String>) -> TaskActivityGuard {
        let task_id = task_id.into();
        let mut state = self.inner.lock().expect("wait-for graph mutex poisoned");
        state.tasks.entry(task_id.clone()).or_default().active_count += 1;
        TaskActivityGuard {
            graph: Arc::clone(self),
            task_id,
        }
    }

    pub(crate) fn wait_for_tasks(
        self: &Arc<Self>,
        task_id: &str,
        task_ids: impl IntoIterator<Item = String>,
    ) -> Result<WaitGuard, VmError> {
        self.set_wait(task_id, WaitKind::Tasks(task_ids.into_iter().collect()))
    }

    pub(crate) fn wait_for_channel_send(
        self: &Arc<Self>,
        task_id: &str,
        target: ChannelTarget,
    ) -> Result<WaitGuard, VmError> {
        self.set_wait(task_id, WaitKind::ChannelSend(target))
    }

    pub(crate) fn wait_for_channel_receive(
        self: &Arc<Self>,
        task_id: &str,
        targets: Vec<ChannelTarget>,
    ) -> Result<WaitGuard, VmError> {
        self.set_wait(task_id, WaitKind::ChannelReceive(targets))
    }

    pub(crate) fn notify_channel_send(self: &Arc<Self>, target: &ChannelTarget) {
        let mut state = self.inner.lock().expect("wait-for graph mutex poisoned");
        state.clear_channel_waits(|wait| match &wait.kind {
            WaitKind::ChannelReceive(targets) => {
                targets.iter().any(|waiting| waiting.id == target.id)
            }
            _ => false,
        });
    }

    pub(crate) fn notify_channel_receive(self: &Arc<Self>, target: &ChannelTarget) {
        let mut state = self.inner.lock().expect("wait-for graph mutex poisoned");
        state.clear_channel_waits(|wait| match &wait.kind {
            WaitKind::ChannelSend(waiting) => waiting.id == target.id,
            _ => false,
        });
    }

    fn set_wait(self: &Arc<Self>, task_id: &str, kind: WaitKind) -> Result<WaitGuard, VmError> {
        let mut state = self.inner.lock().expect("wait-for graph mutex poisoned");
        state.next_token = state.next_token.wrapping_add(1);
        let token = state.next_token;
        let entry = state.tasks.entry(task_id.to_string()).or_default();
        let previous = entry.wait.replace(WaitRecord { token, kind });
        if let Some(deadlock) = state.detect_deadlock() {
            let entry = state
                .tasks
                .get_mut(task_id)
                .expect("wait entry was just inserted");
            entry.wait = previous;
            return Err(VmError::Deadlock(Box::new(deadlock)));
        }
        Ok(WaitGuard {
            graph: Arc::clone(self),
            task_id: task_id.to_string(),
            token,
            previous,
        })
    }
}

impl WaitForState {
    fn detect_deadlock(&self) -> Option<DeadlockError> {
        let mut active_waits = Vec::new();
        for (task_id, entry) in self
            .tasks
            .iter()
            .filter(|(_, entry)| entry.active_count > 0)
        {
            let wait = entry.wait.as_ref()?;
            active_waits.push((task_id.as_str(), wait));
        }
        if active_waits.is_empty() {
            return None;
        }

        let mut waiting_sends = BTreeSet::new();
        let mut waiting_receives = BTreeSet::new();
        for (_, wait) in &active_waits {
            match &wait.kind {
                WaitKind::Tasks(_) => {}
                WaitKind::ChannelSend(target) => {
                    waiting_sends.insert(target.id.clone());
                }
                WaitKind::ChannelReceive(targets) => {
                    waiting_receives.extend(targets.iter().map(|target| target.id.clone()));
                }
            }
        }

        let mut blocked_channels = Vec::new();
        for (_, wait) in &active_waits {
            match &wait.kind {
                WaitKind::Tasks(task_ids) => {
                    if !self.task_wait_is_blocked_on_active_tasks(task_ids) {
                        return None;
                    }
                }
                WaitKind::ChannelSend(target) => {
                    if !target.send_is_blocked() {
                        return None;
                    }
                    if waiting_receives.contains(&target.id) {
                        return None;
                    }
                    blocked_channels.push((ChannelDirection::Send, target.clone()));
                }
                WaitKind::ChannelReceive(targets) => {
                    if targets.iter().any(|target| !target.receive_is_blocked()) {
                        return None;
                    }
                    if targets
                        .iter()
                        .any(|target| waiting_sends.contains(&target.id))
                    {
                        return None;
                    }
                    if let Some(target) = targets.first() {
                        blocked_channels.push((ChannelDirection::Receive, target.clone()));
                    }
                }
            }
        }

        channel_deadlock_error(blocked_channels)
    }

    fn task_wait_is_blocked_on_active_tasks(&self, task_ids: &BTreeSet<String>) -> bool {
        let mut has_active_target = false;
        for task_id in task_ids {
            let Some(entry) = self
                .tasks
                .get(task_id)
                .filter(|entry| entry.active_count > 0)
            else {
                continue;
            };
            has_active_target = true;
            if entry.wait.is_none() {
                return false;
            }
        }
        has_active_target
    }

    fn clear_channel_waits(&mut self, mut should_clear: impl FnMut(&WaitRecord) -> bool) {
        for entry in self.tasks.values_mut() {
            if entry.wait.as_ref().is_some_and(&mut should_clear) {
                entry.wait = None;
            }
        }
    }
}

impl Drop for TaskActivityGuard {
    fn drop(&mut self) {
        let mut state = self
            .graph
            .inner
            .lock()
            .expect("wait-for graph mutex poisoned");
        let Some(entry) = state.tasks.get_mut(&self.task_id) else {
            return;
        };
        entry.active_count = entry.active_count.saturating_sub(1);
        if entry.active_count == 0 {
            state.tasks.remove(&self.task_id);
        }
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        let mut state = self
            .graph
            .inner
            .lock()
            .expect("wait-for graph mutex poisoned");
        let Some(entry) = state.tasks.get_mut(&self.task_id) else {
            return;
        };
        if entry
            .wait
            .as_ref()
            .is_some_and(|record| record.token == self.token)
        {
            entry.wait = self.previous.take();
        }
    }
}

pub(crate) fn channel_target(channel: &VmChannelHandle) -> ChannelTarget {
    ChannelTarget {
        id: format!("{:p}", Arc::as_ptr(&channel.sender)),
        name: channel.name.to_string(),
        sender: channel.sender.clone(),
        _receiver: channel.receiver.clone(),
        close: channel.close.clone(),
    }
}

fn channel_deadlock_error(
    blocked_channels: Vec<(ChannelDirection, ChannelTarget)>,
) -> Option<DeadlockError> {
    let [(direction, target)] = blocked_channels.as_slice() else {
        return (!blocked_channels.is_empty()).then(|| {
            DeadlockError::wait_for_graph(
                "channel",
                "multiple channels",
                "all active tasks are waiting on channel operations with no matching send or receive",
            )
        });
    };
    let channel_name = target.display_name();
    let detail = match direction {
        ChannelDirection::Send => {
            format!(
                "all active tasks are waiting; send on channel '{channel_name}' has no receiver"
            )
        }
        ChannelDirection::Receive => {
            format!(
                "all active tasks are waiting; receive on channel '{channel_name}' has no sender"
            )
        }
    };
    Some(DeadlockError::wait_for_graph(
        "channel",
        channel_name,
        detail,
    ))
}

impl ChannelTarget {
    fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    fn buffered_len(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }

    fn send_is_blocked(&self) -> bool {
        !self.close.is_closed() && !self.sender.is_closed() && self.sender.capacity() == 0
    }

    fn receive_is_blocked(&self) -> bool {
        !self.close.is_closed() && !self.sender.is_closed() && self.buffered_len() == 0
    }
}

impl PartialEq for ChannelTarget {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ChannelTarget {}

impl PartialOrd for ChannelTarget {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for ChannelTarget {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.id.cmp(&other.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> ChannelTarget {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        ChannelTarget {
            id: name.to_string(),
            name: name.to_string(),
            sender: Arc::new(sender),
            _receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            close: Arc::new(VmChannelCloseState::open()),
        }
    }

    #[test]
    fn channel_receive_deadlock_requires_every_active_task_to_wait() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _root = graph.register_task("root");
        let _child = graph.register_task("child");
        let _root_wait = graph
            .wait_for_tasks("root", ["child".to_string()])
            .expect("running child can still make progress");

        let err = graph
            .wait_for_channel_receive("child", vec![target("empty")])
            .unwrap_err();

        assert!(err.to_string().contains("HARN-ORC-012"));
        assert!(err
            .to_string()
            .contains("receive on channel 'empty' has no sender"));
    }

    #[test]
    fn runnable_task_prevents_channel_deadlock_report() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _root = graph.register_task("root");
        let _child = graph.register_task("child");

        graph
            .wait_for_channel_receive("child", vec![target("empty")])
            .expect("root can still send later");
    }

    #[test]
    fn complementary_channel_waits_can_make_progress() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _sender = graph.register_task("sender");
        let _receiver = graph.register_task("receiver");
        let _send = graph
            .wait_for_channel_send("sender", target("handoff"))
            .expect("receiver has not parked yet");

        graph
            .wait_for_channel_receive("receiver", vec![target("handoff")])
            .expect("matching send and receive can rendezvous");
    }

    #[test]
    fn channel_send_notification_clears_receive_wait() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _root = graph.register_task("root");
        let _receiver = graph.register_task("receiver");
        let target = target("ready");
        let _receive = graph
            .wait_for_channel_receive("receiver", vec![target.clone()])
            .expect("root can still send");

        graph.notify_channel_send(&target);

        graph
            .wait_for_tasks("root", ["receiver".to_string()])
            .expect("receiver wait was cleared by the matching send");
    }

    #[test]
    fn channel_receive_notification_clears_send_wait() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _root = graph.register_task("root");
        let _sender = graph.register_task("sender");
        let target = target("drained");
        target
            .sender
            .try_send(VmValue::Nil)
            .expect("test channel starts with one free slot");
        let _send = graph
            .wait_for_channel_send("sender", target.clone())
            .expect("root can still receive");

        graph.notify_channel_receive(&target);

        graph
            .wait_for_tasks("root", ["sender".to_string()])
            .expect("sender wait was cleared by the matching receive");
    }
}
