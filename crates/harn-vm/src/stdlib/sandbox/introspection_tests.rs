use super::*;
use crate::stdlib::sandbox::backend::UnconfinedBackend;
use crate::stdlib::sandbox::build_std_command;

#[test]
fn unconfined_status_matches_actual_spawn_refusal_for_each_network_ceiling() {
    for (level, dimensions) in [("process_exec", 4), ("network", 3)] {
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::OsHardened,
            side_effect_level: Some(level.to_string()),
            ..CapabilityPolicy::default()
        };
        let status = confinement_for::<UnconfinedBackend>(&policy);
        let VmValue::Dict(status) = status else {
            panic!("confinement status must be a record");
        };
        let error = build_std_command::<UnconfinedBackend>(
            "never-spawned",
            &[],
            &policy,
            SandboxProfile::OsHardened,
        )
        .expect_err("an unconfined backend must refuse before spawning");
        let refusal = error
            .sandbox_mechanism_unavailable()
            .expect("the spawn must preserve its typed refusal");
        assert_eq!(refusal.unconfined.len(), dimensions);
        assert!(matches!(
            status.get("confines_processes"),
            Some(VmValue::Bool(false))
        ));
        let projected = status
            .get("os_hardened_refusal")
            .expect("an unconfined status must carry its refusal");
        assert!(
            crate::value::values_equal(projected, &refusal.thrown_value()),
            "status must project the actual refusal at the {level} ceiling"
        );
    }
}
