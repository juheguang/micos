use super::chat::output_index;
use super::sse::SseEvent;
use super::ModelResponse;
use crate::ui::{AgentEvent, UiSink};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct ResponsesFunctionAcc {
    call_id: String,
    name: String,
    arguments: String,
}

pub(super) fn emit_responses_sse_delta<S: UiSink>(event: &SseEvent, sink: &mut S) -> Result<()> {
    if event.data == "[DONE]" {
        return Ok(());
    }
    let value: Value = serde_json::from_str(&event.data).context("parse Responses SSE event")?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    if event_type == "response.output_text.delta" {
        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
            sink.on_event(AgentEvent::AssistantDelta {
                text: delta.to_string(),
            })?;
        }
    }
    Ok(())
}

pub(super) fn parse_responses_sse_events<S: UiSink>(
    events: &[SseEvent],
    sink: &mut S,
) -> Result<ModelResponse> {
    let mut output_by_index = BTreeMap::new();
    let mut function_calls: BTreeMap<usize, ResponsesFunctionAcc> = BTreeMap::new();
    let mut text = String::new();

    for event in events {
        if event.data == "[DONE]" {
            continue;
        }
        let value: Value =
            serde_json::from_str(&event.data).context("parse Responses SSE event")?;
        let event_type = event
            .event
            .as_deref()
            .or_else(|| value.get("type").and_then(Value::as_str))
            .unwrap_or_default();

        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                    sink.on_event(AgentEvent::AssistantDelta {
                        text: delta.to_string(),
                    })?;
                }
            }
            "response.function_call_arguments.delta" => {
                let index = output_index(&value).unwrap_or(0);
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    function_calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let index = output_index(&value).unwrap_or(0);
                let acc = function_calls.entry(index).or_default();
                if let Some(arguments) = value.get("arguments").and_then(Value::as_str) {
                    acc.arguments = arguments.to_string();
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if let Some(item) = value.get("item") {
                    let index = output_index(&value).unwrap_or(output_by_index.len());
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let acc = function_calls.entry(index).or_default();
                        if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                            acc.call_id = call_id.to_string();
                        }
                        if let Some(name) = item.get("name").and_then(Value::as_str) {
                            acc.name = name.to_string();
                        }
                        if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                            acc.arguments = arguments.to_string();
                        }
                    }
                    if event_type == "response.output_item.done" {
                        output_by_index.insert(index, item.clone());
                    }
                }
            }
            "response.completed" | "response.done" => {
                if let Some(output) = value
                    .get("response")
                    .and_then(|response| response.get("output"))
                    .and_then(Value::as_array)
                {
                    return Ok(ModelResponse::from_output(output.clone()));
                }
            }
            _ => {}
        }
    }

    let existing_function_indexes = output_by_index
        .iter()
        .filter_map(|(index, item)| {
            (item.get("type").and_then(Value::as_str) == Some("function_call")).then_some(*index)
        })
        .collect::<BTreeSet<_>>();
    let mut output = output_by_index.into_values().collect::<Vec<_>>();
    if output.is_empty() && !text.is_empty() {
        output.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}]
        }));
    }

    for (index, call) in function_calls {
        if existing_function_indexes.contains(&index) {
            continue;
        }
        if call.name.is_empty() && call.call_id.is_empty() && call.arguments.is_empty() {
            continue;
        }
        let arguments = if call.arguments.is_empty() {
            "{}".to_string()
        } else {
            call.arguments
        };
        output.push(json!({
            "type": "function_call",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": arguments,
        }));
    }

    Ok(ModelResponse::from_output(output))
}
