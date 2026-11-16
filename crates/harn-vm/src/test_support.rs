pub(crate) use harn_clock::test_support::current_thread_runtime;

#[cfg(test)]
#[track_caller]
pub(crate) fn decode_fixture<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    let mut ignored = Vec::new();
    let fixture = serde_ignored::deserialize(value, |path| ignored.push(path.to_string()))
        .expect("typed fixture must deserialize");
    assert!(
        ignored.is_empty(),
        "fixture fields were ignored: {ignored:?}"
    );
    fixture
}

#[cfg(test)]
pub(crate) mod host_command_fixture;

#[cfg(test)]
pub(crate) fn vm_with_stdlib() -> crate::vm::Vm {
    let mut vm = crate::new_vm();
    crate::stdlib::register_vm_stdlib(&mut vm);
    vm
}

#[cfg(test)]
pub(crate) mod session_init_fixture {
    use crate::value::VmDictExt;
    use std::path::Path;

    pub(crate) fn reset() {
        crate::agent_sessions::reset_session_store();
    }

    pub(crate) fn reset_with_hooks() {
        reset();
        crate::orchestration::clear_session_hooks();
    }

    pub(crate) async fn prepare(root: &Path, session_id: &str) {
        let mut options = crate::value::DictMap::new();
        options.put_str("root", root.to_string_lossy().as_ref());
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            format!("run-{session_id}"),
            format!("turn-{session_id}"),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
    }

    pub(crate) async fn terminal(root: &Path, session_id: &str) -> serde_json::Value {
        let store = crate::stdlib::session_store::open_canonical_agent_session(
            &crate::stdlib::session_store::SessionStoreDir::under_root(root),
            session_id,
            None,
            harn_session_store::SessionType::User,
        )
        .await
        .expect("open canonical session");
        crate::stdlib::session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events")
            .into_iter()
            .find(|event| {
                event.payload.pointer("/transcript_event/kind")
                    == Some(&serde_json::json!("agent_run_terminal"))
            })
            .expect("durable terminal")
            .payload
    }
}

#[cfg(test)]
mod tests {
    use super::current_thread_runtime;
    use futures::FutureExt;

    #[test]
    #[should_panic(expected = "fixture fields were ignored: [\"context_widnow\"]")]
    fn fixture_refuses_unknown_model_fields() {
        super::decode_fixture::<crate::llm_config::ModelDef>(serde_json::json!({
            "name": "Fixture", "provider": "openai", "context_window": 128_000,
            "context_widnow": 64,
        }));
    }

    #[test]
    #[should_panic(expected = "missing field `name`")]
    fn fixture_refuses_missing_required_model_fields() {
        super::decode_fixture::<crate::llm_config::ModelDef>(serde_json::json!({
            "provider": "openai", "context_window": 128_000,
        }));
    }

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
