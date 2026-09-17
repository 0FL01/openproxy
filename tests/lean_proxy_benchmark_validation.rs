use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

fn baseline() -> Value {
    let path = std::env::var_os("LEAN_BENCH_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/lean/baseline.json"));
    serde_json::from_slice(&std::fs::read(path).expect("run scripts/bench-lean to create baseline"))
        .expect("valid lean baseline JSON")
}

#[test]
fn baseline_has_complete_release_matrix_and_metadata() {
    let baseline = baseline();
    assert_eq!(baseline["schema"], "openproxy.lean-benchmark.v1");
    assert_eq!(baseline["command"], "scripts/bench-lean");
    assert_eq!(baseline["build"]["cargo_profile"], "release");
    assert_eq!(baseline["build"]["debug_assertions"], false);
    assert!(baseline["build"]["features"].as_array().unwrap().len() >= 2);
    assert_eq!(baseline["build"]["logging"]["RUST_LOG"], "off");
    assert_eq!(baseline["workload"]["external_or_paid_network"], false);
    assert_eq!(baseline["database"]["production_database_used"], false);
    assert!(baseline["database"]["size_bytes"].as_u64().unwrap() > 0);
    assert_eq!(baseline["matrix"]["repetitions"], 5);
    assert_eq!(baseline["thresholds"]["declared_before_measurement"], true);
    assert_eq!(baseline["allocation_run"]["status"], "measured");
    assert!(baseline["environment"]["kernel"].is_string());
    assert!(baseline["environment"]["cpu_model"].is_string());
    assert!(baseline["workload"]["provider_path"].is_string());
    assert!(baseline["workload"]["translator_path"].is_string());

    let results = baseline["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    let cells: BTreeSet<_> = results
        .iter()
        .map(|cell| {
            (
                cell["condition"].as_str().unwrap(),
                cell["concurrency"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        cells,
        BTreeSet::from([
            ("cold", 1),
            ("cold", 8),
            ("cold", 32),
            ("warm", 1),
            ("warm", 8),
            ("warm", 32),
        ])
    );
}

#[test]
fn baseline_metrics_are_successful_related_and_non_fabricated() {
    let baseline = baseline();
    for cell in baseline["results"].as_array().unwrap() {
        let concurrency = cell["concurrency"].as_u64().unwrap();
        let expected = concurrency * 5;
        for path_name in ["direct_mock", "full_openproxy_handler"] {
            let metrics = &cell["paths"][path_name];
            assert_eq!(metrics["request_samples"], expected);
            assert_eq!(metrics["interchunk_samples"], expected * 2);
            assert_eq!(metrics["successes"], expected);
            assert_eq!(metrics["upstream_attempts"], expected);
            assert!(metrics["throughput_requests_per_second"].as_f64().unwrap() > 0.0);
            assert_percentiles(&metrics["time_to_first_content_us"]);
            assert_percentiles(&metrics["interchunk_interval_us"]);
            assert!(metrics["allocation"].is_object());
            assert!(metrics["allocation"]["temporary_allocated_bytes_estimate"].is_u64());
            assert!(metrics["allocation"]["temporary_metric_definition"].is_string());
            assert!(metrics["allocation_unavailable_reason"].is_null());
            let memory = &metrics["memory"];
            if memory["rss_bytes"].is_null() || memory["pss_bytes"].is_null() {
                assert!(memory["unavailable_reason"].is_string());
            } else {
                assert!(memory["rss_bytes"]["peak_max"].as_u64().unwrap() > 0);
                assert!(memory["pss_bytes"]["peak_max"].as_u64().unwrap() > 0);
                assert!(memory["unavailable_reason"].is_null());
            }
        }
        let paired = &cell["paired_proxy_overhead_us"];
        assert_eq!(paired["pair_count"], expected);
        assert!(paired["derivation"]
            .as_str()
            .unwrap()
            .contains("per-request differences"));
        assert!(paired["time_to_first_content"]["p95"].is_i64());
        assert!(paired["mean_interchunk_interval"]["p95"].is_i64());
    }
}

fn assert_percentiles(value: &Value) {
    let p50 = value["p50"].as_u64().unwrap();
    let p95 = value["p95"].as_u64().unwrap();
    let p99 = value["p99"].as_u64().unwrap();
    assert!(p50 > 0);
    assert!(p50 <= p95);
    assert!(p95 <= p99);
}
