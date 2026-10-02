//! Lock state of the Linux Secret Service, read without asking it to unlock.
//!
//! The Secret Service protocol has no "do not prompt" flag. An unlock dialog
//! appears only when a client calls `Unlock` and then `Prompt` on the object
//! that call returns. The keyring adapter does both whenever an operation
//! touches a locked item, and blocks until somebody answers. `SearchItems`
//! and a collection's `Locked` property do neither, so this module answers
//! "would serving a credential need a prompt?" without asking for an unlock.
//!
//! The adapter searches every collection, so a credential moved out of the
//! default collection is still served from wherever it is. The probe follows
//! it: any of the service's items in a locked collection makes the store
//! locked, and so does a locked default collection, where writes go.
//!
//! Everything runs on the calling thread and is bounded by a timer. The D-Bus
//! connection is built without zbus's executor thread, and the probe ticks
//! that executor itself, so when the probe returns or the timer wins there is
//! no thread left to park.

use std::collections::HashMap;
use std::time::Duration;

use futures::future::{self, Either};
use keyring_core::Error as KeyringError;
use secret_service::{EncryptionType, SecretService};

use super::keyring::{NativeKeyringAvailability, NativeKeyringError};

/// Report whether `service`'s credentials can be read, and new ones written,
/// without an unlock prompt.
pub(super) fn secret_service_availability(
    service: &str,
    timeout: Duration,
) -> Result<NativeKeyringAvailability, NativeKeyringError> {
    availability_on_bus(zbus::connection::Builder::session, service, timeout)
}

fn availability_on_bus(
    bus: impl FnOnce() -> zbus::Result<zbus::connection::Builder<'static>>,
    service: &str,
    timeout: Duration,
) -> Result<NativeKeyringAvailability, NativeKeyringError> {
    let probe = Box::pin(read_availability(bus, service));
    let deadline = Box::pin(async_io::Timer::after(timeout));
    match async_io::block_on(future::select(probe, deadline)) {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(inaccessible(format!(
            "the Secret Service did not answer within {}ms",
            timeout.as_millis()
        ))),
    }
}

async fn read_availability(
    bus: impl FnOnce() -> zbus::Result<zbus::connection::Builder<'static>>,
    service: &str,
) -> Result<NativeKeyringAvailability, NativeKeyringError> {
    let connection = bus()
        .map_err(unreachable_bus)?
        .internal_executor(false)
        .build()
        .await
        .map_err(unreachable_bus)?;
    let executor = connection.executor().clone();
    let tick_forever = Box::pin(async move {
        loop {
            executor.tick().await;
        }
    });
    let probe = Box::pin(read_lock_state(connection, service));
    match future::select(probe, tick_forever).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => unreachable!("the executor loop never completes"),
    }
}

async fn read_lock_state(
    connection: zbus::Connection,
    service: &str,
) -> Result<NativeKeyringAvailability, NativeKeyringError> {
    let secrets = SecretService::connect_with_existing(EncryptionType::Plain, connection)
        .await
        .map_err(unreachable_bus)?;
    // The same attribute the adapter searches by, across every collection.
    let matches = secrets
        .search_items(HashMap::from([("service", service)]))
        .await
        .map_err(|error| inaccessible(format!("could not search the Secret Service: {error}")))?;
    if !matches.locked.is_empty() {
        return Ok(NativeKeyringAvailability::Locked);
    }
    let collection = match secrets.get_default_collection().await {
        Ok(collection) => collection,
        // Creating the default collection asks for a new password, which is
        // a prompt too, so a missing collection cannot serve a credential.
        Err(secret_service::Error::NoResult) => {
            return Err(inaccessible(
                "the Secret Service has no default collection".to_string(),
            ))
        }
        Err(error) => {
            return Err(inaccessible(format!(
                "could not open the default Secret Service collection: {error}"
            )))
        }
    };
    let locked = collection.is_locked().await.map_err(|error| {
        inaccessible(format!(
            "could not read the default collection's lock state: {error}"
        ))
    })?;
    Ok(if locked {
        NativeKeyringAvailability::Locked
    } else {
        NativeKeyringAvailability::Available
    })
}

fn unreachable_bus(error: impl std::fmt::Display) -> NativeKeyringError {
    inaccessible(format!("could not reach the Secret Service: {error}"))
}

fn inaccessible(detail: String) -> NativeKeyringError {
    KeyringError::NoStorageAccess(Box::new(std::io::Error::other(detail))).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::NativeKeyringUnavailable;

    fn live_threads() -> usize {
        std::fs::read_dir("/proc/self/task")
            .expect("list this process's threads")
            .count()
    }

    /// A bus that accepts the connection and then says nothing stands in for
    /// a Secret Service that stops answering. The probe must give up at its
    /// deadline and leave no thread behind.
    ///
    /// The first call is the control for the count: it may start async-io's
    /// one process-wide reactor thread. The second call must start nothing,
    /// so a per-call thread parked on the silent bus would show up here.
    #[test]
    fn a_silent_bus_times_out_without_leaving_a_thread() {
        let dir = tempfile::tempdir().expect("socket dir");
        let socket = dir.path().join("bus");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind bus");
        let address = format!("unix:path={}", socket.display());
        let probe = || {
            availability_on_bus(
                || zbus::connection::Builder::address(address.as_str()),
                "harn.availability-test",
                Duration::from_millis(200),
            )
        };

        let first = probe().expect_err("a silent bus is not available");
        let threads = live_threads();
        let second = probe().expect_err("a silent bus is not available");

        for error in [&first, &second] {
            assert!(error.to_string().contains("did not answer"), "{error}");
            assert_eq!(
                error.unavailable_reason(),
                Some(NativeKeyringUnavailable::StorageInaccessible)
            );
        }
        assert_eq!(live_threads(), threads, "a probe left a thread running");
    }

    /// The negative control: no bus at all is unavailable, with the reason.
    #[test]
    fn a_missing_bus_reports_unavailable_with_its_reason() {
        let dir = tempfile::tempdir().expect("socket dir");
        let address = format!("unix:path={}", dir.path().join("absent").display());

        let error = availability_on_bus(
            || zbus::connection::Builder::address(address.as_str()),
            "harn.availability-test",
            Duration::from_secs(5),
        )
        .expect_err("a missing bus is not available");

        assert!(
            error
                .to_string()
                .contains("could not reach the Secret Service"),
            "{error}"
        );
        assert_eq!(
            error.unavailable_reason(),
            Some(NativeKeyringUnavailable::StorageInaccessible)
        );
    }
}
