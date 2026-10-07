//! Passive Claude quota observation from upstream response headers.
//!
//! Anthropic rides account rate-limit watermarks on every `/v1/messages`
//! response via `anthropic-ratelimit-unified-*` headers (utilization,
//! reset epoch, status per 5h/7d window). Reading them from the traffic a
//! connection already generates is the quota source for Claude
//! connections: no separate usage-endpoint polling, which scope-limited
//! grants (live setup tokens) can never authorize anyway.
//!
//! Snapshot semantics mirror the donor's quota signals: a response that
//! carries quota headers REPLACES the previous snapshot (watermarks
//! expire, merging would resurrect stale values); a response without
//! quota headers leaves it untouched.

use axum::http::HeaderMap;
use serde_json::{json, Value};

/// Extract a dashboard-shaped quota snapshot from Claude response headers.
///
/// Returns `Some({quotas, observedAt})` only when at least one utilization
/// watermark is present. `quotas` uses the same entry shape as the active
/// fetchers (`used`/`total`/`remaining`/`remainingPercentage`/`resetAt`),
/// so the existing dashboard table renders it unchanged. Utilization is
/// a 0..1 fraction; entries are reported on a 0..100 scale.
pub fn claude_quota_snapshot_from_headers(headers: &HeaderMap) -> Option<Value> {
    let mut quotas = serde_json::Map::new();

    let five_hour = quota_entry(headers, "5h");
    let seven_day = quota_entry(headers, "7d");
    if let Some(entry) = five_hour {
        quotas.insert("session (5h)".to_string(), entry);
    }
    if let Some(entry) = seven_day {
        quotas.insert("weekly (7d)".to_string(), entry);
    }
    if quotas.is_empty() {
        return None;
    }
    Some(json!({
        "quotas": Value::Object(quotas),
        "observedAt": chrono::Utc::now().to_rfc3339(),
    }))
}

/// One window entry, present only when its utilization header is.
fn quota_entry(headers: &HeaderMap, window: &str) -> Option<Value> {
    let utilization = header_f64(
        headers,
        &format!("anthropic-ratelimit-unified-{window}-utilization"),
    )?;
    // Clamp defensively: live values are 0..1 fractions.
    let used = (utilization.clamp(0.0, 1.0) * 100.0).round_to_dp(2);
    let remaining = (100.0 - used).max(0.0);
    let remaining_pct = remaining;
    let reset_at = header_str(
        headers,
        &format!("anthropic-ratelimit-unified-{window}-reset"),
    )
    .and_then(|raw| raw.trim().parse::<i64>().ok())
    .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
    .map(|time| time.to_rfc3339());
    let status = header_str(
        headers,
        &format!("anthropic-ratelimit-unified-{window}-status"),
    )
    .map(str::to_string);
    let mut entry = json!({
        "used": used,
        "total": 100.0,
        "remaining": remaining,
        "remainingPercentage": remaining_pct,
        "unlimited": false,
    });
    if let Some(reset_at) = reset_at {
        entry["resetAt"] = json!(reset_at);
    }
    if let Some(status) = status {
        entry["status"] = json!(status);
    }
    Some(entry)
}

/// Read a header as a trimmed string, tolerating multiple values (last wins
/// — Anthropic sends single values; the donor uses the same rule).
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_str(headers, name)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

/// Round to `dp` decimal places without pulling in a float-precision dep.
trait RoundToDp {
    fn round_to_dp(self, dp: u32) -> f64;
}

impl RoundToDp for f64 {
    fn round_to_dp(self, dp: u32) -> f64 {
        let factor = 10f64.powi(dp as i32);
        (self * factor).round() / factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                name.parse::<axum::http::HeaderName>().unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn snapshot_builds_from_live_unified_headers() {
        // Verbatim shape from the MITM corpus (2026-10-05/06 captures):
        // utilization is a 0..1 fraction, reset is a unix epoch.
        let map = headers(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1791217200"),
            ("anthropic-ratelimit-unified-5h-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.15"),
            ("anthropic-ratelimit-unified-7d-reset", "1791658800"),
            ("anthropic-ratelimit-unified-7d-status", "allowed"),
            ("anthropic-ratelimit-unified-status", "allowed"),
        ]);
        let snapshot = claude_quota_snapshot_from_headers(&map).expect("snapshot present");
        let quotas = snapshot["quotas"].as_object().unwrap();
        assert_eq!(quotas.len(), 2);
        let five = &quotas["session (5h)"];
        assert_eq!(five["used"], 42.0);
        assert_eq!(five["total"], 100.0);
        assert_eq!(five["remaining"], 58.0);
        assert_eq!(five["remainingPercentage"], 58.0);
        assert_eq!(five["unlimited"], false);
        assert_eq!(five["status"], "allowed");
        let reset = five["resetAt"].as_str().unwrap();
        assert!(reset.starts_with("2026"), "epoch decoded: {reset}");
        assert_eq!(quotas["weekly (7d)"]["used"], 15.0);
        assert!(snapshot["observedAt"].as_str().is_some());
    }

    #[test]
    fn snapshot_clamps_out_of_range_utilization() {
        let map = headers(&[
            ("anthropic-ratelimit-unified-5h-utilization", "1.4"),
            ("anthropic-ratelimit-unified-7d-utilization", "-0.2"),
        ]);
        let snapshot = claude_quota_snapshot_from_headers(&map).unwrap();
        let quotas = snapshot["quotas"].as_object().unwrap();
        assert_eq!(quotas["session (5h)"]["used"], 100.0);
        assert_eq!(quotas["session (5h)"]["remaining"], 0.0);
        assert_eq!(quotas["weekly (7d)"]["used"], 0.0);
        assert_eq!(quotas["weekly (7d)"]["remaining"], 100.0);
    }

    #[test]
    fn snapshot_absent_without_utilization_headers() {
        // Reset/status alone (e.g. a 429 naming only a rejected window)
        // are not utilization watermarks: no snapshot.
        let map = headers(&[
            ("anthropic-ratelimit-unified-5h-reset", "1791217200"),
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("retry-after", "2379"),
        ]);
        assert!(claude_quota_snapshot_from_headers(&map).is_none());
        // And with no quota headers at all.
        assert!(claude_quota_snapshot_from_headers(&HeaderMap::new()).is_none());
    }

    #[test]
    fn snapshot_partial_windows_ok() {
        // Only the 7d window present (aux/haiku responses sometimes omit 5h).
        let map = headers(&[("anthropic-ratelimit-unified-7d-utilization", "0.03")]);
        let snapshot = claude_quota_snapshot_from_headers(&map).unwrap();
        let quotas = snapshot["quotas"].as_object().unwrap();
        assert_eq!(quotas.len(), 1);
        assert!(quotas.contains_key("weekly (7d)"));
    }

    #[test]
    fn snapshot_ignores_garbage_values() {
        let map = headers(&[
            ("anthropic-ratelimit-unified-5h-utilization", "not-a-number"),
            ("anthropic-ratelimit-unified-7d-utilization", "nan"),
        ]);
        assert!(claude_quota_snapshot_from_headers(&map).is_none());
    }
}
