# Request TPS

Application Logs shows completed-attempt throughput next to Tokens. TPS is
original provider-generated output tokens ÷ observed upstream seconds, displayed
to one decimal (including `0.0`). The drawer preserves the original numerator and
microsecond-derived time in its formula and refreshes with the existing row poll.
Polling remains every 60 seconds on the visible first page.

Timing begins at the generation send after preparation and ends at original
protocol completion/body read before translation or downstream yield. It includes
network, initial wait and downstream backpressure: observed throughput, not GPU
decode speed. Reasoning is included when the original protocol's generated count
includes it. Unknown final usage or timing, live/error/interrupted and legacy rows
show **—**; existing Output Tokens and Duration keep their previous meanings.

Private attempt metadata retains validated final inputs rather than derived TPS.
Explicit inbound chat IDs are HMAC-SHA256 fingerprints scoped to installation
secret and authenticated API-key ID. Changing the secret or replacing the API-key
identity breaks historic linkage; shared keys and colliding IDs cannot distinguish
clients. No raw IDs, session history, body fallback or cross-request state is
introduced. Session HMAC/source, request correlation and configured connection ID
are excluded from the dashboard API/UI. The existing bounded log pipeline and
retention policy apply. See [the contract](lean-proxy.md).

## Rolling provider × model medians

`/dashboard/logs` also shows an independent table of ordinary median TPS for
rolling 1h, 6h and 24h windows. The authenticated read-only dashboard/management
endpoint is `GET /api/request-logs/tps-stats`. It returns one server `asOf`, common
half-open boundaries `[asOf − Nh, asOf)`, paginated groups, and per-window
`medianTps`, `sampleCount` and `successfulAttemptCount`. Windows use saved
**attempt-start** timestamps, not completion time. `asOf` anchors the windows;
it does not reconstruct which attempts had finished at a historical instant.

Each saved successful attempt accepted by the existing TPS validator contributes
one unrounded `generatedOutputTokens × 1,000,000 / elapsedMicros` observation.
Even-sized medians average the two middle values; explicit zero participates.
Missing, malformed, legacy and non-success observations never use old token or
duration fallbacks. A group with saved successes but no valid TPS shows **—**.
Coverage is eligible samples / saved successes in each window, not guaranteed
total traffic: existing best-effort logging and crash-loss behavior remain.

Group identities are the literal stored nullable `(provider, model)` pair;
null and empty strings remain distinct. No catalog remapping or inference of an
aggregator's internal provider/physical model is performed. Accounts and keys
pool unless `apiKeyId` is selected. Provider/key filters use equality and model
uses a case-sensitive literal substring; trimmed-empty filters are omitted.
Status filters apply only to raw logs; medians always measure successful attempts.
Group pagination defaults to page 1/size 20, with sizes 1–100. Groups with valid
24h measurements come first, then groups without measurements; each partition
is ordered by stored provider then model (null before strings). Explicit zero
is measured, and measurements only in 24h still qualify. Provider exclusion and
ordering apply before totals/pagination, independently of raw-log paging.

The optional `hiddenProviders` query is a JSON array of exact string/null provider
IDs; omission or `[]` excludes none. It is deduplicated without trimming IDs,
so null, empty, whitespace-bearing and literal `"null"` identities remain distinct.
JSON input is limited to 128 KiB and each string ID to 4 KiB (UTF-8 bytes);
invalid input returns a safe 400 without echoing supplied data. Additive
`availableProviders` contains unique null-first provider IDs from all matching
saved-success groups before exclusion/pagination, including unknown-TPS groups.
It uses saved attribution, not the current configured-provider catalog. Exclusion
is a display selection after aggregation; excluded rows still consume the same
scan/parse/retained-state budgets. Choices remain available when all are excluded.

`Hide Median TPS` remains accessible outside the panel and unmounts it, stopping
new statistics requests and cancelling browser timers/fetches. An already-running
blocking reader can still finish under its existing worker-owned permit.
Panel visibility and excluded providers persist only in this browser under
`openproxy.logs.tps-preferences.v1` (`{hidden,hiddenProviders}`), never in SQLite.
Storage errors/malformed preferences cannot break the logs page. New providers
default visible; saved exclusions survive temporary absence and filter remounts.
Provider checkboxes affect TPS only, intersect existing key/provider/model filters,
reset group paging, and remain usable during refresh/errors within that filter
scope. There is no cross-device preference synchronization.

The panel debounces filter changes, reuses Refresh/focus/visibility signals and
polls every 60 seconds after settlement while visible, including later group
pages and errors. Obsolete filter/page snapshots are hidden; a same-scope stale
snapshot retains its server time and an explicit warning. Counts are not weights
for averaging medians, and cross-model/provider TPS is not a standardized speed
benchmark.

Aggregates are computed per read from existing `requestDetails.data.upstreamTps`
in `$DATA_DIR/openproxy.sqlite`, never persisted. There are no metric tables,
snapshots, collectors, caches, migrations, indexes or upstream calls. A local
read-only SQLite connection scans the existing timestamp index with conservative
RFC3339 date padding and exact UTC comparisons, without holding the shared
writer mutex. It closes before provider/page selection and page medians are sorted.
Provider-option/exclusion lists, page keys, response vectors and label-copy
capacities are charged to the same retained-state budget; page selection uses
two map passes without cloning the population's sample vectors. One nonqueued reader
permit remains owned by the blocking worker even after HTTP cancellation.
Busy/read/resource failures return 503 `tps_stats_unavailable`, never a partial
or truncated-sample median. Rust copying/parsing and retained state are bounded;
these limits are not a hard SQLite allocation, RSS or disk-I/O deadline promise.

The engineering envelope is at most 1,000,000 coarse candidates, 128 KiB JSON
per relevant row, 4 KiB per label, 128 bytes per necessary timestamp, and 32 MiB
accounted retained state (including vector capacities and conservative group
overhead). A cooperative 5-second scan check bounds ordinary reader work between
rows, not stalled I/O. These budgets are combined: fewer rows can exceed another
limit, and filters cannot rescue the raw-candidate ceiling.

The repeatable release probe is `cargo test --release --locked
--no-default-features --lib measure_indexed_scan_with_concurrent_writes --
--ignored --nocapture`. On the implementation host, 250,000 small observations
across 100 groups took 525 ms including page medians, with 9.9 MB accounted state.
1,000 rows combining maximum JSON and label sizes took 114 ms with 8.8 MB state.
1,000,000 small rows / 100 groups and 4,500 maximum-field groups were explicitly
refused rather than sampled. Concurrent insert/finish pairs continued (observed
maximum 4.1 ms); the fresh reader had `cache_size=-2000` (2,000 KiB target). These are synthetic
fixture measurements, not production throughput, peak-RSS or latency guarantees.

Existing retention is unchanged: cleanup runs at startup and then sleeps 86400
seconds after each pass, deleting request logs older than 30 days. This is not a
midnight reset or fixed disk-size cap. SQLite reuses freed pages; its file need
not shrink immediately. Aggregate reads add no metric writes, but a reader can
temporarily pin WAL checkpoint progress while existing writers continue.
