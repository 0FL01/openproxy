use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use tower::util::ServiceExt;

const TEST_KEY: &str = "updater-hard-cut-test-key";

#[tokio::test]
async fn retired_updater_routes_return_404_with_or_without_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::load_from(temp.path()).await.unwrap());
    db.update(|state| {
        state.settings.require_login = true;
        state.api_keys = vec![ApiKey {
            id: "updater-test-key".into(),
            name: "Test".into(),
            key: TEST_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        }];
    })
    .await
    .unwrap();

    // Exercise all dashboard serving modes: absent API routes must never
    // reach the embedded/disk SPA or the development sidecar.
    for mode in ["embedded", "disk", "sidecar"] {
        let mut state = AppState::new(db.clone());
        match mode {
            "disk" => state.web_dir = Some(temp.path().to_path_buf()),
            "sidecar" => state.dashboard_sidecar_url = Some("http://127.0.0.1:1".into()),
            _ => {}
        }
        let app = openproxy::build_app(state);
        for (method, path) in [("GET", "/api/version"), ("POST", "/api/version/update")] {
            for key in [None, Some(TEST_KEY), Some("invalid-key")] {
                let mut request = Request::builder().method(method).uri(path);
                if let Some(key) = key {
                    request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{mode}: {method} {path}, key={key:?}"
                );
                let content_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .unwrap()
                    .to_str()
                    .unwrap();
                assert!(!content_type.contains("text/html"), "{content_type}");
                let body = to_bytes(response.into_body(), 4096).await.unwrap();
                assert_eq!(body.as_ref(), b"Not Found");
            }
        }
    }
}
