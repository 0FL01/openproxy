//! Utilities ported from `open-sse/utils/` in 9router. Pure-logic helpers
//! (no streaming machinery) live here; streaming/transport-level helpers
//! that depend on Node-specific abstractions are reimplemented inline by
//! the relevant executor instead.

pub mod antigravity_project;
pub mod client_detector;
pub mod error;
pub mod reasoning_content_injector;
pub mod session_manager;
pub mod stream_flags;
pub mod thinking_suffix;
