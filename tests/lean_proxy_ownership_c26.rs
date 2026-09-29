#[test]
fn handler_moves_owned_json_once_and_fallback_keeps_its_required_source() {
    let chat = include_str!("../src/server/api/chat.rs");
    let boundary = chat
        .split("async fn execute_single_model(")
        .nth(1)
        .expect("execute_single_model source")
        .split("async fn forward_with_provider_fallback(")
        .next()
        .expect("single-model ownership section");

    assert!(boundary.contains("request_body: Value"));
    assert!(boundary.contains("let mut body = request_body;"));
    assert!(!boundary.contains("request_body.clone()"));
    assert!(!chat.contains("execute_single_model(\n        &state,\n        &body,"));

    let fallback = chat
        .split("async fn forward_with_provider_fallback(")
        .nth(1)
        .expect("fallback source");
    assert!(fallback.contains("mut request_body: Value"));
    // Retired adapters no longer contribute clones; all 14 surviving specialized
    // request constructors must still retain the immutable fallback source.
    assert_eq!(
        fallback.matches("body: request_body.clone()").count(),
        14,
        "specialized account/provider attempts must retain the request-scoped source"
    );

    for forbidden in ["unsafe {", "static REQUEST_BODY", "Arc<Value>"] {
        assert!(
            !boundary.contains(forbidden),
            "ownership transfer introduced forbidden mechanism: {forbidden}"
        );
    }
}
