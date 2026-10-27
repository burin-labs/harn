//! Selection of a supported interface from an agent card.
use super::*;

pub(super) fn endpoint_from_card(
    card_url: String,
    allow_cleartext: bool,
    requested_authority: &str,
    target_agent: String,
    card: &Value,
) -> Result<NegotiatedEndpoint, A2aClientError> {
    let (rpc_url, protocol_version, tenant) = if let Some(interface) =
        endpoint_from_supported_interfaces(card, allow_cleartext, requested_authority)?
    {
        interface
    } else if has_current_transport_fields(card) {
        (
            endpoint_from_current_card(card, allow_cleartext, requested_authority)?,
            A2aWireVersion::V0_3,
            None,
        )
    } else {
        return Err(A2aClientError::Discovery(
            "A2A agent card missing preferredTransport/additionalInterfaces".to_string(),
        ));
    };
    Ok(NegotiatedEndpoint {
        endpoint: ResolvedA2aEndpoint {
            card_url,
            rpc_url: rpc_url.to_string(),
            agent_id: card.get("id").and_then(Value::as_str).map(str::to_string),
            target_agent,
        },
        protocol_version,
        tenant,
    })
}

pub(crate) fn peer_wire_settings(
    card: &Value,
    card_url: &str,
    rpc_url: &str,
    raw_target: &str,
) -> Result<(A2aWireVersion, Option<String>), A2aClientError> {
    if card.get("supportedInterfaces").is_none() {
        return Ok((A2aWireVersion::V0_3, None));
    }
    let target = parse_target(raw_target)?;
    let endpoint = endpoint_from_card(
        card_url.to_string(),
        rpc_url.starts_with("http://"),
        &target.authority,
        target.target_agent,
        card,
    )?;
    if endpoint.rpc_url != rpc_url {
        return Err(A2aClientError::Discovery(
            "A2A peer endpoint no longer matches its advertised interface".to_string(),
        ));
    }
    Ok((endpoint.protocol_version, endpoint.tenant))
}

fn endpoint_from_supported_interfaces(
    card: &Value,
    allow_cleartext: bool,
    requested_authority: &str,
) -> Result<Option<(Url, A2aWireVersion, Option<String>)>, A2aClientError> {
    let Some(interfaces) = card.get("supportedInterfaces") else {
        return Ok(None);
    };
    let interfaces = interfaces.as_array().ok_or_else(|| {
        A2aClientError::Discovery("A2A supportedInterfaces must be an array".to_string())
    })?;
    let mut saw_jsonrpc = false;
    for interface in interfaces {
        if !interface
            .get("protocolBinding")
            .and_then(Value::as_str)
            .is_some_and(transport_is_jsonrpc)
        {
            continue;
        }
        saw_jsonrpc = true;
        let Some(protocol_version) = interface
            .get("protocolVersion")
            .and_then(Value::as_str)
            .and_then(A2aWireVersion::from_advertised)
        else {
            continue;
        };
        let interface_url = interface
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                A2aClientError::Discovery("A2A JSONRPC supportedInterface missing url".to_string())
            })?;
        let tenant = match interface.get("tenant") {
            None => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(A2aClientError::Discovery(
                    "A2A interface tenant must be a string".to_string(),
                ))
            }
        };
        return resolve_declared_url(
            interface_url,
            allow_cleartext,
            requested_authority,
            "JSONRPC supportedInterface url",
        )
        .map(|url| Some((url, protocol_version, tenant)));
    }
    if saw_jsonrpc {
        return Err(A2aClientError::Discovery(
            "A2A supportedInterfaces does not expose JSONRPC for a supported protocol version"
                .to_string(),
        ));
    }
    Err(A2aClientError::Discovery(
        "A2A agent card does not expose a JSONRPC supportedInterface".to_string(),
    ))
}
