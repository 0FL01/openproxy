use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Barrier};
use tokio::task::JoinHandle;

use crate::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};

const REPETITIONS: usize = 5;
const CONCURRENCIES: [usize; 3] = [1, 8, 32];
const FIRST_CHUNK_DELAY: Duration = Duration::from_millis(50);
const INTER_CHUNK_DELAY: Duration = Duration::from_millis(50);
const REQUEST_BODY: &str = r#"{"model":"lean/gpt-lean","messages":[{"role":"user","content":"hello from the lean release benchmark"}],"tools":[{"type":"function","function":{"name":"fixture_tool","description":"deterministic fixture","parameters":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}}],"tool_choice":"auto","stream":true}"#;
const RESPONSE_CHUNKS: [&[u8]; 4] = [
    b"data: {\"id\":\"chatcmpl-lean\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"alpha\"}}]}\n\n",
    b"data: {\"id\":\"chatcmpl-lean\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" beta\"}}]}\n\n",
    b"data: {\"id\":\"chatcmpl-lean\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" gamma\"}}]}\n\n",
    b"data: [DONE]\n\n",
];

#[derive(Clone, Copy)]
pub struct AllocationHooks {
    pub snapshot: fn() -> AllocationSnapshot,
    pub reset_peak: fn(),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllocationSnapshot {
    pub live_bytes: u64,
    pub peak_live_bytes: u64,
    pub allocated_bytes: u64,
    pub deallocated_bytes: u64,
    pub allocation_calls: u64,
    pub deallocation_calls: u64,
}

#[derive(Serialize)]
struct Baseline {
    schema: &'static str,
    generated_at: String,
    command: &'static str,
    build: Value,
    environment: Value,
    workload: Value,
    database: Value,
    methodology: Value,
    thresholds: Value,
    matrix: Value,
    results: Vec<CellResult>,
    allocation_run: Value,
}

#[derive(Serialize)]
struct CellResult {
    condition: &'static str,
    concurrency: usize,
    repetitions: usize,
    paths: Paths,
    paired_proxy_overhead_us: PairedOverhead,
}

#[derive(Serialize)]
struct Paths {
    direct_mock: PathMetrics,
    full_openproxy_handler: PathMetrics,
}

#[derive(Serialize)]
struct PathMetrics {
    request_samples: usize,
    interchunk_samples: usize,
    time_to_first_content_us: Percentiles,
    interchunk_interval_us: Percentiles,
    throughput_requests_per_second: f64,
    successes: usize,
    upstream_attempts: usize,
    memory: MemoryMetrics,
    allocation: Option<AllocationMetrics>,
    allocation_unavailable_reason: Option<&'static str>,
}

#[derive(Serialize)]
struct PairedOverhead {
    derivation: &'static str,
    pair_count: usize,
    time_to_first_content: SignedPercentiles,
    mean_interchunk_interval: SignedPercentiles,
}

#[derive(Serialize, Default)]
struct Percentiles {
    p50: u64,
    p95: u64,
    p99: u64,
}

#[derive(Serialize, Default)]
struct SignedPercentiles {
    p50: i64,
    p95: i64,
    p99: i64,
}

#[derive(Serialize)]
struct MemoryMetrics {
    rss_bytes: Option<MemorySummary>,
    pss_bytes: Option<MemorySummary>,
    unavailable_reason: Option<String>,
}

#[derive(Serialize)]
struct MemorySummary {
    before_p50: u64,
    after_p50: u64,
    peak_max: u64,
    peak_above_before_max: u64,
}

#[derive(Serialize)]
struct AllocationMetrics {
    allocated_bytes: u64,
    deallocated_bytes: u64,
    temporary_allocated_bytes_estimate: u64,
    temporary_metric_definition: &'static str,
    allocation_calls: u64,
    deallocation_calls: u64,
    allocated_bytes_per_request: u64,
    live_bytes_delta: i64,
    peak_live_bytes_above_start_max: u64,
}

#[derive(Default)]
struct PathAccum {
    ttfc_us: Vec<u64>,
    interchunk_us: Vec<u64>,
    elapsed: Duration,
    successes: usize,
    attempts: usize,
    memory: Vec<MemoryBatch>,
    allocations: Vec<AllocationBatch>,
}

struct RequestSample {
    ttfc_us: u64,
    interchunk_us: Vec<u64>,
}

struct BatchResult {
    samples: Vec<RequestSample>,
    elapsed: Duration,
    attempts: usize,
    memory: Option<MemoryBatch>,
    allocation: Option<AllocationBatch>,
}

#[derive(Clone, Copy)]
struct ProcMemory {
    rss: u64,
    pss: u64,
}

struct MemoryBatch {
    before: ProcMemory,
    after: ProcMemory,
    peak: ProcMemory,
}

struct AllocationBatch {
    before: AllocationSnapshot,
    after: AllocationSnapshot,
}

struct RunOutput {
    cells: Vec<CellResult>,
    database_bytes: u64,
}

struct BenchStack {
    mock: MockUpstream,
    proxy_url: String,
    proxy_shutdown: Option<oneshot::Sender<()>>,
    proxy_task: JoinHandle<()>,
    db: TempTestDb,
}

impl BenchStack {
    async fn start(response_count: usize) -> anyhow::Result<Self> {
        let scripts = (0..response_count).map(|_| benchmark_response());
        let mock = MockUpstream::start(scripts).await;
        let db = TempTestDb::new().await;
        let upstream_base = mock.url("");
        db.db
            .update(|state| {
                state.settings.require_api_key = false;
                state.provider_nodes = vec![ProviderNode {
                    id: "lean-node".into(),
                    r#type: "openai-compatible".into(),
                    name: "Lean loopback".into(),
                    prefix: Some("lean".into()),
                    api_type: Some("chat".into()),
                    base_url: Some(upstream_base.clone()),
                    ..ProviderNode::default()
                }];
                let mut connection = ProviderConnection {
                    id: "lean-node-connection".into(),
                    provider: "lean-node".into(),
                    auth_type: "apikey".into(),
                    name: Some("Lean loopback".into()),
                    priority: Some(1),
                    is_active: Some(true),
                    api_key: Some("benchmark-placeholder-key".into()),
                    default_model: Some("gpt-lean".into()),
                    backoff_level: Some(0),
                    consecutive_errors: Some(0),
                    ..ProviderConnection::default()
                };
                connection
                    .provider_specific_data
                    .insert("baseUrl".into(), Value::String(upstream_base));
                state.provider_connections = vec![connection];
            })
            .await?;

        let app: Router = openproxy::build_app(AppState::new(db.db.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (proxy_shutdown, shutdown_rx) = oneshot::channel();
        let proxy_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve benchmark OpenProxy handler");
        });

        Ok(Self {
            mock,
            proxy_url: format!("http://{address}/v1/chat/completions"),
            proxy_shutdown: Some(proxy_shutdown),
            proxy_task,
            db,
        })
    }

