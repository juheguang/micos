use crate::config::ApiKind;
use crate::ui::{AgentEvent, UiSink};
use anyhow::Result;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelErrorClass {
    ContextPressure,
    RateLimit,
    ServerError,
    Fatal,
}

impl ModelErrorClass {
    pub fn classify(error: &anyhow::Error) -> Self {
        let msg = error.to_string();
        let lower = msg.to_ascii_lowercase();
        if lower.contains("prompt_too_long")
            || lower.contains("context_length_exceeded")
            || lower.contains("error 413")
            || lower.contains("413")
        {
            Self::ContextPressure
        } else if lower.contains("rate_limit")
            || lower.contains("error 429")
            || lower.contains("429")
        {
            Self::RateLimit
        } else if lower.contains("error 50") || lower.contains("error 5") {
            Self::ServerError
        } else {
            Self::Fatal
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait ModelClient {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse>;

    async fn respond_streaming<S: UiSink + Send>(
        &self,
        request: ModelRequest,
        sink: &mut S,
    ) -> Result<ModelResponse> {
        let response = self.respond(request).await?;
        for text in &response.assistant_text {
            sink.on_event(AgentEvent::AssistantDelta { text: text.clone() })?;
        }
        Ok(response)
    }
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    pub instructions: String,
    pub parallel_tool_calls: bool,
    pub thinking: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelResponse {
    pub output: Vec<Value>,
    pub assistant_text: Vec<String>,
    pub function_calls: Vec<ModelFunctionCall>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

pub struct OpenAiModelClient {
    pub(super) api_key: String,
    pub(super) api_kind: ApiKind,
    pub(super) base_url: String,
    pub(super) http: reqwest::Client,
}

impl OpenAiModelClient {
    pub fn new(api_key: String, api_kind: ApiKind, base_url: String) -> Self {
        Self {
            api_key,
            api_kind,
            base_url,
            http: reqwest::Client::new(),
        }
    }

    pub fn update_endpoint(&mut self, api_kind: ApiKind, base_url: String) {
        self.api_kind = api_kind;
        self.base_url = base_url;
    }
}

impl ModelResponse {
    pub fn from_output(output: Vec<Value>) -> Self {
        let mut assistant_text = Vec::new();
        let mut function_calls = Vec::new();

        for item in &output {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_string();
                    function_calls.push(ModelFunctionCall {
                        call_id,
                        name,
                        arguments,
                    });
                }
                Some("message") => {
                    if item.get("role").and_then(Value::as_str) == Some("assistant") {
                        collect_message_text(item, &mut assistant_text);
                    }
                }
                _ => {}
            }
        }

        Self {
            output,
            assistant_text,
            function_calls,
        }
    }
}

fn collect_message_text(item: &Value, out: &mut Vec<String>) {
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return;
    };

    for part in content {
        let part_type = part.get("type").and_then(Value::as_str);
        if matches!(part_type, Some("output_text") | Some("text")) {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                out.push(text.to_string());
            }
        }
    }
}

pub fn responses_input_to_chat_messages(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("message") => message_item_to_chat_message(item),
            Some("function_call") => {
                if item
                    .get("chat_message_present")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    None
                } else {
                    function_call_item_to_chat_message(item)
                }
            }
            Some("function_call_output") => Some(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                "content": item.get("output").and_then(Value::as_str).unwrap_or_default()
            })),
            _ => None,
        })
        .collect()
}

fn message_item_to_chat_message(item: &Value) -> Option<Value> {
    let role = item.get("role").and_then(Value::as_str)?;
    let text = item
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let mut message = json!({
        "role": role,
        "content": text
    });
    if let Some(reasoning_content) = item.get("reasoning_content").and_then(Value::as_str) {
        message["reasoning_content"] = json!(reasoning_content);
    }
    if let Some(tool_calls) = item.get("tool_calls").and_then(Value::as_array) {
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls.clone());
        }
    }
    Some(message)
}

fn function_call_item_to_chat_message(item: &Value) -> Option<Value> {
    Some(json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
            "type": "function",
            "function": {
                "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}")
            }
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_error_class_context_pressure() {
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!(
                "Responses API error 413: request too large"
            )),
            ModelErrorClass::ContextPressure
        );
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("prompt_too_long")),
            ModelErrorClass::ContextPressure
        );
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("context_length_exceeded")),
            ModelErrorClass::ContextPressure
        );
    }

    #[test]
    fn model_error_class_rate_limit() {
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!(
                "Chat Completions API error 429: rate limit"
            )),
            ModelErrorClass::RateLimit
        );
    }

    #[test]
    fn model_error_class_server_error() {
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("Responses API error 502: Bad Gateway")),
            ModelErrorClass::ServerError
        );
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("Responses API error 503: overloaded")),
            ModelErrorClass::ServerError
        );
    }

    #[test]
    fn model_error_class_fatal() {
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("401 Unauthorized")),
            ModelErrorClass::Fatal
        );
        assert_eq!(
            ModelErrorClass::classify(&anyhow::anyhow!("unknown network error")),
            ModelErrorClass::Fatal
        );
    }
}

pub fn responses_tools_to_chat_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").cloned().unwrap_or(Value::Null),
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
                }
            })
        })
        .collect()
}
