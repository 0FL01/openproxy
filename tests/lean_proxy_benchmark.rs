#![allow(dead_code)]

#[path = "common/lean_benchmark.rs"]
mod lean_benchmark;
#[path = "common/lean_harness.rs"]
mod lean_harness;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "release benchmark; run scripts/bench-lean"]
async fn generate_lean_latency_baseline() {
    lean_benchmark::generate_latency_baseline().await.unwrap();
}
