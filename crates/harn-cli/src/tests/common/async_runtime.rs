use std::future::Future;
use std::thread;

fn run_on_cli_stack<T: Send + 'static>(name: &str, run: impl FnOnce() -> T + Send + 'static) -> T {
    thread::Builder::new()
        .name(name.into())
        .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
        .spawn(run)
        .expect("failed to spawn CLI-stack test thread")
        .join()
        .expect("CLI-stack test thread panicked")
}

/// Runs an async CLI test on the same stack size as the production CLI.
///
/// Harn compilation and VM setup can exceed libtest's 2 MiB worker stack.
/// Keep stack-sensitive tests on this shared path instead of relying on the
/// host's thread defaults.
pub fn block_on_cli_stack<Fut>(build: impl FnOnce() -> Fut + Send + 'static) -> Fut::Output
where
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    run_on_cli_stack("harn-cli-test", move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("CLI test runtime")
            .block_on(build())
    })
}

/// Runs an async CLI test on a multi-thread runtime whose workers also match
/// the production CLI stack size.
pub fn block_on_cli_stack_multi_thread<Fut>(
    build: impl FnOnce() -> Fut + Send + 'static,
) -> Fut::Output
where
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    run_on_cli_stack("harn-cli-test", move || {
        tokio::runtime::Builder::new_multi_thread()
            .thread_stack_size(crate::CLI_RUNTIME_STACK_SIZE)
            .enable_all()
            .build()
            .expect("multi-thread CLI test runtime")
            .block_on(build())
    })
}
