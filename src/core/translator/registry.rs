//! Translation registry — mirrors open-sse/translator/index.js
//!
//! Provides a registry-backed translation system for request and response transforms.
//! The pipeline is: source format -> OpenAI intermediate -> target format.
//!
//! This module does NOT include the actual transform implementations.
//! Those live in request_transform.rs (to be filled by Phase 2 translator beads)
//! and response_transform.rs (already partially implemented).

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::limits::{
    wire_index, StreamLimitError, MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_CHOICES,
    MAX_STREAM_TOOL_ARGUMENT_BYTES, MAX_STREAM_TOOL_CALLS,
};

/// Valid OpenAI content block types (mirrors VALID_OPENAI_CONTENT_TYPES in schema/blocks.js).
const VALID_OPENAI_CONTENT_TYPES: &[&str] = &[
    "text",
    "image_url",
    "image",
    "input_audio",
    "audio_url",
    "refusal",
];

/// Valid OpenAI message-level roles (mirrors VALID_OPENAI_MESSAGE_TYPES).
const VALID_OPENAI_MESSAGE_TYPES: &[&str] = &["system", "user", "assistant", "tool"];

/// All supported translation formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    OpenAi,
    OpenAiResponses,
    OpenAiResponse,
    Claude,
    Gemini,
    Vertex,
    Codex,
    Antigravity,
    Ollama,
}

impl Format {
    /// Incremental text framing used on this format's streaming wire.
    pub fn text_stream_mode(
        self,
        content_type: Option<&str>,
    ) -> Option<crate::core::stream_framing::TextStreamMode> {
        use crate::core::stream_framing::TextStreamMode;

        match self {
            Self::Ollama => Some(TextStreamMode::Lines),
            Self::OpenAi
            | Self::OpenAiResponses
            | Self::OpenAiResponse
            | Self::Claude
            | Self::Gemini
            | Self::Vertex
            | Self::Codex
            | Self::Antigravity => Some(TextStreamMode::Sse),
        }
    }

    /// Parse from string (used for registry key lookups).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Self::OpenAi),
            "openai-responses" | "openaiResponses" => Some(Self::OpenAiResponses),
            "openai-response" => Some(Self::OpenAiResponse),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "vertex" => Some(Self::Vertex),
            "codex" => Some(Self::Codex),
            "antigravity" => Some(Self::Antigravity),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiResponse => "openai-response",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Vertex => "vertex",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Ollama => "ollama",
        }
    }

    /// Returns true if this format supports remote image URLs natively.
    pub fn is_openai(&self) -> bool {
        matches!(
            self,
            Self::OpenAi | Self::OpenAiResponses | Self::OpenAiResponse
        )
    }

    /// Returns true if this format needs images as inline base64
    /// rather than supporting remote HTTP URLs (9router TARGETS_NEED_BASE64 parity).
    pub fn needs_image_prefetch(&self) -> bool {
        matches!(
            self,
            Self::Gemini | Self::Vertex | Self::Ollama | Self::Antigravity
        )
    }
}

/// Request transform signature: (model, body, stream) -> transformed_body
pub type RequestTransformFn =
    fn(model: &str, body: &mut Value, stream: bool, credentials: Option<&Value>) -> bool;