    fn direct_url(&self) -> String {
        self.mock.url("/direct")
    }

    fn database_bytes(&self) -> u64 {
        directory_bytes(self.db.path())
    }

    async fn validate_requests(&self, expected_direct: usize, expected_proxy: usize) {
        let requests = self.mock.requests().await;
        assert_eq!(requests.len(), expected_direct + expected_proxy);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path == "/direct")
                .count(),
            expected_direct
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path == "/chat/completions")
                .count(),
            expected_proxy
        );
        for request in requests {
            let body: Value =
                serde_json::from_slice(&request.body).expect("benchmark request JSON");
            assert_eq!(body["stream"], true);
            assert_eq!(body["tools"][0]["function"]["name"], "fixture_tool");
            let expected_model = if request.path == "/direct" {
                "lean/gpt-lean"
            } else {
                "gpt-lean"
            };
            assert_eq!(body["model"], expected_model);
        }
    }

    async fn shutdown(mut self) {
        if let Some(shutdown) = self.proxy_shutdown.take() {
            let _ = shutdown.send(());
        }
        self.proxy_task
            .await
            .expect("join benchmark OpenProxy handler");
        self.mock.shutdown().await;
    }
}

#[allow(clippy::assertions_on_constants)]
pub async fn generate_latency_baseline() -> anyhow::Result<()> {
    assert!(!cfg!(debug_assertions), "benchmark must use --release");
    let run = run_matrix(None, true).await?;
    let baseline = Baseline {
        schema: "openproxy.lean-benchmark.v1",
        generated_at: chrono::Utc::now().to_rfc3339(),
        command: "scripts/bench-lean",
        build: build_metadata(),
        environment: environment_metadata(),
        workload: workload_metadata(),
        database: json!({
            "kind": "owned temporary SQLite",
            "production_database_used": false,
            "seeded_connections": 1,
            "seeded_provider_nodes": 1,
            "size_bytes": run.database_bytes,
        }),
        methodology: methodology(),
        thresholds: thresholds(),
        matrix: json!({
            "conditions": ["cold", "warm"],
            "concurrency": CONCURRENCIES,
            "repetitions": REPETITIONS,
            "warmup_requests_per_path": 1,
        }),
        results: run.cells,
        allocation_run: json!({
            "status": "pending",
            "reason": "scripts/bench-lean runs the counting-allocator executable next"
        }),
    };
    write_json(&baseline)?;
    Ok(())
}

