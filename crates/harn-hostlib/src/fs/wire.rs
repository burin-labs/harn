use std::sync::Arc;

use harn_vm::VmValue;

use crate::tools::args::{build_dict, str_value};

use super::{CommitResult, DiscardResult, PendingWrite, StagedStatus};

pub(super) fn status_to_value(status: StagedStatus) -> VmValue {
    build_dict([
        (
            "pending_writes",
            VmValue::List(Arc::new(
                status
                    .pending_writes
                    .into_iter()
                    .map(pending_write_to_value)
                    .collect(),
            )),
        ),
        (
            "total_bytes_pending",
            VmValue::Int(status.total_bytes_pending as i64),
        ),
        (
            "oldest_pending_age_ms",
            VmValue::Int(status.oldest_pending_age_ms),
        ),
    ])
}

fn pending_write_to_value(write: PendingWrite) -> VmValue {
    build_dict([
        ("path", str_value(&write.path)),
        ("kind", str_value(write.kind)),
        ("bytes_added", VmValue::Int(write.bytes_added as i64)),
        ("bytes_removed", VmValue::Int(write.bytes_removed as i64)),
        (
            "snapshot_id",
            write
                .snapshot_id
                .as_deref()
                .map(str_value)
                .unwrap_or(VmValue::Nil),
        ),
    ])
}

pub(super) fn commit_result_to_value(result: CommitResult) -> VmValue {
    build_dict([
        (
            "committed_paths",
            VmValue::List(Arc::new(
                result
                    .committed_paths
                    .into_iter()
                    .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
                    .collect(),
            )),
        ),
        (
            "failed_paths_with_reasons",
            VmValue::List(Arc::new(
                result
                    .failed_paths_with_reasons
                    .into_iter()
                    .map(|(path, reason)| {
                        build_dict([("path", str_value(&path)), ("reason", str_value(&reason))])
                    })
                    .collect(),
            )),
        ),
    ])
}

pub(super) fn discard_result_to_value(result: DiscardResult) -> VmValue {
    build_dict([(
        "discarded_paths",
        VmValue::List(Arc::new(
            result
                .discarded_paths
                .into_iter()
                .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
                .collect(),
        )),
    )])
}