/// Response transform signature: (chunk, state) -> Vec<String>
/// Returns SSE lines to emit.
pub type ResponseTransformFn = fn(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String>;

/// Shared state for response streaming transforms.
/// Each format has its own state variant tracked here.
#[derive(Debug, Clone, Default)]
pub struct ResponseTransformState {
    /// Shared incremental source framer used by registry callers. The chat
    /// stream dispatcher uses the same abstraction externally and calls the
    /// payload-only transform entry point, so source bytes are scanned once.
    pub text_framer: Option<crate::core::stream_framing::TextStreamFramer>,
    /// OpenAI SSE state
    pub openai: OpenAiResponseState,
    /// Anthropic SSE state
    pub anthropic: AnthropicResponseState,
    /// Gemini SSE state
    pub gemini: GeminiResponseState,
    /// Responses API state
    pub responses: ResponsesResponseState,
    /// Ollama streaming state
    pub ollama: OllamaResponseState,
    /// Generic scratch map for Value-based response transforms
    /// (openai→claude, openai→antigravity, chat→responses, etc.).
    pub generic: serde_json::Map<String, Value>,
    /// Shared accounting for state retained by active response translators.
    pub accumulation: ResponseAccumulationBudget,
    /// Terminal pre-emission transform failure for the registry caller.
    pub failure: Option<StreamLimitError>,
}

#[derive(Debug, Clone, Default)]
pub struct ResponseAccumulationBudget {
    retained_bytes: usize,
    choices: BTreeSet<(u8, u64)>,
    tools: BTreeSet<(u8, u64, u64)>,
    tool_argument_bytes: BTreeMap<(u8, u64, u64), usize>,
}

impl ResponseAccumulationBudget {
    pub fn track_choice(
        &mut self,
        namespace: u8,
        choice_index: u64,
    ) -> Result<(), StreamLimitError> {
        let key = (namespace, choice_index);
        let namespace_choices = self
            .choices
            .iter()
            .filter(|(stored_namespace, _)| *stored_namespace == namespace)
            .count();
        if !self.choices.contains(&key) && namespace_choices >= MAX_STREAM_CHOICES {
            return Err(StreamLimitError::too_many(
                "response choices",
                MAX_STREAM_CHOICES,
            ));
        }
        self.choices.insert(key);
        Ok(())
    }

    pub fn track_tool(
        &mut self,
        namespace: u8,
        choice_index: u64,
        tool_index: u64,
    ) -> Result<(), StreamLimitError> {
        let key = (namespace, choice_index, tool_index);
        let namespace_tools = self
            .tools
            .iter()
            .filter(|(stored_namespace, _, _)| *stored_namespace == namespace)
            .count();
        if !self.tools.contains(&key) && namespace_tools >= MAX_STREAM_TOOL_CALLS {
            return Err(StreamLimitError::too_many(
                "tool calls",
                MAX_STREAM_TOOL_CALLS,
            ));
        }
        self.tools.insert(key);
        Ok(())
    }

    pub fn track_tool_arguments(
        &mut self,
        namespace: u8,
        choice_index: u64,
        tool_index: u64,
        bytes: usize,
    ) -> Result<(), StreamLimitError> {
        self.track_tool(namespace, choice_index, tool_index)?;
        let key = (namespace, choice_index, tool_index);
        let current = self.tool_argument_bytes.get(&key).copied().unwrap_or(0);
        let next = current
            .checked_add(bytes)
            .ok_or_else(|| StreamLimitError::arithmetic("tool arguments"))?;
        if next > MAX_STREAM_TOOL_ARGUMENT_BYTES {
            return Err(StreamLimitError::bytes(
                "tool arguments",
                MAX_STREAM_TOOL_ARGUMENT_BYTES,
            ));
        }
        self.track_retained(bytes, "tool arguments")?;
        self.tool_argument_bytes.insert(key, next);
        Ok(())
    }

    pub fn track_retained(&mut self, bytes: usize, kind: &str) -> Result<(), StreamLimitError> {
        let next = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or_else(|| StreamLimitError::arithmetic(kind))?;
        if next > MAX_STREAM_ACCUMULATED_BYTES {
            return Err(StreamLimitError::bytes(
                "retained state",
                MAX_STREAM_ACCUMULATED_BYTES,
            ));
        }
        self.retained_bytes = next;
        Ok(())
    }
}

impl ResponseTransformState {
    pub fn fail(&mut self, error: StreamLimitError) -> Vec<String> {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        Vec::new()
    }
}

/// Validate OpenAI wire indices and account for state retained by translators
/// before those translators allocate or append to their maps and strings.
pub fn track_openai_accumulation(
    state: &mut ResponseTransformState,
    chunk: &Value,
    namespace: u8,
    retain_text_and_reasoning: bool,
) -> Result<(), StreamLimitError> {
    let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
        return Ok(());
    };
    for choice in choices {
        let choice_index = wire_index(choice.get("index"), "choices[].index")?;
        state.accumulation.track_choice(namespace, choice_index)?;
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if retain_text_and_reasoning {
            for key in ["content", "reasoning_content", "reasoning"] {
                if let Some(text) = delta.get(key).and_then(Value::as_str) {
                    state.accumulation.track_retained(text.len(), key)?;
                }
            }
            if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
                for detail in details {
                    let text = detail.as_str().or_else(|| {
                        detail
                            .get("text")
                            .or_else(|| detail.get("content"))
                            .and_then(Value::as_str)
                    });
                    if let Some(text) = text {
                        state.accumulation.track_retained(text.len(), "reasoning")?;
                    }
                }
            }
        }
        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for tool_call in tool_calls {
            let tool_index = wire_index(tool_call.get("index"), "tool_calls[].index")?;
            state
                .accumulation
                .track_tool(namespace, choice_index, tool_index)?;
            if let Some(arguments) = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
            {
                state.accumulation.track_tool_arguments(
                    namespace,
                    choice_index,
                    tool_index,
                    arguments.len(),
                )?;
            }
            let id = tool_call.get("id").and_then(Value::as_str);
            let name = tool_call.pointer("/function/name").and_then(Value::as_str);
            if (id.is_some() || name.is_some())
                && (!id.is_some_and(|value| !value.is_empty())
                    || !name.is_some_and(|value| !value.is_empty()))
            {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message:
                        "Upstream tool call declaration requires a non-empty id and function name"
                            .to_string(),
                });
            }
            for value in [id, name].into_iter().flatten() {
                state
                    .accumulation
                    .track_retained(value.len(), "tool metadata")?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct OpenAiResponseState {}

#[derive(Debug, Clone, Default)]
pub struct AnthropicResponseState {
    pub current_block_index: Option<usize>,
    pub text_block_open: bool,
    pub in_thinking: bool,
    pub cache_lookaheads: Vec<String>,
    pub message_id: Option<String>,
    pub model: Option<String>,
    /// Dynamic state used by claude_to_openai_response streaming transform
    pub claude_state: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct GeminiResponseState {
    pub current_part_index: usize,
    /// Accumulated tool call data: tool-call-index -> {id, name, arguments_buf}
    pub tool_calls_accum: serde_json::Map<String, Value>,
    /// Response ID extracted from the first OpenAI SSE chunk
    pub response_id: String,
    /// Model name extracted from the first OpenAI SSE chunk
    pub model: String,
    /// Whether we have already emitted a finish chunk (guard against duplicates)
    pub finish_emitted: bool,
    /// Dynamic state used by gemini_to_openai_response streaming transform
    pub gemini_state: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ResponsesResponseState {
    pub seq: usize,
    pub func_names: std::collections::HashMap<usize, String>,
    pub func_call_ids: std::collections::HashMap<usize, String>,
    pub msg_item_done: std::collections::HashMap<usize, bool>,
    pub completed_sent: bool,
    /// Generic state used by responses_to_chat_response (OpenAiResponses -> OpenAi).
    pub state: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct OllamaResponseState {
    pub message_idx: usize,
    /// Generic state used by ollama_to_openai_response.
    pub state: std::collections::HashMap<String, Value>,
}

/// Detect source format from request body structure.
/// Mirrors open-sse/services/provider.js:detectFormat() order carefully:
/// Responses (input array|string && !messages) → Antigravity → Gemini contents[]
/// → OpenAI-specific fields → Claude heuristics → default OpenAI.
pub fn detect_source_format(body: &Value) -> Format {
    // 1. OpenAI Responses API: input as array or string, and no messages
    //    (JS requires !body.messages — bodies with both stay non-responses)
    if let Some(input) = body.get("input") {
        let input_ok = input.is_array() || input.is_string();
        // JS: !body.messages — any messages key blocks responses detection
        if input_ok && body.get("messages").is_none() {
            return Format::OpenAiResponses;
        }
    }

    // 2. Antigravity format: Gemini wrapped in body.request
    if body
        .get("request")
        .and_then(|r| r.get("contents"))
        .is_some()
        && body
            .get("userAgent")
            .and_then(Value::as_str)
            .is_some_and(|ua| ua == "antigravity")
    {
        return Format::Antigravity;
    }

    // 3. Gemini format: contents must be an array (JS)
    if body.get("contents").and_then(Value::as_array).is_some() {
        return Format::Gemini;
    }

    // 4. OpenAI-specific indicators BEFORE Claude (9router order)
    if body.get("stream_options").is_some()
        || body.get("response_format").is_some()
        || body.get("logprobs").is_some()
        || body.get("top_logprobs").is_some()
        || body.get("n").is_some()
        || body.get("presence_penalty").is_some()
        || body.get("frequency_penalty").is_some()
        || body.get("logit_bias").is_some()
        || body.get("user").is_some()
    {
        return Format::OpenAi;
    }

    // 5. Claude-specific indicators
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        if body.get("system").is_some() || body.get("anthropic_version").is_some() {
            return Format::Claude;
        }
        if let Some(first) = messages.first() {
            if let Some(content) = first.get("content").and_then(Value::as_array) {
                for part in content {
                    let t = part.get("type").and_then(Value::as_str);
                    if t == Some("tool_use") || t == Some("tool_result") {
                        return Format::Claude;
                    }
                    // Claude image: source.type === base64 (JS)
                    if t == Some("image")
                        && part
                            .get("source")
                            .and_then(|s| s.get("type"))
                            .and_then(Value::as_str)
                            == Some("base64")
                    {
                        return Format::Claude;
                    }
                    if t == Some("image_url") {
                        return Format::OpenAi;
                    }
                }
            }
        }
    }

    // 6. Default to OpenAI
    Format::OpenAi
}

/// Detect source format from endpoint path (+ optional body for Responses-shaped input).
/// Mirrors open-sse/translator/formats.js:detectFormatByEndpoint.
pub fn detect_source_format_by_endpoint(path: &str) -> Option<Format> {
    detect_source_format_by_endpoint_with_body(path, None)
}

/// Body-aware endpoint detection (/v1/chat/completions + input[] → openai).
pub fn detect_source_format_by_endpoint_with_body(
    path: &str,
    body: Option<&Value>,
) -> Option<Format> {
    if path.contains("/v1/responses") {
        return Some(Format::OpenAiResponses);
    }
    if path.contains("/v1/messages") {
        return Some(Format::Claude);
    }
    // Responses-shaped `input` on chat/completions — force OpenAI
    if path.contains("/v1/chat/completions") {
        if let Some(b) = body {
            if b.get("input").and_then(Value::as_array).is_some() {
                return Some(Format::OpenAi);
            }
        }
    }
    None
}

/// Get the default target format for a provider.
/// Mirrors open-sse/services/provider.js:getTargetFormat() including
/// openai-compatible-* and anthropic-compatible-* prefixes.
pub fn get_target_format_for_provider(provider: &str) -> Format {
    if provider.starts_with("openai-compatible") {
        return if provider.contains("responses") {
            Format::OpenAiResponses
        } else {
            Format::OpenAi
        };
    }
    if provider.starts_with("anthropic-compatible") {
        return Format::Claude;
    }
    match provider {
        "openai" => Format::OpenAi,
        "anthropic" | "claude" | "kimi" | "minimax" | "kimi-coding" => Format::Claude,
        "glm" => Format::OpenAi,
        "gemini" => Format::Gemini,
        "vertex" | "vertex-partner" => Format::Vertex,
        "codex" | "perplexity-agent" => Format::OpenAiResponses,
        "ollama" | "ollama-cloud" => Format::Ollama,
        "antigravity" => Format::Antigravity,
        _ => Format::OpenAi,
    }
}

/// Translation registry for request and response transforms.
#[derive(Default)]
pub struct TranslationRegistry {
    /// Request transforms: (source_format, target_format) -> transform_fn
    request_transforms: HashMap<(Format, Format), RequestTransformFn>,
    /// Response transforms: (source_format, target_format) -> transform_fn
    response_transforms: HashMap<(Format, Format), ResponseTransformFn>,
}

#[derive(Debug, Default)]
pub struct ResponseTranslationBatch {
    pub chunks: Vec<String>,
    pub error: Option<StreamLimitError>,
}

impl TranslationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request transform.
    pub fn register_request(&mut self, from: Format, to: Format, f: RequestTransformFn) {
        self.request_transforms.insert((from, to), f);
    }

    /// Register a response transform.
    pub fn register_response(&mut self, from: Format, to: Format, f: ResponseTransformFn) {
        self.response_transforms.insert((from, to), f);
    }

    /// Check if a request transform exists.
    pub fn has_request_transform(&self, from: Format, to: Format) -> bool {
        self.request_transforms.contains_key(&(from, to))
    }

    /// Check if a response transform exists.
    pub fn has_response_transform(&self, from: Format, to: Format) -> bool {
        self.response_transforms.contains_key(&(from, to))
    }

    /// Apply request transform with 9router parity:
    /// 1. Direct route if `source:target` is registered
    /// 2. Else pivot source→OpenAI→target
    /// 3. Normalization + target-specific hooks (filter OpenAI / prepare Claude)
    ///
    /// `source` = client format, `target` = provider format.
    pub fn translate_request(
        &self,
        source: Format,
        target: Format,
        model: &str,
        body: &mut Value,
        stream: bool,
        credentials: Option<&Value>,
    ) -> bool {
        self.translate_request_with_strip(source, target, model, body, stream, credentials, None)
    }

    /// Seed Responses→OpenAI streaming state from a request body that
    /// carried translator-only `_customToolNames` metadata (9router chatCore
    /// threads `customToolNames` into the response path).
    pub fn seed_custom_tool_names(state: &mut ResponseTransformState, body: &Value) {
        if let Some(names) = body.get("_customToolNames").and_then(Value::as_array) {
            let joined = names
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(",");
            if !joined.is_empty() {
                state
                    .responses
                    .state
                    .insert("customToolNames".to_string(), Value::String(joined));
            }
        }
    }

    /// Like [`translate_request`] but applies optional content-type strip list
    /// (9router `stripList`) before normalization.
    pub fn translate_request_with_strip(
        &self,
        source: Format,
        target: Format,
        model: &str,
        body: &mut Value,
        stream: bool,
        credentials: Option<&Value>,
        strip_list: Option<&[&str]>,
    ) -> bool {
        if source != target {
            // Direct route: exact source→target pair
            if let Some(transform) = self.request_transforms.get(&(source, target)) {
                tracing::debug!(
                    target: "openproxy::translator",
                    "route=direct request {}→{}",
                    source.as_str(),
                    target.as_str()
                );
                let _ = transform(model, body, stream, credentials);
            } else {
                tracing::debug!(
                    target: "openproxy::translator",
                    "route=pivot request {}→openai→{}",
                    source.as_str(),
                    target.as_str()
                );
                // Step 1: source -> OpenAI intermediate
                if source != Format::OpenAi {
                    if let Some(transform) = self.request_transforms.get(&(source, Format::OpenAi))
                    {
                        let _ = transform(model, body, stream, credentials);
                    }
                }
                // Step 2: OpenAI intermediate -> target
                if target != Format::OpenAi {
                    if let Some(transform) = self.request_transforms.get(&(Format::OpenAi, target))
                    {
                        let _ = transform(model, body, stream, credentials);
                    }
                }
            }
            // 9router chatCore.js:198-199: _customToolNames is translator-only
            // metadata for the response conversion — strip it from the
            // translated OUTPUT, not the caller's input. Passthrough
            // (source==target) leaves the body untouched so non-translated
            // paths keep the metadata.
            if let Some(obj) = body.as_object_mut() {
                obj.remove("_customToolNames");
            }
        }

        if let Some(list) = strip_list {
            strip_content_types(body, list);
        }

        apply_normalization_hooks(body);

        // Target-format post-hooks (9router translator/index.js:124-128).
        // preserveCacheControl follows the provider quirk (alicode / alicode-intl
        // / alims-intl carry quirks.preserveCacheControl:true).
        if target == Format::OpenAi || target == Format::OpenAiResponses || target == Format::Codex
        {
            let provider = credentials
                .and_then(|c| c.get("provider").and_then(Value::as_str))
                .unwrap_or("");
            let preserve = matches!(
                provider,
                "alicode" | "alicode-intl" | "alims-intl" | "alitp-intl"
            );
            filter_to_openai_format(body, preserve);
        }
        if target == Format::Claude {
            let provider = credentials
                .and_then(|c| c.get("provider").and_then(Value::as_str))
                .unwrap_or("claude");
            crate::core::translator::request::claude_format::prepare_claude_request(body, provider);
        }

        true
    }

    /// Apply response transform with 9router parity.
    ///
    /// Parameter naming matches chat.rs / JS: `source` = provider (upstream)
    /// format, `target` = client format.
    ///
    /// 1. Direct route if `source:target` registered
    /// 2. Else provider→OpenAI into intermediates, then each intermediate → client
    pub fn translate_response(
        &self,
        source: Format,
        target: Format,
        chunk: &[u8],
        state: &mut ResponseTransformState,
    ) -> Result<Vec<String>, StreamLimitError> {
        let mut batch = self.translate_response_batch(source, target, chunk, state);
        if let Some(error) = batch.error {
            if batch.chunks.is_empty() {
                return Err(error);
            }
            batch.chunks.push(stream_error_event(&error));
        }
        Ok(batch.chunks)
    }

    /// Frame source bytes and retain both completed output and a later terminal
    /// framing/semantic error. This is needed when one transport chunk contains
    /// a valid event followed by an oversized incomplete event.
    pub fn translate_response_batch(
        &self,
        source: Format,
        target: Format,
        chunk: &[u8],
        state: &mut ResponseTransformState,
    ) -> ResponseTranslationBatch {
        if let Some(error) = state.failure.clone() {
            return ResponseTranslationBatch {
                chunks: Vec::new(),
                error: Some(error),
            };
        }

        let no_pending_frame = state
            .text_framer
            .as_ref()
            .is_none_or(|framer| framer.pending_len() == 0);
        let complete_bare_json = no_pending_frame
            && serde_json::from_slice::<Value>(chunk)
                .is_ok_and(|value| value.is_object() || value.is_array());
        let Some(mode) = source.text_stream_mode(None) else {
            return match self.translate_response_payload(source, target, chunk, state) {
                Ok(chunks) => ResponseTranslationBatch {
                    chunks,
                    error: None,
                },
                Err(error) => ResponseTranslationBatch {
                    chunks: Vec::new(),
                    error: Some(error),
                },
            };
        };
        if source == target || complete_bare_json {
            return match self.translate_response_payload(source, target, chunk, state) {
                Ok(chunks) => ResponseTranslationBatch {
                    chunks,
                    error: None,
                },
                Err(error) => ResponseTranslationBatch {
                    chunks: Vec::new(),
                    error: Some(error),
                },
            };
        }

        if state
            .text_framer
            .as_ref()
            .is_none_or(|framer| framer.mode() != mode)
        {
            state.text_framer = Some(crate::core::stream_framing::TextStreamFramer::new(mode));
        }
        let mut framer = state
            .text_framer
            .take()
            .expect("text framer initialized above");
        let mut chunks = Vec::new();
        let mut transform_error = None;
        let frame_result = framer.feed(chunk, |frame| {
            if transform_error.is_some() {
                return;
            }
            let Some(payload) = frame.payload() else {
                return;
            };
            match self.translate_response_payload(source, target, payload.as_bytes(), state) {
                Ok(output) => chunks.extend(output),
                Err(error) => transform_error = Some(error),
            }
        });
        state.text_framer = Some(framer);
        let error = transform_error.or_else(|| frame_result.err().map(frame_limit_error));
        if let Some(error) = error.as_ref() {
            state.failure = Some(error.clone());
        }
        ResponseTranslationBatch { chunks, error }
    }

    /// Translate one complete source payload. Callers that own a shared source
    /// framer use this entry point to avoid duplicate scan/buffer state.
    pub fn translate_response_payload(
        &self,
        source: Format,
        target: Format,
        chunk: &[u8],
        state: &mut ResponseTransformState,
    ) -> Result<Vec<String>, StreamLimitError> {
        if source == target {
            return Ok(vec![String::from_utf8_lossy(chunk).to_string()]);
        }

        // Direct route (provider→client)
        if let Some(transform) = self.response_transforms.get(&(source, target)) {
            tracing::debug!(
                target: "openproxy::translator",
                "route=direct response {}→{}",
                source.as_str(),
                target.as_str()
            );
            let output = transform(chunk, state);
            return state.failure.clone().map_or(Ok(output), Err);
        }

        tracing::debug!(
            target: "openproxy::translator",
            "route=pivot response {}→openai→{}",
            source.as_str(),
            target.as_str()
        );

        // Step 1: provider (source) -> OpenAI intermediate
        let mut intermediates: Vec<String> = Vec::new();
        if source != Format::OpenAi {
            if let Some(transform) = self.response_transforms.get(&(source, Format::OpenAi)) {
                let converted = transform(chunk, state);
                if let Some(error) = state.failure.clone() {
                    return Err(error);
                }
                if !converted.is_empty() {
                    intermediates = converted;
                }
            }
        } else {
            intermediates.push(String::from_utf8_lossy(chunk).to_string());
        }

        // Step 2: OpenAI intermediate -> client (target)
        // Critical 9router parity: feed INTERMEDIATE strings, not the raw chunk.
        if target != Format::OpenAi {
            if let Some(transform) = self.response_transforms.get(&(Format::OpenAi, target)) {
                let mut final_results = Vec::new();
                for mid in &intermediates {
                    transform_openai_intermediate(mid, state, *transform, &mut final_results)?;
                }
                if !final_results.is_empty() {
                    return Ok(final_results);
                }
            }
        }

        Ok(intermediates)
    }

    /// Flush end-of-stream state for a response transform. Called once when
    /// the upstream stream ends (clean EOF or error). Flushes any pending
    /// text-framer payloads.
    pub fn finish_stream(
        &self,
        source: Format,
        target: Format,
        state: &mut ResponseTransformState,
    ) -> Vec<String> {
        if state.failure.is_some() {
            return Vec::new();
        }
        if source == target {
            return Vec::new();
        }
        let mut output = Vec::new();
        if state
            .text_framer
            .as_ref()
            .is_some_and(|framer| framer.pending_len() > 0)
        {
            let mut framer = state
                .text_framer
                .take()
                .expect("pending text framer exists");
            let mut transform_error = None;
            let frame_result = framer.finish(|frame| {
                if transform_error.is_some() {
                    return;
                }
                let Some(payload) = frame.payload() else {
                    return;
                };
                match self.translate_response_payload(source, target, payload.as_bytes(), state) {
                    Ok(chunks) => output.extend(chunks),
                    Err(error) => transform_error = Some(error),
                }
            });
            state.text_framer = Some(framer);
            let error = transform_error.or_else(|| frame_result.err().map(frame_limit_error));
            if let Some(error) = error {
                state.failure = Some(error.clone());
                output.push(stream_error_event(&error));
                return output;
            }
        }
        if let Some(transform) = self.response_transforms.get(&(source, target)) {
            // No response transform buffers state that needs flushing.
            let _ = transform;
        }
        output
    }
}

fn frame_limit_error(error: crate::core::stream_framing::FrameError) -> StreamLimitError {
    StreamLimitError {
        code: error.code(),
        message: error.to_string(),
    }
}

fn stream_error_event(error: &StreamLimitError) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "error": {
                "message": error.message,
                "type": "upstream_error",
                "code": error.code,
            }
        })
    )
}

