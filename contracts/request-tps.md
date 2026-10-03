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
