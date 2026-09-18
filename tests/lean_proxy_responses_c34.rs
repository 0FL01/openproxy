//! C34: Responses accumulator appends in amortized linear time without
//! whole-map clones or old-text-plus-delta quadratic copies.
//!
//! Lean boundary: proxy must keep valid translation goldens byte-equivalent
//! while bounding retained tool/text/reasoning state (C31). Temporary copy
//! growth must not be quadratic in the number of small deltas.

use openproxy::core::translator::registry::{track_openai_accumulation, ResponseTransformState};
use openproxy::core::translator::response::openai_responses::chat_to_responses_response;
use serde_json::{json, Value};

fn feed_chat(state: &mut ResponseTransformState, chunk: &Value) -> Vec<Value> {
    track_openai_accumulation(state, chunk, 4, true).expect("within C31 bounds");
    chat_to_responses_response(chunk, &mut state.responses.state)
}

fn text_chunk(content: &str) -> Value {
    json!({
        "id": "chatcmpl-c34",
        "model": "m",
        "choices": [{
            "index": 0,
            "delta": {"content": content},
            "finish_reason": null
        }]
    })
}

fn finish_chunk() -> Value {
    json!({
        "id": "chatcmpl-c34",
        "model": "m",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    })
}

#[test]
fn growing_text_deltas_concatenate_and_free_after_done() {
    let mut state = ResponseTransformState::default();
    let deltas = 2_000usize;
    let frag = "abcdefghij";
    let mut events = Vec::new();
    for _ in 0..deltas {
        events.extend(feed_chat(&mut state, &text_chunk(frag)));
    }
    events.extend(feed_chat(&mut state, &finish_chunk()));

    let expected = frag.repeat(deltas);
    let done = events
        .iter()
        .find(|e| e["event"] == "response.output_item.done")
        .expect("message done");
    assert_eq!(done["data"]["item"]["content"][0]["text"], expected.clone());

    // Full text was required exactly once for the done payload; the retained
    // buffer must be gone afterwards.
    let retained = state
        .responses
        .state
        .get("msgTextBuf")
        .and_then(Value::as_object)
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(retained, 0, "completed text buffer must be freed");
    assert!(
        state.responses.state.get("msgId_0").is_none(),
        "completed msg id must be freed"
    );

    // Delta stream itself must carry every fragment in order.
    let streamed: String = events
        .iter()
        .filter(|e| e["event"] == "response.output_text.delta")
        .filter_map(|e| e["data"]["delta"].as_str())
        .collect();
    assert_eq!(streamed, expected);
}

#[test]
fn tool_argument_fragments_concatenate_and_free_after_done() {
    let mut state = ResponseTransformState::default();
    let start = json!({
        "id": "chatcmpl-c34",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0, "id": "call_c34", "type": "function",
                "function": {"name": "lookup", "arguments": ""}
            }]},
            "finish_reason": null
        }]
    });
    let mut events = feed_chat(&mut state, &start);
    let frags = 1_000usize;
    for i in 0..frags {
        let frag = format!("{{\"k{i}\":\"v{i}\"}},");
        let chunk = json!({
            "id": "chatcmpl-c34",
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0, "function": {"arguments": frag}
                }]},
                "finish_reason": null
            }]
        });
        events.extend(feed_chat(&mut state, &chunk));
    }
    let finish = json!({
        "id": "chatcmpl-c34",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    });
    events.extend(feed_chat(&mut state, &finish));

    let expected: String = (0..frags)
        .map(|i| format!("{{\"k{i}\":\"v{i}\"}},"))
        .collect();
    let done = events
        .iter()
        .find(|e| e["event"] == "response.function_call_arguments.done")
        .expect("args done");
    assert_eq!(done["data"]["arguments"], expected);

    let retained = state
        .responses
        .state
        .get("funcArgsBuf")
        .and_then(Value::as_object)
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(retained, 0, "completed args buffer must be freed");
}

#[test]
fn reasoning_deltas_concatenate_and_free_after_done() {
    let mut state = ResponseTransformState::default();
    let mut events = Vec::new();
    for _ in 0..500 {
        let chunk = json!({
            "id": "chatcmpl-c34",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "r0123456789"},
                "finish_reason": null
            }]
        });
        events.extend(feed_chat(&mut state, &chunk));
    }
    events.extend(feed_chat(&mut state, &finish_chunk()));
    let expected = "r0123456789".repeat(500);
    let done = events
        .iter()
        .find(|e| e["event"] == "response.reasoning_summary_text.done")
        .expect("reasoning done");
    assert_eq!(done["data"]["text"], expected);
    assert_eq!(
        state.responses.state["reasoningBuf"],
        Value::String(String::new()),
        "completed reasoning buffer must be dropped"
    );
}

#[test]
fn parallel_items_keep_ordering_without_cross_copy() {
    let mut state = ResponseTransformState::default();
    // Two messages on different choice indices plus two tools.
    for idx in [0u64, 1u64] {
        events_for_idx(&mut state, idx, &format!("text-{idx}-"));
    }
    let tool_start = |idx: u64, id: &str, name: &str| {
        json!({
            "id": "chatcmpl-c34",
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": idx, "id": id, "type": "function",
                    "function": {"name": name, "arguments": ""}
                }]},
                "finish_reason": null
            }]
        })
    };
    let mut events = feed_chat(&mut state, &tool_start(0, "call_a", "fa"));
    events.extend(feed_chat(&mut state, &tool_start(1, "call_b", "fb")));
    for (idx, frag) in [(0u64, "{\"a\":1}"), (1u64, "{\"b\":2}")] {
        events.extend(feed_chat(
            &mut state,
            &json!({
                "id": "chatcmpl-c34",
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": idx, "function": {"arguments": frag}
                    }]},
                    "finish_reason": null
                }]
            }),
        ));
    }
    events.extend(feed_chat(
        &mut state,
        &json!({
            "id": "chatcmpl-c34",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        }),
    ));
    let done_args: Vec<String> = events
        .iter()
        .filter(|e| e["event"] == "response.function_call_arguments.done")
        .filter_map(|e| e["data"]["arguments"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        done_args,
        vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
    );
}

fn events_for_idx(state: &mut ResponseTransformState, idx: u64, frag: &str) {
    let chunk = json!({
        "id": "chatcmpl-c34",
        "choices": [{
            "index": idx,
            "delta": {"content": frag},
            "finish_reason": null
        }]
    });
    let _ = feed_chat(state, &chunk);
}

#[test]
fn source_guards_keep_point_mutation_without_whole_map_clones() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src =
        std::fs::read_to_string(root.join("src/core/translator/response/openai_responses.rs"))
            .expect("read responses translator");
    for banned in [
        "get(\"funcCallIds\")\n        .cloned()",
        "get(\"funcArgsBuf\")\n        .cloned()",
        "get(\"msgTextBuf\")\n        .cloned()",
        "get(\"msgItemAdded\")\n        .cloned()",
        "format!(\"{}{}\", existing",
    ] {
        assert!(
            !src.contains(banned),
            "C34 must not clone whole maps or rebuild strings via {banned}"
        );
    }
    for required in [
        "fn append_indexed_text",
        "fn state_obj_mut",
        "fn take_indexed_text",
        "push_str",
    ] {
        assert!(
            src.contains(required),
            "C34 point-mutation helper missing: {required}"
        );
    }
}
