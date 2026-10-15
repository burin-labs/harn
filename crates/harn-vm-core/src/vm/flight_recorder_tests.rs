use super::*;
use crate::chunk::{Chunk, Op};
use std::sync::Arc;
#[test]
fn ring_retains_the_exact_newest_path_and_counts_eviction() {
    let mut chunk = Chunk::new();
    chunk.emit(crate::chunk::Op::Nil, 7);
    let recorder = FlightRecorder::new(crate::ExecutionId::mint(), 2);
    for task in ["a", "b", "c"] {
        recorder.record_instruction(task, 1, "main", &chunk, Some("main.harn"), 0, Op::Nil);
    }
    let recording = recorder.snapshot();
    assert_eq!(recording.dropped_events, 1);
    assert_eq!(recording.events.len(), 2);
    assert_eq!(recording.events[0].sequence, 1);
    assert_eq!(recording.tasks[recording.events[1].task_id as usize], "c");
    assert_eq!(recording.events[1].line, 7);
    assert_eq!(
        recording.sources[recording.events[1].source_id.unwrap() as usize],
        "main.harn"
    );
    assert_eq!(recording.opcode_names[&recording.events[1].opcode], "Nil");
}

#[test]
fn concurrent_writers_share_one_gap_free_sequence() {
    let mut chunk = Chunk::new();
    chunk.emit(Op::Nil, 1);
    let chunk = Arc::new(chunk);
    let recorder = FlightRecorder::new(crate::ExecutionId::mint(), 8_000);
    let writers = (0..8)
        .map(|worker| {
            let chunk = Arc::clone(&chunk);
            let recorder = Arc::clone(&recorder);
            crate::runtime_stack::spawn(move || {
                let task = format!("task-{worker}");
                for _ in 0..1_000 {
                    recorder.record_instruction(
                        &task,
                        1,
                        "main",
                        &chunk,
                        Some("main.harn"),
                        0,
                        Op::Nil,
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }

    let recording = recorder.snapshot();
    assert_eq!(recording.events.len(), 8_000);
    assert_eq!(recording.dropped_events, 0);
    assert_eq!(recording.tasks.len(), 8);
    assert!(recording
        .events
        .iter()
        .enumerate()
        .all(|(index, event)| event.sequence == index as u64));
}

#[test]
fn only_the_root_vm_owns_terminal_state_and_rollover() {
    let mut root = crate::vm::test_runtime::new_vm();
    root.enable_flight_recorder(8);
    let configured_id = root.execution_id().to_string();
    root.prepare_execution_for_top_level();
    let first = root.flight_recorder.as_ref().unwrap().clone();
    let child = root.child_vm();
    assert!(root.owns_execution);
    assert!(!child.owns_execution);
    assert_ne!(root.execution_id().as_str(), configured_id);
    assert_eq!(root.execution_id(), first.execution_id());
    assert_eq!(root.execution_id(), child.execution_id());
    assert!(Arc::ptr_eq(
        root.flight_recorder.as_ref().unwrap(),
        child.flight_recorder.as_ref().unwrap()
    ));

    first.finish("returned", None);
    root.prepare_execution_for_top_level();
    let second = root.flight_recorder.as_ref().unwrap();
    assert!(!Arc::ptr_eq(&first, second));
    assert_eq!(second.max_events, 8);
    assert!(second.snapshot().terminal.is_none());
}