#[allow(clippy::assertions_on_constants)]
pub async fn add_allocation_baseline(hooks: AllocationHooks) -> anyhow::Result<()> {
    assert!(!cfg!(debug_assertions), "benchmark must use --release");
    let run = run_matrix(Some(hooks), false).await?;
    let path = output_path();
    let mut baseline: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let stored = baseline["results"]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("latency baseline results are missing"))?;
    if stored.len() != run.cells.len() {
        anyhow::bail!("latency and allocation matrices differ");
    }
    for (target, measured) in stored.iter_mut().zip(run.cells) {
        let measured = serde_json::to_value(measured)?;
        for path_name in ["direct_mock", "full_openproxy_handler"] {
            target["paths"][path_name]["allocation"] =
                measured["paths"][path_name]["allocation"].clone();
            target["paths"][path_name]["allocation_unavailable_reason"] = Value::Null;
        }
    }
    baseline["allocation_run"] = json!({
        "status": "measured",
        "mode": "separate release integration executable",
        "allocator": "std::alloc::System wrapped by atomic counting allocator",
        "scope": "request batches only; stack and temporary database setup excluded",
        "metrics": ["allocated_bytes", "deallocated_bytes", "temporary_allocated_bytes_estimate", "allocation_calls", "deallocation_calls", "live_bytes_delta", "peak_live_bytes_above_start_max"]
    });
    write_json(&baseline)?;
    Ok(())
}

async fn run_matrix(
    allocation_hooks: Option<AllocationHooks>,
    capture_memory: bool,
) -> anyhow::Result<RunOutput> {
    let mut cells = Vec::new();
    let mut database_bytes = 0;
    for condition in ["cold", "warm"] {
        for concurrency in CONCURRENCIES {
            let (cell, observed_db_bytes) =
                run_cell(condition, concurrency, allocation_hooks, capture_memory).await?;
            database_bytes = database_bytes.max(observed_db_bytes);
            cells.push(cell);
        }
    }
    Ok(RunOutput {
        cells,
        database_bytes,
    })
}

