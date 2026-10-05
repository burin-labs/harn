//! Send a selected parent store through the existing contained-process owner.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use harn_vm::secrets::{ParentSecretHandoff, PARENT_SECRET_HANDOFF_OPTION};

use super::{
    spawn_process, OutputCapture, ProcessCleanupReport, ProcessError, ProcessHandle, SpawnSpec,
    WaitOutcome,
};

/// A failed handoff keeps the owning process cleanup evidence inspectable.
#[derive(Debug, thiserror::Error)]
pub enum SecretHandoffSpawnError {
    /// The existing process owner refused or failed before transfer.
    #[error("{0}")]
    Spawn(#[from] ProcessError),
    /// Invalid channel ownership or an already-expired transfer.
    #[error("parent secret handoff refused: {reason}")]
    Refused {
        /// Value-free diagnostic naming the refusal.
        reason: &'static str,
    },
    /// The child started, but its parent store could not be handed over.
    #[error("parent secret handoff failed: {reason}")]
    Transfer {
        /// Value-free diagnostic naming the transfer failure.
        reason: &'static str,
        /// Original process-owner cleanup evidence, including unknown children.
        cleanup: Box<ProcessCleanupReport>,
        /// Bounded wait result after reclamation, never assumed successful.
        reap: Box<std::io::Result<WaitOutcome>>,
    },
}

/// Start Harn with a closed, process-local store. The explicit one-shot stdin
/// channel cannot coexist with an application stdin stream. No raw credential
/// is added to the process environment, argument vector or filesystem.
///
/// `timeout` bounds transfer rather than the subsequent application lifetime.
/// Cancellation and failed transfer reclaim through the existing process owner;
/// an unproven cleanup remains visible in the returned receipt.
pub fn spawn_harn_with_parent_secrets(
    mut spec: SpawnSpec,
    handoff: ParentSecretHandoff,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> Result<Box<dyn ProcessHandle>, SecretHandoffSpawnError> {
    if spec.use_stdin || matches!(spec.output_capture, OutputCapture::Inherit) {
        return Err(SecretHandoffSpawnError::Refused {
            reason: "stdin is already owned by the application",
        });
    }
    if timeout.is_zero() || interrupted() {
        return Err(SecretHandoffSpawnError::Refused {
            reason: "transfer deadline or cancellation reached before spawn",
        });
    }
    let option = format!("--{PARENT_SECRET_HANDOFF_OPTION}");
    if spec.args.iter().any(|arg| arg == &option) {
        return Err(SecretHandoffSpawnError::Refused {
            reason: "handoff option is owned by the process sender",
        });
    }
    spec.args.insert(0, option);
    spec.use_stdin = true;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(SecretHandoffSpawnError::Refused {
            reason: "invalid transfer deadline",
        })?;
    let mut child = spawn_process(spec)?;
    let Some(pipe) = child.take_stdin() else {
        return Err(failed_transfer(&mut *child, "child stdin pipe unavailable"));
    };
    let (sender, receiver) = mpsc::channel();
    // Blocking pipe writes must not hold the supervising caller indefinitely.
    // Closing/killing the existing containment releases an ordinary blocked pipe.
    let writer = std::thread::Builder::new()
        .name("harn-parent-secret-handoff".into())
        .spawn(move || {
            let result = handoff.write_to(pipe).is_ok();
            let _ = sender.send(result);
        });
    let Ok(writer) = writer else {
        return Err(failed_transfer(&mut *child, "cannot start pipe writer"));
    };
    loop {
        if interrupted() {
            return Err(failed_transfer(&mut *child, "transfer cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(failed_transfer(&mut *child, "transfer deadline reached"));
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(true) => {
                // Receipt is sent only after the write and stdin close complete.
                if writer.join().is_err() {
                    return Err(failed_transfer(&mut *child, "pipe writer failed"));
                }
                return Ok(child);
            }
            Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(failed_transfer(&mut *child, "frame delivery failed"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn failed_transfer(child: &mut dyn ProcessHandle, reason: &'static str) -> SecretHandoffSpawnError {
    let cleanup = child.killer().kill();
    let reap = child.wait_with_timeout(Some(Duration::ZERO), &|| false);
    SecretHandoffSpawnError::Transfer {
        reason,
        cleanup: Box::new(cleanup),
        reap: Box::new(reap),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{
        install_spawner, EnvMode, MockProcessConfig, MockSpawner, OwnerDeathPolicy,
    };
    use harn_vm::secrets::{MemorySecretProvider, SecretId, SecretProvider};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            builtin: "parent-secret-handoff-test",
            program: "harn".into(),
            args: vec!["run".into(), "probe.harn".into()],
            cwd: None,
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: false,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        }
    }

    async fn handoff() -> ParentSecretHandoff {
        let id = SecretId::new("fixture", "provider");
        let parent = MemorySecretProvider::new("parent").with_secret(id.clone(), b"inert-canary");
        ParentSecretHandoff::capture(&parent, [id]).await.unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sender_uses_the_owned_pipe_and_preserves_containment_and_environment() {
        let spawner = Arc::new(MockSpawner::new());
        let controller = spawner.enqueue(MockProcessConfig::completed(0));
        let _guard = install_spawner(spawner.clone());
        let mut child = spawn_harn_with_parent_secrets(
            spec(),
            handoff().await,
            Duration::from_secs(1),
            &|| false,
        )
        .unwrap();
        assert_eq!(child.wait().unwrap().code, Some(0));
        let captured = spawner.captured();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].env.is_empty());
        assert_eq!(captured[0].owner_death, OwnerDeathPolicy::KillContainment);
        assert!(captured[0]
            .args
            .iter()
            .all(|arg| !arg.contains("inert-canary")));
        let bytes = controller.stdin_written();
        let received = ParentSecretHandoff::read_from(bytes.as_slice())
            .unwrap()
            .into_provider();
        assert!(received
            .get(&SecretId::new("fixture", "provider"))
            .await
            .unwrap()
            .with_exposed(|value| value == b"inert-canary"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn application_stdin_is_refused_without_spawning() {
        let spawner = Arc::new(MockSpawner::new());
        let _guard = install_spawner(spawner.clone());
        let mut spec = spec();
        spec.use_stdin = true;
        let error =
            spawn_harn_with_parent_secrets(spec, handoff().await, Duration::from_secs(1), &|| {
                false
            })
            .err()
            .expect("application stdin must not be replaced");
        assert!(matches!(error, SecretHandoffSpawnError::Refused { .. }));
        assert!(spawner.captured().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_after_spawn_returns_the_existing_cleanup_and_reap_receipts() {
        let spawner = Arc::new(MockSpawner::new());
        let controller = spawner.enqueue(MockProcessConfig::running());
        let _guard = install_spawner(spawner.clone());
        let calls = std::cell::Cell::new(0);
        let interrupted = || {
            let previous = calls.get();
            calls.set(previous + 1);
            previous > 0
        };
        let error = spawn_harn_with_parent_secrets(
            spec(),
            handoff().await,
            Duration::from_secs(1),
            &interrupted,
        )
        .err()
        .expect("cancelled transfer must not yield a usable child");
        let SecretHandoffSpawnError::Transfer { cleanup, reap, .. } = error else {
            panic!("missing owner receipts")
        };
        assert!(controller.was_killed());
        assert_eq!(cleanup.root_pid, Some(99_999));
        assert!(reap.is_ok());
    }
}
