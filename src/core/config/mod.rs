//! Configuration / static lookup tables ported from 9router's
//! `open-sse/config/`. Each submodule mirrors one upstream JS file.
//!
//! Most entries are static lookup tables exposed as `once_cell::sync::Lazy`
//! values so they are computed once on first use.

pub mod app_constants;
pub mod default_thinking_signature;
pub mod error_config;
pub mod ollama_models;
pub mod runtime_config;
