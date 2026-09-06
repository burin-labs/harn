use super::*;
use generated_rust_binding::{
    HarnPlanApproval, HarnPlanCommentAnchor, HarnPlanDocument, HarnPlanStep,
};

fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(
    value: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(serde_json::from_value::<T>(value)?)
}

#[test]
fn generated_plan_records_preserve_producer_data_and_presence() {
    let fixture =
        super::super::plan_records::round_trip_fixture().expect("canonical plan producer");
    assert_eq!(fixture["comments"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["resolution_receipts"].as_array().unwrap().len(), 1);
    assert_eq!(
        round_trip::<HarnPlanDocument>(fixture.clone()).unwrap(),
        fixture
    );

    let step = json!({"id": "step", "content": "Verify", "status": "pending"});
    let mut expected = step.clone();
    expected["priority"] = json!(null);
    assert_eq!(round_trip::<HarnPlanStep>(step).unwrap(), expected);
    assert_eq!(
        round_trip::<HarnPlanStep>(expected.clone()).unwrap(),
        expected
    );
    expected["priority"] = json!("high");
    assert_eq!(
        round_trip::<HarnPlanStep>(expected.clone()).unwrap(),
        expected
    );

    let approval = json!({"state": "unrequested"});
    assert_eq!(
        round_trip::<HarnPlanApproval>(approval.clone()).unwrap(),
        approval
    );
    assert!(
        round_trip::<HarnPlanApproval>(json!({"state":"unrequested", "reviewers":null})).is_err()
    );
    let reviewers = json!({"state":"unrequested", "reviewers":["reviewer"]});
    assert_eq!(
        round_trip::<HarnPlanApproval>(reviewers.clone()).unwrap(),
        reviewers
    );

    let anchor = json!({"step_id":"step"});
    assert_eq!(
        round_trip::<HarnPlanCommentAnchor>(anchor.clone()).unwrap(),
        anchor
    );
    assert_eq!(
        round_trip::<HarnPlanCommentAnchor>(json!({"step_id":"step", "range":null})).unwrap(),
        anchor
    );
    let range = json!({"step_id":"step", "range":{"start":0,"end":1}});
    assert_eq!(
        round_trip::<HarnPlanCommentAnchor>(range.clone()).unwrap(),
        range
    );
}
