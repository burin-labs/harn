use super::*;

#[test]
fn parse_oauth_redirect_url_extracts_code_state_issuer() {
    let (state, code, issuer) = parse_oauth_redirect_url(
        "burin://oauth/callback?code=auth-code&state=xyz&iss=https://auth.example",
    )
    .expect("parse");
    assert_eq!(state, "xyz");
    assert_eq!(code, "auth-code");
    assert_eq!(issuer.as_deref(), Some("https://auth.example"));
}

#[test]
fn parse_oauth_redirect_url_propagates_provider_error() {
    let error = parse_oauth_redirect_url(
        "http://127.0.0.1/cb?error=access_denied&error_description=nope&state=xyz",
    )
    .expect_err("error param");
    assert!(error.contains("access_denied"), "{error}");
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn parse_oauth_redirect_url_requires_code() {
    let error =
        parse_oauth_redirect_url("http://127.0.0.1/cb?state=xyz").expect_err("missing code");
    assert!(error.contains("code"), "{error}");
}