async fn run_cell(
    condition: &'static str,
    concurrency: usize,
    allocation_hooks: Option<AllocationHooks>,
    capture_memory: bool,
) -> anyhow::Result<(CellResult, u64)> {
    let mut direct = PathAccum::default();
    let mut proxy = PathAccum::default();
    let mut paired_ttfc = Vec::new();
    let mut paired_interchunk = Vec::new();
    let mut database_bytes = 0;

    if condition == "cold" {
        for _ in 0..REPETITIONS {
            let stack = BenchStack::start(concurrency * 2).await?;
            database_bytes = database_bytes.max(stack.database_bytes());
            let client = benchmark_client()?;
            let direct_batch = run_batch(
                &client,
                stack.direct_url(),
                concurrency,
                &stack.mock,
                allocation_hooks,
                capture_memory,
            )
            .await?;
            let proxy_batch = run_batch(
                &client,
                stack.proxy_url.clone(),
                concurrency,
                &stack.mock,
                allocation_hooks,
                capture_memory,
            )
            .await?;
            pair_samples(
                &direct_batch.samples,
                &proxy_batch.samples,
                &mut paired_ttfc,
                &mut paired_interchunk,
            );
            absorb(&mut direct, direct_batch);
            absorb(&mut proxy, proxy_batch);
            stack.validate_requests(concurrency, concurrency).await;
            stack.shutdown().await;
        }
    } else {
        let stack = BenchStack::start(2 + REPETITIONS * concurrency * 2).await?;
        database_bytes = stack.database_bytes();
        let client = benchmark_client()?;
        run_batch(&client, stack.direct_url(), 1, &stack.mock, None, false).await?;
        run_batch(
            &client,
            stack.proxy_url.clone(),
            1,
            &stack.mock,
            None,
            false,
        )
        .await?;
        for _ in 0..REPETITIONS {
            let direct_batch = run_batch(
                &client,
                stack.direct_url(),
                concurrency,
                &stack.mock,
                allocation_hooks,
                capture_memory,
            )
            .await?;
            let proxy_batch = run_batch(
                &client,
                stack.proxy_url.clone(),
                concurrency,
                &stack.mock,
                allocation_hooks,
                capture_memory,
            )
            .await?;
            pair_samples(
                &direct_batch.samples,
                &proxy_batch.samples,
                &mut paired_ttfc,
                &mut paired_interchunk,
            );
            absorb(&mut direct, direct_batch);
            absorb(&mut proxy, proxy_batch);
        }
        stack
            .validate_requests(1 + REPETITIONS * concurrency, 1 + REPETITIONS * concurrency)
            .await;
        stack.shutdown().await;
    }

    assert_eq!(direct.successes, REPETITIONS * concurrency);
    assert_eq!(proxy.successes, REPETITIONS * concurrency);
    assert_eq!(direct.attempts, direct.successes);
    assert_eq!(proxy.attempts, proxy.successes);
    let pair_count = paired_ttfc.len();
    Ok((
        CellResult {
            condition,
            concurrency,
            repetitions: REPETITIONS,
            paths: Paths {
                direct_mock: finish_path(direct, allocation_hooks.is_some()),
                full_openproxy_handler: finish_path(proxy, allocation_hooks.is_some()),
            },
            paired_proxy_overhead_us: PairedOverhead {
                derivation: "proxy-minus-direct per-request differences matched by repetition/request index; never percentile subtraction",
                pair_count,
                time_to_first_content: signed_percentiles(&mut paired_ttfc),
                mean_interchunk_interval: signed_percentiles(&mut paired_interchunk),
            },
        },
        database_bytes,
    ))
}

