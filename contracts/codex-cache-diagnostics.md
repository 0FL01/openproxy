# Passive Codex cache diagnostics

Generation attempts persist the actual configured `connectionId` and private
`requestDetails.data.codexCache`; there is no migration, extra request, warming,
prompt storage, replay, or dashboard/API field. Search and compact are excluded.
The normal insert/finish pipeline, queue bounds, drop counters and 30-day
retention still apply. Interrupted attempts retain observations already seen;
process crashes and queue drops can still lose them.

Version 1 contains the effective sent model, optional `accountHmac` (actual
`chatgpt-account-id`) and `cacheKeyHmac` (actual `prompt_cache_key`), an
`envelopeHmac` of the actual endpoint URL plus all sent fields except input,
`inputCount`, and at most 16
`inputPrefixes` entries `{count, hmac}`. Each count means the number of included
items. HMAC-SHA256 uses the existing per-install API-key secret with separate
versioned domains. Input is serialized once, item by item, using the send JSON
serializer, followed by a NUL separator (not present literally in valid JSON).
No semantic normalization is applied. Hashes reveal equality, not content:
this is pseudonymization, not anonymity. Rotating the secret breaks comparisons.
Models longer than 512 bytes omit diagnostics rather than truncate identity.

`sentAt` is local send start after preparation/fingerprinting, not upstream
receipt. `completedAt` is local recognition of original `response.completed`,
before translation/yield; EOF, failed/incomplete and cancellation do not set it.
Legacy collected dashboard/plain-response paths recognize events only after
their existing bounded body collection; cancellation during that collection
leaves usage/completion unknown. Native and forced-SSE paths observe incrementally.
Original `inputTokens`, `cachedTokens`, and `cacheWriteTokens` (only explicit
`cache_creation_input_tokens`) are nullable. Repeated cumulative usage replaces,
never sums; missing/null fields do not erase observations and zero is preserved.
No arbitrary usage extras, raw IDs, headers, credentials or response text persist.

## Read-only analysis

Compare only the same connection, known account HMAC, model, envelope/cache key
and fingerprint version. A previous full-input digest matching the same count
boundary in a later input proves preservation of its serialized input, not an
upstream token-prefix or cache-routing guarantee. At most 15 appended items are
checkable; an absent boundary, rewritten input or unknown account is uncertainty,
not evidence of expiry. The last boundary already hashes the entire input.

Inspect **all** sent attempts for intervening shared-prefix use, including rows
without completion/usage, concurrent siblings and ambiguous predecessors.
Report both send-to-send and completion-to-send intervals and both requests'
input/cached counts. `cached(B)/input(A)` may exceed one and is not exact retained
cache coverage. Earlier nonzero cached usage proves only some prefix was warm.
Show exclusions and logging gaps. Requests outside this proxy remain unknown.
These data can establish observed reuse after pauses, not exact upstream TTL or
whether a miss was caused by expiration versus routing/eviction. A first quality
check after 2–3 days is useful; insufficient comparable traffic is a valid result.
