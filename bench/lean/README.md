# Lean Proxy Release Benchmark

Run from the repository root:

```bash
scripts/bench-lean
```

The command builds and runs the deterministic C02 matrix with Cargo's `release`
profile and default features. It uses only owned temporary SQLite databases and
ephemeral `127.0.0.1` listeners. The first executable records normal latency,
throughput, RSS, and PSS. A separate executable repeats the same matrix with a
counting wrapper around `std::alloc::System` and adds live/temporary allocation
metrics. The final command validates `bench/lean/baseline.json`.

`RUST_LOG=off` is fixed by the script. The benchmark still traverses the normal
request-logging code path; it does not install a tracing subscriber. Do not use
the generated numbers as a live-provider benchmark: the upstream is the C01
scripted loopback mock, with fixed first-content and inter-chunk delays.

The artifact declares noise and regression thresholds before its measured
results. Proxy overhead percentiles are derived from matched per-request
direct/proxy differences, not by subtracting independently aggregated
percentiles.