async fn run_batch(
    client: &Client,
    url: String,
    concurrency: usize,
    mock: &MockUpstream,
    allocation_hooks: Option<AllocationHooks>,
    capture_memory: bool,
) -> anyhow::Result<BatchResult> {
    assert!(url.starts_with("http://127.0.0.1:"));
    let attempts_before = mock.request_count().await;
    let memory_before = capture_memory.then(read_proc_memory).flatten();
    let sampling = Arc::new(AtomicBool::new(capture_memory && memory_before.is_some()));
    let memory_task = if let Some(initial) = memory_before {
        let sampling = sampling.clone();
        Some(tokio::spawn(async move {
            let mut peak = initial;
            while sampling.load(Ordering::Relaxed) {
                if let Some(sample) = read_proc_memory() {
                    peak.rss = peak.rss.max(sample.rss);
                    peak.pss = peak.pss.max(sample.pss);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            peak
        }))
    } else {
        None
    };
    if let Some(hooks) = allocation_hooks {
        (hooks.reset_peak)();
    }
    let allocation_before = allocation_hooks.map(|hooks| (hooks.snapshot)());

    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let body = Arc::new(REQUEST_BODY.as_bytes().to_vec());
    let mut tasks = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let client = client.clone();
        let url = url.clone();
        let barrier = barrier.clone();
        let body = body.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            run_request(&client, &url, body.as_slice()).await
        }));
    }
    barrier.wait().await;
    let batch_start = Instant::now();
    let mut samples = Vec::with_capacity(concurrency);
    for task in tasks {
        samples.push(task.await??);
    }
    let elapsed = batch_start.elapsed();

    let allocation_after = allocation_hooks.map(|hooks| (hooks.snapshot)());
    sampling.store(false, Ordering::Relaxed);
    let memory_after = memory_before.and_then(|before| {
        read_proc_memory().map(|after| MemoryBatch {
            before,
            after,
            peak: after,
        })
    });
    let memory = match (memory_after, memory_task) {
        (Some(mut batch), Some(task)) => {
            let sampled_peak = task.await?;
            batch.peak.rss = batch.peak.rss.max(sampled_peak.rss);
            batch.peak.pss = batch.peak.pss.max(sampled_peak.pss);
            Some(batch)
        }
        _ => None,
    };
    let attempts = mock.request_count().await - attempts_before;
    if attempts != concurrency {
        anyhow::bail!("expected {concurrency} upstream attempts, observed {attempts}");
    }
    Ok(BatchResult {
        samples,
        elapsed,
        attempts,
        memory,
        allocation: allocation_before
            .zip(allocation_after)
            .map(|(before, after)| AllocationBatch { before, after }),
    })
}

async fn run_request(client: &Client, url: &str, body: &[u8]) -> anyhow::Result<RequestSample> {
    let started = Instant::now();
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("user-agent", "openproxy-lean-benchmark/1")
        .body(body.to_vec())
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("benchmark response status {}", response.status());
    }
    let mut stream = response.bytes_stream();
    let mut pending = Vec::new();
    let mut content_times = Vec::new();
    let mut content = String::new();
    let mut saw_done = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let observed = started.elapsed();
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.windows(2).position(|window| window == b"\n\n") {
            let event: Vec<u8> = pending.drain(..end + 2).collect();
            let event = String::from_utf8(event)?;
            let data = event.trim().strip_prefix("data: ").unwrap_or("");
            if data == "[DONE]" {
                saw_done = true;
                continue;
            }
            let value: Value = serde_json::from_str(data)?;
            if let Some(fragment) = value["choices"][0]["delta"]["content"].as_str() {
                content.push_str(fragment);
                content_times.push(observed);
            }
        }
    }
    if content != "alpha beta gamma" || !saw_done || content_times.len() != 3 {
        anyhow::bail!(
            "invalid streaming response: content={content:?}, done={saw_done}, events={}",
            content_times.len()
        );
    }
    let ttfc_us = micros(content_times[0]);
    let interchunk_us = content_times
        .windows(2)
        .map(|times| micros(times[1] - times[0]))
        .collect();
    Ok(RequestSample {
        ttfc_us,
        interchunk_us,
    })
}

fn benchmark_response() -> ScriptedResponse {
    ScriptedResponse::sse(RESPONSE_CHUNKS.iter().copied().map(Bytes::from_static))
        .with_chunk_timing(FIRST_CHUNK_DELAY, INTER_CHUNK_DELAY)
}

fn benchmark_client() -> anyhow::Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(64)
        .build()?)
}

