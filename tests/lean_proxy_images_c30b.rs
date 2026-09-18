use openproxy::core::translator::helpers::image_helper::{
    fetch_image_as_base64, ImagePrefetchBudget, ImagePrefetchError,
};
use serde_json::json;

#[tokio::test]
async fn private_literal_destinations_are_rejected_before_connect() {
    let client = reqwest::Client::new();
    for url in [
        "http://127.0.0.1:1/image",
        "http://2130706433:1/image",
        "http://10.0.0.1/image",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/image",
        "http://[fe80::1]/image",
        "http://[fc00::1]/image",
        "http://[::ffff:127.0.0.1]/image",
    ] {
        let mut budget = ImagePrefetchBudget::new(&json!({"messages": []})).unwrap();
        let error = fetch_image_as_base64(&client, url, &mut budget)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ImagePrefetchError::BlockedDestination),
            "{url}: {error}"
        );
        assert_eq!(error.http_status(), 502);
    }
}

#[test]
fn source_binds_validated_addresses_without_weakening_tls_or_pool_policy() {
    let source = include_str!("../src/core/translator/helpers/image_helper.rs");
    assert!(!source.contains("_pinned_ip"));
    assert!(source.contains("resolve_to_addrs(&target.host, &target.pinned_addrs)"));
    assert!(source.contains("let target = resolve_image_target(&parsed).await?;"));
    assert!(source.contains("let pinned_client = build_pinned_image_client(&target)?;"));
    assert!(source.contains(".no_proxy()"));
    assert!(source.contains("Policy::none()"));
    assert!(source.contains("IMAGE_DNS_TIMEOUT"));
    assert!(source.contains("IMAGE_CONNECT_TIMEOUT"));
    assert!(!source.contains("danger_accept_invalid_certs"));
    assert!(!source.contains("danger_accept_invalid_hostnames"));

    let fetch = source
        .split("pub async fn fetch_image_as_base64")
        .nth(1)
        .and_then(|tail| tail.split("async fn read_image_response").next())
        .expect("image fetch function");
    assert!(!fetch.contains("Client::new()"));
    assert!(!fetch.contains("reqwest::Client::builder()"));
    assert!(!fetch.contains("lookup_host"));

    // The risk proof is deterministic code-path evidence, not a claim that a
    // live DNS rebinding exploit was performed. Unit tests additionally prove
    // Host preservation, pinned multi-address connect, and TLS name mismatch.
    assert!(source.contains("pinned_client_connects_only_to_supplied_addresses_and_preserves_host"));
    assert!(source.contains("pinned_tls_keeps_hostname_verification_enabled"));
}
