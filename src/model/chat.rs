use super::sse::SseEvent;
use super::ModelResponse;
use crate::ui::{AgentEvent, UiSink};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
struct ChatToolCallAcc {
    id: String,
    r#type: String,
    name: String,
    arguments: String,
}

pub(super) fn emit_chat_completions_sse_delta<S: UiSink>(
    event: &SseEvent,
    sink: &mut S,
) -> Result<()> {
    if event.data == "[DONE]" {
        return Ok(());
    }
    let value: Value =
        serde_json::from_str(&event.data).context("parse Chat Completions SSE event")?;
    let Some(delta) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
    else {
        return Ok(());
    };

    if let Some(piece) = delta.get("content").and_then(Value::as_str) {
        sink.on_event(AgentEvent::AssistantDelta {
            text: piece.to_string(),
        })?;
    }
    if let Some(piece) = delta.get("reasoning_content").and_then(Value::as_str) {
        sink.on_event(AgentEvent::ReasoningDelta {
            text: piece.to_string(),
        })?;
    }
    Ok(())
}

pub(super) struct NoopUi;

impl UiSink for NoopUi {
    fn on_event(&mut self, _event: AgentEvent) -> Result<()> {
        Ok(())
    }
}

pub(super) fn parse_chat_completions_sse_events<S: UiSink>(
    events: &[SseEvent],
    sink: &mut S,
) -> Result<ModelResponse> {
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: BTreeMap<usize, ChatToolCallAcc> = BTreeMap::new();

    for event in events {
        if event.data == "[DONE]" {
            continue;
        }
        let value: Value =
            serde_json::from_str(&event.data).context("parse Chat Completions SSE event")?;
        let Some(delta) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
        else {
            continue;
        };

        if let Some(piece) = delta.get("content").and_then(Value::as_str) {
            content.push_str(piece);
            sink.on_event(AgentEvent::AssistantDelta {
                text: piece.to_string(),
            })?;
        }
        if let Some(piece) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning_content.push_str(piece);
            sink.on_event(AgentEvent::ReasoningDelta {
                text: piece.to_string(),
            })?;
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(0);
                let acc = tool_calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    acc.id = id.to_string();
                }
                if let Some(kind) = call.get("type").and_then(Value::as_str) {
                    acc.r#type = kind.to_string();
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        acc.name.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        acc.arguments.push_str(arguments);
                    }
                }
            }
        }
    }

    let message = ChatMessage {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content)
        },
        tool_calls: tool_calls
            .into_values()
            .filter(|call| !call.id.is_empty() || !call.name.is_empty())
            .map(|call| ChatToolCall {
                id: call.id,
                r#type: if call.r#type.is_empty() {
                    "function".into()
                } else {
                    call.r#type
                },
                function: ChatFunctionCall {
                    name: call.name,
                    arguments: if call.arguments.is_empty() {
                        "{}".into()
                    } else {
                        call.arguments
                    },
                },
            })
            .collect(),
    };

    Ok(ChatCompletionsResponse {
        choices: vec![ChatChoice { message }],
    }
    .into_model_response())
}

pub(super) fn output_index(value: &Value) -> Option<usize> {
    value
        .get("output_index")
        .or_else(|| value.get("item_index"))
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ResponsesApiResponse {
    #[serde(default)]
    pub(super) output: Vec<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatCompletionsResponse {
    #[serde(default)]
    pub(super) choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ChatChoice {
    pub(super) message: ChatMessage,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ChatMessage {
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) reasoning_content: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ChatToolCall {
    pub(super) id: String,
    #[serde(default)]
    pub(super) r#type: String,
    pub(super) function: ChatFunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ChatFunctionCall {
    pub(super) name: String,
    pub(super) arguments: String,
}

impl ChatCompletionsResponse {
    pub(super) fn into_model_response(self) -> ModelResponse {
        let Some(choice) = self.choices.into_iter().next() else {
            return ModelResponse::default();
        };

        let mut output = Vec::new();
        let reasoning_content = choice
            .message
            .reasoning_content
            .filter(|text| !text.is_empty());
        let content = choice.message.content.filter(|text| !text.is_empty());
        if content.is_some() || reasoning_content.is_some() || !choice.message.tool_calls.is_empty()
        {
            let content_items = content
                .as_ref()
                .map(|text| vec![json!({"type": "output_text", "text": text})])
                .unwrap_or_default();
            let tool_calls: Vec<Value> = choice
                .message
                .tool_calls
                .iter()
                .map(chat_tool_call_to_message_value)
                .collect();
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": content_items,
                "reasoning_content": reasoning_content,
                "tool_calls": tool_calls
            }));
        }

        for tool_call in choice.message.tool_calls {
            output.push(json!({
                "type": "function_call",
                "call_id": tool_call.id,
                "name": tool_call.function.name,
                "arguments": tool_call.function.arguments,
                "chat_message_present": true
            }));
        }

        ModelResponse::from_output(output)
    }
}

fn chat_tool_call_to_message_value(tool_call: &ChatToolCall) -> Value {
    json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.function.name,
            "arguments": tool_call.function.arguments
        }
    })
}