fn absorb(accum: &mut PathAccum, batch: BatchResult) {
    accum.elapsed += batch.elapsed;
    accum.attempts += batch.attempts;
    accum.successes += batch.samples.len();
    for sample in batch.samples {
        accum.ttfc_us.push(sample.ttfc_us);
        accum.interchunk_us.extend(sample.interchunk_us);
    }
    if let Some(memory) = batch.memory {
        accum.memory.push(memory);
    }
    if let Some(allocation) = batch.allocation {
        accum.allocations.push(allocation);
    }
}

fn pair_samples(
    direct: &[RequestSample],
    proxy: &[RequestSample],
    ttfc: &mut Vec<i64>,
    interchunk: &mut Vec<i64>,
) {
    assert_eq!(direct.len(), proxy.len());
    for (direct, proxy) in direct.iter().zip(proxy) {
        ttfc.push(proxy.ttfc_us as i64 - direct.ttfc_us as i64);
        interchunk.push(mean(&proxy.interchunk_us) as i64 - mean(&direct.interchunk_us) as i64);
    }
}

fn finish_path(mut accum: PathAccum, measured_allocations: bool) -> PathMetrics {
    let throughput = if accum.elapsed.is_zero() {
        0.0
    } else {
        accum.successes as f64 / accum.elapsed.as_secs_f64()
    };
    let memory = summarize_memory(&accum.memory);
    let allocation = measured_allocations.then(|| summarize_allocations(&accum));
    PathMetrics {
        request_samples: accum.ttfc_us.len(),
        interchunk_samples: accum.interchunk_us.len(),
        time_to_first_content_us: percentiles(&mut accum.ttfc_us),
        interchunk_interval_us: percentiles(&mut accum.interchunk_us),
        throughput_requests_per_second: throughput,
        successes: accum.successes,
        upstream_attempts: accum.attempts,
        memory,
        allocation,
        allocation_unavailable_reason: (!measured_allocations)
            .then_some("pending separate counting-allocator run"),
    }
}

fn summarize_memory(samples: &[MemoryBatch]) -> MemoryMetrics {
    if samples.is_empty() {
        return MemoryMetrics {
            rss_bytes: None,
            pss_bytes: None,
            unavailable_reason: Some(if cfg!(target_os = "linux") {
                "/proc/self/smaps_rollup could not be read".into()
            } else {
                "RSS/PSS sampling is implemented only on Linux".into()
            }),
        };
    }
    let summarize = |select: fn(ProcMemory) -> u64| {
        let mut before: Vec<u64> = samples.iter().map(|sample| select(sample.before)).collect();
        let mut after: Vec<u64> = samples.iter().map(|sample| select(sample.after)).collect();
        let peak_max = samples
            .iter()
            .map(|sample| select(sample.peak))
            .max()
            .unwrap_or(0);
        let peak_above_before_max = samples
            .iter()
            .map(|sample| select(sample.peak).saturating_sub(select(sample.before)))
            .max()
            .unwrap_or(0);
        MemorySummary {
            before_p50: percentile(&mut before, 0.50),
            after_p50: percentile(&mut after, 0.50),
            peak_max,
            peak_above_before_max,
        }
    };
    MemoryMetrics {
        rss_bytes: Some(summarize(|sample| sample.rss)),
        pss_bytes: Some(summarize(|sample| sample.pss)),
        unavailable_reason: None,
    }
}

