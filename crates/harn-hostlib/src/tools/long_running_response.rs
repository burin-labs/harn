use harn_vm::VmValue;

use super::LongRunningHandleInfo;
use crate::tools::args::to_agent_path;
use crate::tools::proc::{self, CommandStatus};
use crate::tools::response::ResponseBuilder;

impl LongRunningHandleInfo {
    /// Convert into the standard handle response dict returned to the agent.
    pub fn into_handle_response(self) -> VmValue {
        self.handle_response()
    }

    pub(crate) fn handle_response(&self) -> VmValue {
        let artifacts = proc::planned_artifact_paths(&self.command_id);
        let mut builder = ResponseBuilder::new()
            .str("command_id", self.command_id.clone())
            .str("cwd", to_agent_path(&self.cwd))
            .str("status", CommandStatus::Running.as_str())
            .int("pid", self.pid as i64)
            .str("handle_id", self.handle_id.clone())
            .str("started_at", self.started_at.clone())
            .nil("ended_at")
            .int("duration_ms", 0)
            .nil("exit_code")
            .nil("signal")
            .bool("timed_out", false)
            .str("stdout", "")
            .bool("stdout_truncated", false)
            .str("stderr", "")
            .bool("stderr_truncated", false)
            .str("output_path", to_agent_path(&artifacts.output_path))
            .str("stdout_path", to_agent_path(&artifacts.stdout_path))
            .str("stderr_path", to_agent_path(&artifacts.stderr_path))
            .int("line_count", 0)
            .int("byte_count", 0)
            .str("output_sha256", "")
            .nil("denial")
            .value("sandbox", proc::sandbox_response(&self.sandbox_report))
            .str("audit_id", format!("audit_{}", self.command_id))
            .str("command", self.command_display.clone())
            .str("command_or_op_descriptor", self.command_display.clone());
        builder = match self.process_group_id {
            Some(pgid) => builder.int("process_group_id", pgid as i64),
            None => builder.nil("process_group_id"),
        };
        if let Some(snapshot_binding) = &self.snapshot_binding {
            builder = builder.dict("snapshot_binding", snapshot_binding.clone());
        }
        builder.build()
    }
}
