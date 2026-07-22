use harn_vm::orchestration::CapabilityPolicy;
use harn_vm::personas::{PersonaBudgetPolicy, PersonaRuntimeBinding};

#[test]
fn released_persona_policy_struct_literals_remain_constructible() {
    let _policy = CapabilityPolicy {
        tools: vec!["read".to_string()],
        ..Default::default()
    };
    let _binding = PersonaRuntimeBinding {
        name: "reviewer".to_string(),
        template_ref: None,
        entry_workflow: "review.harn".to_string(),
        schedules: Vec::new(),
        triggers: Vec::new(),
        budget: PersonaBudgetPolicy::default(),
        stages: Vec::new(),
    };
}
