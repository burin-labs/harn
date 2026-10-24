pub(crate) use harn_clock::test_support::current_thread_runtime;

#[cfg(test)]
mod tests {
    use super::current_thread_runtime;
    use futures::FutureExt;

    #[test]
    fn current_thread_runtime_supports_io_and_paused_timers() {
        current_thread_runtime().block_on(async {
            assert!(matches!(
                tokio::runtime::Handle::current().runtime_flavor(),
                tokio::runtime::RuntimeFlavor::CurrentThread
            ));
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("listen on loopback");
            let (client, server) = tokio::join!(
                tokio::net::TcpStream::connect(listener.local_addr().expect("local address")),
                listener.accept()
            );
            let _client = client.expect("connect loopback client");
            let (_server, _) = server.expect("accept loopback client");
            tokio::time::pause();
            let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
            ticks.tick().await;
            assert!(ticks.tick().now_or_never().is_none());
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
            assert!(ticks.tick().now_or_never().is_some());
        });
    }
}
