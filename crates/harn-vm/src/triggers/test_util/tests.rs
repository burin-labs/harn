use super::{run_trigger_harness_fixture, TRIGGER_TEST_FIXTURES};

#[tokio::test(flavor = "current_thread")]
async fn every_trigger_harness_fixture_reports_success() {
    for fixture in TRIGGER_TEST_FIXTURES {
        let result = run_trigger_harness_fixture(fixture)
            .await
            .unwrap_or_else(|error| panic!("{fixture} should run: {error}"));
        assert!(result.ok, "{fixture} should report success: {result:?}");
    }
}