fn transform_openai_intermediate(
    intermediate: &str,
    state: &mut ResponseTransformState,
    transform: ResponseTransformFn,
    output: &mut Vec<String>,
) -> Result<(), StreamLimitError> {
    if serde_json::from_str::<Value>(intermediate)
        .is_ok_and(|value| value.is_object() || value.is_array())
    {
        output.extend(transform(intermediate.as_bytes(), state));
        return state.failure.clone().map_or(Ok(()), Err);
    }

    let mut framer = crate::core::stream_framing::SseFramer::new();
    let mut apply = |event: crate::core::stream_framing::SseEvent<'_>| {
        if state.failure.is_some() {
            return;
        }
        if let Some(payload) = event.data() {
            output.extend(transform(payload.as_bytes(), state));
        }
    };
    framer
        .feed(intermediate.as_bytes(), &mut apply)
        .map_err(frame_limit_error)?;
    framer.finish(apply).map_err(frame_limit_error)?;
    state.failure.clone().map_or(Ok(()), Err)
}

/// Apply normalization hooks that are always run regardless of translation.
/// Mirrors the hooks in open-sse/translator/index.js:
///   stripContentTypes, normalizeThinkingConfig, ensureToolCallIds, fixMissingToolResponses
fn apply_normalization_hooks(body: &mut Value) -> bool {
    // normalizeThinkingConfig (9router): drop thinking on non-user turns
    normalize_thinking_config(body);
    // normalizeDeveloperRole: rewrite role "developer" -> "system" so
    // OAI-compat providers (DeepSeek, Groq, Ollama, …) that pre-date the
    // Codex CLI role split don't 400 on the request.
    crate::core::translator::helpers::openai_helper::normalize_developer_role(body);
    // ensureToolCallIds: ensure tool_calls have ids (full impl from tool_call_helper)
    crate::core::translator::helpers::tool_call_helper::ensure_tool_call_ids(body);
    // fixMissingToolResponses: insert empty tool_result if needed (full impl from tool_call_helper)
    crate::core::translator::helpers::tool_call_helper::fix_missing_tool_responses(body);
    true
}

