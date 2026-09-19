//! Resource bounds shared by streaming response accumulators.

use std::fmt;

use serde_json::Value;

pub const MAX_STREAM_CHOICES: usize = 128;
pub const MAX_STREAM_TOOL_CALLS: usize = 128;
pub const MAX_RESPONSES_OUTPUT_ITEMS: usize = 512;
pub const MAX_STREAM_WIRE_INDEX: u64 = 4095;
pub const MAX_STREAM_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_ACCUMULATED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLimitError {
    pub code: &'static str,
    pub message: String,
}

impl StreamLimitError {
    pub fn invalid_index(field: &str) -> Self {
        Self {
            code: "upstream_stream_invalid_index",
            message: format!("Upstream stream field {field} must be a non-negative integer"),
        }
    }

    pub fn index_too_large(field: &str, index: u64) -> Self {
        Self {
            code: "upstream_stream_index_limit",
            message: format!(
                "Upstream stream field {field} index {index} exceeds limit {MAX_STREAM_WIRE_INDEX}"
            ),
        }
    }

    pub fn too_many(kind: &str, limit: usize) -> Self {
        Self {
            code: "upstream_stream_state_limit",
            message: format!("Upstream stream exceeds the {limit} {kind} limit"),
        }
    }

    pub fn bytes(kind: &str, limit: usize) -> Self {
        Self {
            code: "upstream_stream_state_limit",
            message: format!("Upstream stream {kind} exceeds the {limit}-byte limit"),
        }
    }

    pub fn capacity(kind: &str) -> Self {
        Self {
            code: "upstream_stream_capacity_error",
            message: format!("Unable to reserve memory for upstream stream {kind}"),
        }
    }

    pub fn arithmetic(kind: &str) -> Self {
        Self {
            code: "upstream_stream_arithmetic_overflow",
            message: format!("Upstream stream {kind} counter overflowed"),
        }
    }
}

impl fmt::Display for StreamLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamLimitError {}

pub fn wire_index(value: Option<&Value>, field: &str) -> Result<u64, StreamLimitError> {
    let index = value
        .and_then(Value::as_u64)
        .ok_or_else(|| StreamLimitError::invalid_index(field))?;
    if index > MAX_STREAM_WIRE_INDEX {
        return Err(StreamLimitError::index_too_large(field, index));
    }
    Ok(index)
}

pub fn checked_append(
    target: &mut String,
    fragment: &str,
    field_limit: usize,
    retained_bytes: &mut usize,
    kind: &str,
) -> Result<(), StreamLimitError> {
    let field_bytes = target
        .len()
        .checked_add(fragment.len())
        .ok_or_else(|| StreamLimitError::arithmetic(kind))?;
    if field_bytes > field_limit {
        return Err(StreamLimitError::bytes(kind, field_limit));
    }
    let next_retained = retained_bytes
        .checked_add(fragment.len())
        .ok_or_else(|| StreamLimitError::arithmetic("retained state"))?;
    if next_retained > MAX_STREAM_ACCUMULATED_BYTES {
        return Err(StreamLimitError::bytes(
            "retained state",
            MAX_STREAM_ACCUMULATED_BYTES,
        ));
    }
    target
        .try_reserve(fragment.len())
        .map_err(|_| StreamLimitError::capacity(kind))?;
    target.push_str(fragment);
    *retained_bytes = next_retained;
    Ok(())
}

pub fn checked_retain(
    retained_bytes: &mut usize,
    bytes: usize,
    kind: &str,
) -> Result<(), StreamLimitError> {
    let next = retained_bytes
        .checked_add(bytes)
        .ok_or_else(|| StreamLimitError::arithmetic(kind))?;
    if next > MAX_STREAM_ACCUMULATED_BYTES {
        return Err(StreamLimitError::bytes(
            "retained state",
            MAX_STREAM_ACCUMULATED_BYTES,
        ));
    }
    *retained_bytes = next;
    Ok(())
}

pub fn checked_add_u64(left: u64, right: u64, kind: &str) -> Result<u64, StreamLimitError> {
    left.checked_add(right)
        .ok_or_else(|| StreamLimitError::arithmetic(kind))
}
