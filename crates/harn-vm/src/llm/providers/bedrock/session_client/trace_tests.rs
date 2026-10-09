use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::security::session_environment::declare_session_environment_if_absent;
use crate::security::{EnvironmentPolicyKind, LauncherEnvironment};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn captured_sdk_region_trace_redacts_inputs_after_provider_fires() {
    std::thread::spawn(|| {
        let environment = LauncherEnvironment::from_snapshot(super::tests::map(&[
            ("AWS_ACCESS_KEY_ID", "synthetic-trace-access"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-trace-secret"),
            (
                "AWS_CONTAINER_AUTHORIZATION_TOKEN",
                "synthetic-trace-container",
            ),
            ("AWS_REGION", "us-east-1"),
        ]))
        .launch(EnvironmentPolicyKind::Inherited, Vec::new())
        .expect("captured trace environment");
        let _environment = declare_session_environment_if_absent(environment);
        let output = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("trace runtime");
            assert_eq!(
                runtime
                    .block_on(super::super::resolve_live_region(None))
                    .unwrap(),
                "us-east-1"
            );
        });
        let trace = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(
            trace.contains("region_provider_chain"),
            "actual SDK trace must fire"
        );
        for value in [
            "synthetic-trace-access",
            "synthetic-trace-secret",
            "synthetic-trace-container",
        ] {
            assert!(
                !trace.contains(value),
                "SDK tracing disclosed a captured input"
            );
        }
    })
    .join()
    .expect("trace control thread");
}