/// Strip specific content types from messages (opt-in via stripList).
/// Mirrors stripContentTypes in open-sse/translator/index.js.
pub fn strip_content_types(body: &mut Value, strip_list: &[&str]) {
    if strip_list.is_empty() {
        return;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    let image_types: std::collections::HashSet<&str> = ["image_url", "image"].into_iter().collect();
    let audio_types: std::collections::HashSet<&str> =
        ["audio_url", "input_audio"].into_iter().collect();

    let strip_image = strip_list.contains(&"image");
    let strip_audio = strip_list.contains(&"audio");

    for msg in messages.iter_mut() {
        let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        content.retain(|part| {
            let t = match part.get("type").and_then(Value::as_str) {
                Some(t) => t,
                None => return true,
            };
            if image_types.contains(t) && strip_image {
                return false;
            }
            if audio_types.contains(t) && strip_audio {
                return false;
            }
            true
        });
        if content.is_empty() {
            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), Value::String(String::new()));
            }
        }
    }
}

/// Filter messages to OpenAI standard format.
/// Mirrors filterToOpenAIFormat in open-sse/translator/formats/openai.js.
pub fn filter_to_openai_format(body: &mut Value, preserve_cache_control: bool) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    // Process each message
    for msg in messages.iter_mut() {
        // Normalize developer role to system (many providers don't support developer)
        if let Some(obj) = msg.as_object_mut() {
            if obj.get("role").and_then(Value::as_str) == Some("developer") {
                obj.insert("role".to_string(), Value::String("system".to_string()));
            }
        }

        // Keep tool messages as-is
        if msg.get("role").and_then(Value::as_str) == Some("tool") {
            continue;
        }

        // Keep assistant messages with tool_calls as-is
        if msg.get("role").and_then(Value::as_str) == Some("assistant")
            && msg.get("tool_calls").is_some()
        {
            continue;
        }

        // Handle string content — keep as-is
        if msg.get("content").and_then(Value::as_str).is_some() {
            continue;
        }

        // Handle array content — strip Claude-specific blocks
        if let Some(arr) = msg.get_mut("content").and_then(Value::as_array_mut) {
            let mut filtered: Vec<Value> = Vec::new();
            for block in arr.drain(..) {
                let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                // Skip thinking blocks
                if block_type == "thinking"
                    || block_type == "redacted_thinking"
                    || block_type == "signature"
                {
                    continue;
                }
                // Only keep valid OpenAI content types
                if VALID_OPENAI_CONTENT_TYPES.contains(&block_type) {
                    let mut cleaned = block;
                    if let Some(obj) = cleaned.as_object_mut() {
                        obj.remove("signature");
                        if !preserve_cache_control {
                            obj.remove("cache_control");
                        }
                    }
                    filtered.push(cleaned);
                } else if block_type == "tool_use" {
                    // 9router formats/openai.js:44-45 — tool_use blocks are skipped.
                    continue;
                } else if block_type == "tool_result" {
                    // 9router formats/openai.js:46-49 — tool_result kept but passed
                    // through stripBlock (signature always stripped, cache_control
                    // unless preserveCacheControl).
                    let mut cleaned = block;
                    if let Some(obj) = cleaned.as_object_mut() {
                        obj.remove("signature");
                        if !preserve_cache_control {
                            obj.remove("cache_control");
                        }
                    }
                    filtered.push(cleaned);
                }
            }

            // If all content was filtered, add empty text
            if filtered.is_empty() {
                filtered.push(serde_json::json!({"type": "text", "text": ""}));
            }

            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), Value::Array(filtered));
            }
        }
    }

    // Filter out messages with only empty text (but NEVER filter tool messages)
    messages.retain(|msg| {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        // Always keep tool messages
        if role == "tool" {
            return true;
        }
        // Always keep assistant messages with tool_calls
        if role == "assistant" && msg.get("tool_calls").is_some() {
            return true;
        }
        // Check content
        match msg.get("content") {
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(arr)) => arr.iter().any(|b| {
                let t = b.get("type").and_then(Value::as_str).unwrap_or("");
                if t == "text" {
                    b.get("text")
                        .and_then(Value::as_str)
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
                } else {
                    true
                }
            }),
            _ => true,
        }
    });

    // Remove empty tools array
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if tools.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("tools");
            }
        }
    }

    // Normalize tools to OpenAI format (from Claude, Gemini, etc.)
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let mut normalized: Vec<Value> = Vec::new();
        for tool in tools.drain(..) {
            // Already OpenAI format
            if tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("function").is_some()
            {
                normalized.push(tool);
                continue;
            }
            // Claude format: {name, description, input_schema}
            if tool.get("name").is_some()
                && (tool.get("input_schema").is_some() || tool.get("description").is_some())
            {
                normalized.push(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                        "description": tool.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}}))
                    }
                }));
                continue;
            }
            // Gemini format: {functionDeclarations: [{name, description, parameters}]}
            if let Some(decls) = tool.get("functionDeclarations").and_then(Value::as_array) {
                for fn_decl in decls {
                    normalized.push(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": fn_decl.get("name").and_then(Value::as_str).unwrap_or(""),
                            "description": fn_decl.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                            "parameters": fn_decl.get("parameters").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}}))
                        }
                    }));
                }
                continue;
            }
            normalized.push(tool);
        }
        *tools = normalized;
    }

    // Normalize tool_choice to OpenAI format
    if let Some(choice) = body.get("tool_choice").cloned() {
        if let Some(choice_obj) = choice.as_object() {
            let choice_type = choice_obj.get("type").and_then(Value::as_str).unwrap_or("");
            match choice_type {
                "auto" => {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("tool_choice".to_string(), Value::String("auto".to_string()));
                    }
                }
                "any" => {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert(
                            "tool_choice".to_string(),
                            Value::String("required".to_string()),
                        );
                    }
                }
                "tool" => {
                    if let Some(name) = choice_obj.get("name").and_then(Value::as_str) {
                        if let Some(obj) = body.as_object_mut() {
                            obj.insert(
                                "tool_choice".to_string(),
                                serde_json::json!({
                                    "type": "function",
                                    "function": {"name": name}
                                }),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Normalize thinking config: remove `thinking` if last message is not user.
/// Keeps `reasoning_effort` (OpenAI request-level — survives tool-result turns).
/// Mirrors open-sse/services/provider.js:normalizeThinkingConfig.
pub fn normalize_thinking_config(body: &mut Value) {
    if is_last_message_from_user(body) {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("thinking");
    }
}

/// True if the last message/content role is user (or no messages → true).
fn is_last_message_from_user(body: &Value) -> bool {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("contents").and_then(Value::as_array));
    let Some(messages) = messages else {
        return true;
    };
    if messages.is_empty() {
        return true;
    }
    let last = &messages[messages.len() - 1];
    last.get("role").and_then(Value::as_str) == Some("user")
}

/// Global registry instance — lazily initialized.
use std::sync::OnceLock;
static REGISTRY: OnceLock<TranslationRegistry> = OnceLock::new();

/// Get the global translation registry.
/// Initializes with all registered transforms on first call.
pub fn global_registry() -> &'static TranslationRegistry {
    use crate::core::translator::request::antigravity_to_openai::antigravity_to_openai_request;
    use crate::core::translator::request::claude_to_openai::claude_to_openai_request;
    use crate::core::translator::request::gemini_to_openai::gemini_to_openai_request;
    use crate::core::translator::request::openai_responses::{
        chat_to_openai_responses_request, openai_responses_to_chat_request,
    };
    use crate::core::translator::request::openai_to_claude::openai_to_claude_request;
    use crate::core::translator::request::openai_to_gemini::openai_to_antigravity_request;
    use crate::core::translator::request::openai_to_gemini::openai_to_gemini_request;
    use crate::core::translator::request::openai_to_ollama::openai_to_ollama_request;
    use crate::core::translator::request::openai_to_vertex::openai_to_vertex_request;
    use crate::core::translator::response::claude_to_openai::claude_to_openai_streaming;
    use crate::core::translator::response::gemini_to_openai::gemini_to_openai_streaming;
    use crate::core::translator::response::ollama_to_openai::ollama_to_openai_streaming;
    use crate::core::translator::response::openai_responses::{
        chat_to_responses_streaming, responses_to_chat_streaming,
    };
    use crate::core::translator::response::openai_to_antigravity::openai_to_antigravity_streaming;
    use crate::core::translator::response::openai_to_claude::openai_to_claude_streaming;
    use crate::core::translator::response::openai_to_gemini::openai_to_gemini_response;

    REGISTRY.get_or_init(|| {
        let mut reg = TranslationRegistry::new();

        // Request transforms
        reg.register_request(
            Format::OpenAi,
            Format::Claude,
            openai_to_claude_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Claude,
            Format::OpenAi,
            claude_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Gemini,
            Format::OpenAi,
            gemini_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Ollama,
            openai_to_ollama_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Gemini,
            openai_to_gemini_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Vertex,
            openai_to_vertex_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Antigravity,
            openai_to_antigravity_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Antigravity,
            Format::OpenAi,
            antigravity_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::OpenAiResponses,
            chat_to_openai_responses_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAiResponses,
            Format::OpenAi,
            openai_responses_to_chat_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Codex,
            chat_to_openai_responses_request as RequestTransformFn,
        );
        reg.register_response(
            Format::OpenAi,
            Format::Gemini,
            openai_to_gemini_response as ResponseTransformFn,
        );
        reg.register_response(
            Format::Claude,
            Format::OpenAi,
            claude_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Gemini,
            Format::OpenAi,
            gemini_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Ollama,
            Format::OpenAi,
            ollama_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAiResponses,
            Format::OpenAi,
            responses_to_chat_streaming as ResponseTransformFn,
        );
        // OpenAI → client response pairs (required for double-hop when client ≠ OpenAI)
        reg.register_response(
            Format::OpenAi,
            Format::Claude,
            openai_to_claude_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAi,
            Format::OpenAiResponses,
            chat_to_responses_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAi,
            Format::Codex,
            chat_to_responses_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAi,
            Format::Antigravity,
            openai_to_antigravity_streaming as ResponseTransformFn,
        );
        // Gemini-family aliases (JS multi-register gemini/vertex/antigravity → openai)
        reg.register_response(
            Format::Vertex,
            Format::OpenAi,
            gemini_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Antigravity,
            Format::OpenAi,
            gemini_to_openai_streaming as ResponseTransformFn,
        );

        reg
    })
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_responses_requires_no_messages() {
        let body = json!({"input": "hi", "messages": [{"role": "user", "content": "x"}]});
        // With messages present, must NOT force responses (9router guard)
        assert_ne!(detect_source_format(&body), Format::OpenAiResponses);
    }

    #[test]
    fn detect_responses_input_string() {
        let body = json!({"input": "hello", "stream": true});
        assert_eq!(detect_source_format(&body), Format::OpenAiResponses);
    }

    #[test]
    fn detect_openai_fields_before_claude_system() {
        let body = json!({
            "model": "gpt-4",
            "stream_options": {"include_usage": true},
            "system": "you are helpful",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(detect_source_format(&body), Format::OpenAi);
    }

    #[test]
    fn chat_completions_input_array_forces_openai() {
        let body = json!({"input": [{"type": "message", "role": "user", "content": []}]});
        assert_eq!(
            detect_source_format_by_endpoint_with_body("/v1/chat/completions", Some(&body)),
            Some(Format::OpenAi)
        );
    }

    #[test]
    fn anthropic_compatible_targets_claude() {
        assert_eq!(
            get_target_format_for_provider("anthropic-compatible-foo"),
            Format::Claude
        );
        assert_eq!(
            get_target_format_for_provider("openai-compatible-responses"),
            Format::OpenAiResponses
        );
    }

    #[test]
    fn registry_has_direct_antigravity() {
        let reg = global_registry();
        assert!(reg.has_request_transform(Format::OpenAi, Format::Antigravity));
        assert!(reg.has_response_transform(Format::OpenAi, Format::Claude));
    }

    #[test]
    fn normalize_thinking_strips_on_tool_turn() {
        let mut body = json!({
            "thinking": {"type": "enabled", "budget_tokens": 1000},
            "reasoning_effort": "high",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "ok"},
                {"role": "tool", "content": "result", "tool_call_id": "1"}
            ]
        });
        normalize_thinking_config(&mut body);
        assert!(body.get("thinking").is_none());
        // reasoning_effort survives
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn preserves_cache_control_when_true() {
        let mut body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "x",
                         "cache_control": {"type": "ephemeral"},
                         "signature": "foo"}
                    ]
                }
            ]
        });
        filter_to_openai_format(&mut body, true);
        let part = &body["messages"][0]["content"][0];
        // cache_control preserved (alicode quirk), signature always stripped.
        assert!(part.get("cache_control").is_some());
        assert!(part.get("signature").is_none());
    }

    #[test]
    fn strips_tool_result_cache_control_when_false() {
        let mut body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "t1",
                         "content": "ok", "cache_control": {"type": "ephemeral"},
                         "signature": "sig"}
                    ]
                }
            ]
        });
        filter_to_openai_format(&mut body, false);
        let part = &body["messages"][0]["content"][0];
        assert!(part.get("cache_control").is_none());
        assert!(part.get("signature").is_none());
        assert_eq!(part["tool_use_id"], "t1");
    }

    #[test]
    fn drops_tool_use_blocks() {
        let mut body = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "tu1", "name": "f", "input": {}},
                        {"type": "text", "text": "done"}
                    ]
                }
            ]
        });
        filter_to_openai_format(&mut body, true);
        // Message survives (has a text block); tool_use is dropped.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert!(
            !content
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")),
            "tool_use blocks must be dropped"
        );
        assert!(content
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("text")));
    }

    #[test]
    fn custom_tool_names_survive_passthrough_but_strip_on_translate() {
        let reg = global_registry();
        // Passthrough (source==target): caller input keeps the metadata so
        // non-translated paths can thread it to the response conversion.
        let mut passthrough = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "_customToolNames": ["web_search", "code_exec"]
        });
        reg.translate_request_with_strip(
            Format::OpenAi,
            Format::OpenAi,
            "gpt-4",
            &mut passthrough,
            false,
            None,
            None,
        );
        assert!(
            passthrough.get("_customToolNames").is_some(),
            "passthrough must not strip _customToolNames from caller input"
        );
        // Translated path (Claude→OpenAi has a direct request transform):
        // _customToolNames is stripped from the translated output (9router
        // chatCore.js:198-199 deletes it from translatedBody, not the input).
        let mut translated = json!({
            "model": "claude-sonnet-4-5",
            "system": "helpful",
            "messages": [{"role": "user", "content": "hi"}],
            "_customToolNames": ["web_search"]
        });
        reg.translate_request_with_strip(
            Format::Claude,
            Format::OpenAi,
            "claude-sonnet-4-5",
            &mut translated,
            false,
            None,
            None,
        );
        assert!(
            translated.get("_customToolNames").is_none(),
            "translated output must not leak _customToolNames upstream"
        );
    }
}