fn summarize_allocations(accum: &PathAccum) -> AllocationMetrics {
    let allocated_bytes: u64 = accum
        .allocations
        .iter()
        .map(|sample| sample.after.allocated_bytes - sample.before.allocated_bytes)
        .sum();
    let deallocated_bytes: u64 = accum
        .allocations
        .iter()
        .map(|sample| sample.after.deallocated_bytes - sample.before.deallocated_bytes)
        .sum();
    let allocation_calls: u64 = accum
        .allocations
        .iter()
        .map(|sample| sample.after.allocation_calls - sample.before.allocation_calls)
        .sum();
    let deallocation_calls: u64 = accum
        .allocations
        .iter()
        .map(|sample| sample.after.deallocation_calls - sample.before.deallocation_calls)
        .sum();
    let live_bytes_delta: i128 = accum
        .allocations
        .iter()
        .map(|sample| sample.after.live_bytes as i128 - sample.before.live_bytes as i128)
        .sum();
    let peak_live_bytes_above_start_max = accum
        .allocations
        .iter()
        .map(|sample| {
            sample
                .after
                .peak_live_bytes
                .saturating_sub(sample.before.live_bytes)
        })
        .max()
        .unwrap_or(0);
    AllocationMetrics {
        allocated_bytes,
        deallocated_bytes,
        temporary_allocated_bytes_estimate: allocated_bytes
            .saturating_sub(live_bytes_delta.max(0) as u64),
        temporary_metric_definition: "allocated bytes minus positive end-of-batch live-byte delta; an aggregate non-retained estimate, not pointer lifetime tracking",
        allocation_calls,
        deallocation_calls,
        allocated_bytes_per_request: allocated_bytes / accum.successes.max(1) as u64,
        live_bytes_delta: live_bytes_delta.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        peak_live_bytes_above_start_max,
    }
}

fn percentiles(values: &mut [u64]) -> Percentiles {
    Percentiles {
        p50: percentile(values, 0.50),
        p95: percentile(values, 0.95),
        p99: percentile(values, 0.99),
    }
}

fn signed_percentiles(values: &mut [i64]) -> SignedPercentiles {
    values.sort_unstable();
    SignedPercentiles {
        p50: signed_percentile(values, 0.50),
        p95: signed_percentile(values, 0.95),
        p99: signed_percentile(values, 0.99),
    }
}

fn percentile(values: &mut [u64], quantile: f64) -> u64 {
    values.sort_unstable();
    values
        .get(percentile_index(values.len(), quantile))
        .copied()
        .unwrap_or(0)
}

fn signed_percentile(values: &[i64], quantile: f64) -> i64 {
    values
        .get(percentile_index(values.len(), quantile))
        .copied()
        .unwrap_or(0)
}

fn percentile_index(len: usize, quantile: f64) -> usize {
    if len == 0 {
        0
    } else {
        ((len as f64 * quantile).ceil() as usize)
            .saturating_sub(1)
            .min(len - 1)
    }
}

