use gloo_net::http::{Request, Response};
use serde::{de::DeserializeOwned, Serialize};

pub async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    parse_json(Request::get(path).send().await.map_err(network_error)?).await
}

pub async fn send_json<B: Serialize, T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: &B,
) -> Result<T, String> {
    let builder = match method {
        "POST" => Request::post(path),
        "PATCH" => Request::patch(path),
        "PUT" => Request::put(path),
        "DELETE" => Request::delete(path),
        _ => return Err(format!("unsupported HTTP method: {method}")),
    };
    let request = builder.json(body).map_err(|error| error.to_string())?;
    parse_json(request.send().await.map_err(network_error)?).await
}

pub async fn send_empty(method: &str, path: &str) -> Result<(), String> {
    let response = match method {
        "POST" => Request::post(path),
        "PATCH" => Request::patch(path),
        "PUT" => Request::put(path),
        "DELETE" => Request::delete(path),
        _ => return Err(format!("unsupported HTTP method: {method}")),
    }
    .send()
    .await
    .map_err(network_error)?;

    if response.ok() {
        Ok(())
    } else {
        let status = response.status();
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "request failed".to_string());
        Err(format!("HTTP {status}: {message}"))
    }
}

async fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T, String> {
    if response.ok() {
        if response.status() == 204 {
            return serde_json::from_str("null").map_err(|error| error.to_string());
        }
        return response.json().await.map_err(|error| error.to_string());
    }

    let status = response.status();
    let message = response
        .text()
        .await
        .unwrap_or_else(|_| "request failed".to_string());
    Err(format!("HTTP {status}: {message}"))
}

fn network_error(error: gloo_net::Error) -> String {
    format!("network request failed: {error}")
}
