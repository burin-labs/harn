use super::{scope_placement, spawn_child};
use crate::vm::subtask::{placement, SubtaskPlacement};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_placement_reaches_the_spawn_seam() {
    assert_eq!(placement(), SubtaskPlacement::Worker);
    let observed = scope_placement(SubtaskPlacement::CurrentThread, async { placement() }).await;
    assert_eq!(observed, SubtaskPlacement::CurrentThread);
    assert_eq!(placement(), SubtaskPlacement::Worker);
}

#[test]
fn placement_selects_the_executor_thread() {
    fn observed_thread(
        runtime: &tokio::runtime::Runtime,
        placement: SubtaskPlacement,
    ) -> std::thread::ThreadId {
        runtime.block_on(async {
            tokio::task::LocalSet::new()
                .run_until(scope_placement(placement, async {
                    spawn_child(crate::stdlib::pool::new_pool_registry(), async {
                        std::thread::current().id()
                    })
                    .await
                    .expect("subtask completes")
                }))
                .await
        })
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("test runtime");
    let creating_thread = std::thread::current().id();
    assert_ne!(
        observed_thread(&runtime, SubtaskPlacement::Worker),
        creating_thread
    );
    assert_eq!(
        observed_thread(&runtime, SubtaskPlacement::CurrentThread),
        creating_thread
    );
}
