//! Narrow authority for empirical provider-contract probes.
//!
//! Production dispatch lets the capability catalog reject or rewrite options
//! that a route declares unsupported. A contract probe must send exactly one
//! selected option through those local guards so the provider, rather than the
//! catalog, supplies the observation. The exception is typed and task-local so
//! concurrent calls cannot inherit it.

use super::capabilities::PortableOption;

tokio::task_local! {
    static PORTABLE_OPTION: PortableOption;
}

/// Run one provider-contract probe with catalog shaping suspended for exactly
/// the selected portable option.
pub async fn with_portable_option_probe<F>(option: PortableOption, future: F) -> F::Output
where
    F: std::future::Future,
{
    PORTABLE_OPTION.scope(option, future).await
}

/// Whether catalog policy may reject, omit, or rewrite an explicit portable
/// option before the provider sees it.
///
/// Production calls always return true. A probe returns false only for its
/// selected option; every unrelated guard remains active.
pub(crate) fn current_portable_option() -> Option<PortableOption> {
    PORTABLE_OPTION.try_with(|selected| *selected).ok()
}

pub(crate) fn catalog_may_shape_requested_portable_option(
    selected: Option<PortableOption>,
    option: PortableOption,
) -> bool {
    selected != Some(option)
}

/// Whether the current call must stop after its first physical provider request.
///
/// Ordinary calls keep the runtime's bounded empty-output and transport
/// recoveries. A contract probe needs one request to equal one observation so
/// its budget and request-count receipt cannot under-report provider traffic.
pub(crate) fn requires_single_request(selected: Option<PortableOption>) -> bool {
    selected.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_authority_is_typed_exact_and_task_local() {
        use PortableOption::{Temperature, TopP};

        assert!(catalog_may_shape_requested_portable_option(
            None,
            Temperature
        ));
        assert!(!requires_single_request(None));
        let (temperature_task, top_p_task) = tokio::join!(
            with_portable_option_probe(Temperature, async {
                tokio::task::yield_now().await;
                let mut opts = crate::llm::api::LlmCallOptions::default();
                opts.provider_contract_probe = current_portable_option();
                let payload = crate::llm::api::LlmRequestPayload::from(&opts);
                assert_eq!(payload.provider_contract_probe, Some(Temperature));
                (
                    catalog_may_shape_requested_portable_option(
                        current_portable_option(),
                        Temperature,
                    ),
                    catalog_may_shape_requested_portable_option(current_portable_option(), TopP),
                    requires_single_request(current_portable_option()),
                )
            }),
            with_portable_option_probe(TopP, async {
                tokio::task::yield_now().await;
                (
                    catalog_may_shape_requested_portable_option(
                        current_portable_option(),
                        Temperature,
                    ),
                    catalog_may_shape_requested_portable_option(current_portable_option(), TopP),
                    requires_single_request(current_portable_option()),
                )
            }),
        );

        assert_eq!(temperature_task, (false, true, true));
        assert_eq!(top_p_task, (true, false, true));
        assert!(catalog_may_shape_requested_portable_option(
            None,
            Temperature
        ));
        assert!(!requires_single_request(None));
    }
}
