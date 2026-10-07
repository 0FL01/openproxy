//! Request-scoped transport for the quota snapshot collector. Explicit quota
//! callers keep their one-shot behavior; this scope retains no credentials or results.
use std::future::Future;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;

use reqwest::{Client, Response, StatusCode};
use serde_json::Value;

tokio::task_local! {
    static TRANSPORT: Arc<Transport>;
}

#[derive(Default)]
pub(crate) struct Observation {
    pub status: u16,
    pub retry_after: u64,
}

struct Transport {
    client: Client,
    status: AtomicU16,
    retry_after: AtomicU64,
}

pub(crate) async fn fetch_with_client(
    client: Client,
    fetch: impl Future<Output = Value>,
) -> (Value, Observation) {
    let transport = Arc::new(Transport {
        client,
        status: AtomicU16::new(0),
        retry_after: AtomicU64::new(0),
    });
    let value = TRANSPORT.scope(transport.clone(), fetch).await;
    (
        value,
        Observation {
            status: transport.status.load(Ordering::Relaxed),
            retry_after: transport.retry_after.load(Ordering::Relaxed),
        },
    )
}

pub(super) fn scoped_client() -> Option<Client> {
    TRANSPORT
        .try_with(|transport| transport.client.clone())
        .ok()
}

pub(super) fn status(response: &Response) -> StatusCode {
    let status = response.status();
    let _ = TRANSPORT.try_with(|transport| {
        transport.status.store(status.as_u16(), Ordering::Relaxed);
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            if let Some(raw) = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
            {
                let seconds = raw
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(raw).ok().map(|date| {
                            (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
                                .num_seconds()
                                .max(0) as u64
                        })
                    })
                    .unwrap_or(0);
                transport.retry_after.fetch_max(seconds, Ordering::Relaxed);
            }
        }
    });
    status
}

pub(super) async fn text(response: Response) -> Result<String, String> {
    if TRANSPORT.try_with(|_| ()).is_err() {
        return response.text().await.map_err(|error| error.to_string());
    }
    let body = crate::core::executor::read_reqwest_body(response, 512 * 1024)
        .await
        .map_err(|error| error.to_string())?;
    String::from_utf8(body.to_vec()).map_err(|error| error.to_string())
}

pub(super) async fn json(response: Response) -> Result<Value, String> {
    if TRANSPORT.try_with(|_| ()).is_err() {
        return response.json().await.map_err(|error| error.to_string());
    }
    let body = text(response).await?;
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    /// Minimal quota-shaped fetch built on the shared transport, replacing
    /// the removed claude fetch as the transport fixture.
    async fn fixture_fetch(client: Client, url: &str) -> Value {
        let response = match client.get(url).bearer_auth("fixture-token").send().await {
            Ok(response) => response,
            Err(error) => return json!({ "message": format!("Claude error: {error}") }),
        };
        let status = status(&response);
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return json!({ "message": "Invalid token" });
        }
        if !status.is_success() {
            return json!({ "message": format!("Claude quota API error ({}).", status.as_u16()) });
        }
        match json(response).await {
            Ok(value) => value,
            Err(error) => json!({ "message": format!("Claude error: {error}") }),
        }
    }

    #[tokio::test]
    async fn scoped_transport_captures_retry_after_and_bounds_provider_bodies() {
        let server = MockServer::start().await;
        let client = Client::builder()
            .user_agent("quota-transport-fixture")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let retry_at = (chrono::Utc::now() + chrono::Duration::seconds(900))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        Mock::given(method("GET"))
            .and(path("/quota"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("retry-after", retry_at.as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (_, observation) = fetch_with_client(
            client.clone(),
            fixture_fetch(client.clone(), &format!("{}/quota", server.uri())),
        )
        .await;
        assert_eq!(observation.status, 429);
        assert!((898..=900).contains(&observation.retry_after));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].headers["user-agent"].to_str().unwrap(),
            "quota-transport-fixture"
        );
        Mock::given(method("GET"))
            .and(path("/oversized"))
            .respond_with(ResponseTemplate::new(200).set_body_string(" ".repeat(512 * 1024 + 1)))
            .expect(1)
            .mount(&server)
            .await;
        let (result, _) = fetch_with_client(
            client.clone(),
            fixture_fetch(client.clone(), &format!("{}/oversized", server.uri())),
        )
        .await;
        assert!(result.get("quotas").is_none());
        assert!(result["message"]
            .as_str()
            .unwrap()
            .starts_with("Claude error:"));
        Mock::given(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/redirected", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(path("/redirected"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let (_, observation) = fetch_with_client(
            client.clone(),
            fixture_fetch(client, &format!("{}/redirect", server.uri())),
        )
        .await;
        assert_eq!(observation.status, 302);
    }
}