fn mean(values: &[u64]) -> u64 {
    values.iter().sum::<u64>() / values.len().max(1) as u64
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

fn read_proc_memory() -> Option<ProcMemory> {
    #[cfg(target_os = "linux")]
    {
        let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
        let mut values = BTreeMap::new();
        for line in rollup.lines() {
            let mut fields = line.split_whitespace();
            let key = fields.next()?.trim_end_matches(':');
            if matches!(key, "Rss" | "Pss") {
                values.insert(key, fields.next()?.parse::<u64>().ok()? * 1024);
            }
        }
        Some(ProcMemory {
            rss: *values.get("Rss")?,
            pss: *values.get("Pss")?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn directory_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn output_path() -> PathBuf {
    std::env::var_os("LEAN_BENCH_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/lean/baseline.json"))
}

fn write_json(value: &impl Serialize) -> anyhow::Result<()> {
    let output = output_path();
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(output, bytes)?;
    Ok(())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn build_metadata() -> Value {
    json!({
        "cargo_profile": "release",
        "debug_assertions": cfg!(debug_assertions),
        "features": if cfg!(feature = "embed-web") { vec!["default", "embed-web"] } else { Vec::<&str>::new() },
        "release_profile": {"lto": "thin", "codegen_units": 1, "strip": "symbols", "panic": "abort", "opt_level": "s"},
        "rustc": command_output("rustc", &["-Vv"]),
        "cargo": command_output("cargo", &["-V"]),
        "logging": {"RUST_LOG": std::env::var("RUST_LOG").unwrap_or_else(|_| "unset".into()), "subscriber": "not installed by benchmark", "request_logging_code_path": "enabled"},
        "runtime_threads": "Tokio test runtime defaults; unchanged",
    })
}

fn environment_metadata() -> Value {
    let opencode = command_output("opencode", &["--version"]);
    let opencode_reason = opencode
        .is_none()
        .then_some("opencode executable not found or --version failed");
    let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_string))
        });
    json!({
        "kernel": command_output("uname", &["-srmo"]),
        "cpu_model": cpu_model,
        "logical_cpus": std::thread::available_parallelism().ok().map(|count| count.get()),
        "opencode_version": opencode,
        "opencode_version_unavailable_reason": opencode_reason,
        "git_commit": command_output("git", &["rev-parse", "HEAD"]),
        "git_worktree_dirty": command_output("git", &["status", "--porcelain"]).is_some_and(|status| !status.is_empty()),
    })
}

fn workload_metadata() -> Value {
    json!({
        "client": "reqwest 0.12 release integration executable",
        "network": "127.0.0.1 ephemeral listeners only",
        "external_or_paid_network": false,
        "request_bytes": REQUEST_BODY.len(),
        "upstream_response_bytes": RESPONSE_CHUNKS.iter().map(|chunk| chunk.len()).sum::<usize>(),
        "stream_content_events": 3,
        "first_chunk_delay_us": FIRST_CHUNK_DELAY.as_micros(),
        "inter_chunk_delay_us": INTER_CHUNK_DELAY.as_micros(),
        "features": {"streaming": true, "tools": true, "tool_choice": "auto", "images": false, "reasoning": false},
        "provider_path": "lean prefix -> lean-node openai-compatible -> loopback /chat/completions",
        "translator_path": "OpenAI chat input -> OpenAI-compatible identity mapping; streamed OpenAI SSE -> OpenAI SSE",
        "success_contract": "HTTP 2xx; exactly alpha beta gamma across three content events; terminal [DONE]",
    })
}

fn methodology() -> Value {
    json!({
        "cold": "fresh temporary SQLite, AppState, handler listener, clients, and upstream listener for each repetition; request timing excludes setup",
        "warm": "one direct and one handler request before five measured repetitions; clients, transports, AppState, and SQLite are reused",
        "timing": "monotonic Instant from client send start; TTFC ends at first parsed non-empty delta.content; interchunk intervals are between parsed content events from the same request",
        "pairing": "direct and proxy requests are paired by repetition and request index; overhead percentiles are computed from those per-request differences",
        "throughput": "successful measured requests divided by summed measured batch wall time",
        "memory": "Linux /proc/self/smaps_rollup before/after each batch and every 1 ms during the batch; peak is process-wide and includes the benchmark harness",
        "allocation": "separate release executable with a System counting allocator; snapshots bracket request batches and exclude stack/database setup; temporary_allocated_bytes_estimate is allocated bytes minus positive end-of-batch live-byte delta",
        "attempt_check": "mock request-count deltas must equal successful client requests for every batch",
    })
}

fn thresholds() -> Value {
    json!({
        "declared_before_measurement": true,
        "noise": {
            "latency_relative_percent": 10,
            "throughput_relative_percent": 10,
            "memory_relative_percent": 5,
            "allocation_relative_percent": 5
        },
        "regression": {
            "ttfc_p95": {"max_relative_percent": 15, "absolute_allowance_us": 250},
            "interchunk_p95": {"max_relative_percent": 15, "absolute_allowance_us": 250},
            "throughput": {"max_decrease_percent": 10},
            "rss_pss_peak": {"max_relative_percent": 10, "absolute_allowance_bytes": 1048576},
            "allocated_bytes_per_request": {"max_relative_percent": 10, "absolute_allowance_bytes": 65536},
            "successes": "must equal request samples",
            "upstream_attempts": "must equal successes"
        }
    })
}
