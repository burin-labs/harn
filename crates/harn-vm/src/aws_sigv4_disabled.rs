use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

pub(crate) struct AwsSigV4Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

pub(crate) struct AwsSigV4Input<'a> {
    pub credentials: &'a AwsSigV4Credentials,
    pub method: &'a str,
    pub url: &'a str,
    pub service: &'a str,
    pub region: &'a str,
    pub headers: &'a BTreeMap<String, String>,
    pub body: &'a [u8],
    pub timestamp: DateTime<Utc>,
}

pub(crate) struct AwsSigV4SignedRequest {
    pub headers: BTreeMap<String, String>,
    pub authorization: String,
    pub amz_date: String,
    pub content_sha256: String,
    pub security_token: Option<String>,
    pub signed_headers: String,
}

pub(crate) fn sign(input: AwsSigV4Input<'_>) -> Result<AwsSigV4SignedRequest, String> {
    let _ = (
        &input.credentials.access_key_id,
        &input.credentials.secret_access_key,
        &input.credentials.session_token,
        input.method,
        input.url,
        input.service,
        input.region,
        input.headers,
        input.body,
        input.timestamp,
    );
    Err("AWS signing requires the harn-vm `cloud-aws` feature".to_string())
}
