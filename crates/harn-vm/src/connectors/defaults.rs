use super::*;

pub(super) fn default_connector_for_provider(
    provider: &ProviderMetadata,
    clock: Arc<dyn harn_clock::Clock>,
) -> Box<dyn Connector> {
    match &provider.runtime {
        ProviderRuntimeMetadata::Builtin {
            connector,
            default_signature_variant,
        } => match connector.as_str() {
            "a2a-push" => Box::new(A2aPushConnector::new()),
            "cron" => Box::new(CronConnector::with_clock(clock)),
            "stream" => Box::new(StreamConnector::new(
                ProviderId::from(provider.provider.clone()),
                provider.schema_name.clone(),
            )),
            "webhook" => {
                let variant = WebhookSignatureVariant::parse(default_signature_variant.as_deref())
                    .expect("catalog webhook signature variant must be valid");
                Box::new(GenericWebhookConnector::with_profile(
                    WebhookProviderProfile::new(
                        ProviderId::from(provider.provider.clone()),
                        provider.schema_name.clone(),
                        variant,
                    ),
                ))
            }
            _ => Box::new(PlaceholderConnector::from_metadata(provider)),
        },
        ProviderRuntimeMetadata::Placeholder => {
            Box::new(PlaceholderConnector::from_metadata(provider))
        }
    }
}
