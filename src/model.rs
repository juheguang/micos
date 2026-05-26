use crate::config::ApiKind;
use crate::ui::{AgentEvent, UiSink};
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
    api_key: String,
    api_kind: ApiKind,
    base_url: String,
    http: reqwest::Client,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SseEvent {
    event: Option<String>,
    data: String,
}

async fn read_sse_events(response: reqwest::Response) -> Result<Vec<SseEvent>> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut events = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read SSE chunk")?;
        buffer.push_str(std::str::from_utf8(&chunk).context("SSE chunk is not UTF-8")?);
        while let Some((index, boundary_len)) = find_sse_boundary(&buffer) {
            let frame = buffer[..index].to_string();
            buffer = buffer[index + boundary_len..].to_string();
            if let Some(event) = parse_sse_frame(&frame) {
                events.push(event);
            }
        }
    }

    if let Some(event) = parse_sse_frame(&buffer) {
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
fn parse_sse_text(text: &str) -> Vec<SseEvent> {
    let mut rest = text.to_string();
    let mut events = Vec::new();
    while let Some((index, boundary_len)) = find_sse_boundary(&rest) {
        let frame = rest[..index].to_string();
        rest = rest[index + boundary_len..].to_string();
        if let Some(event) = parse_sse_frame(&frame) {
            events.push(event);
        }
    }
    if let Some(event) = parse_sse_frame(&rest) {
        events.push(event);
    }
    events
}

fn find_sse_boundary(text: &str) -> Option<(usize, usize)> {
    let lf = text.find("\n\n").map(|index| (index, 2));
    let crlf = text.find("\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_sse_frame(frame: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_string());
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

#[derive(Default)]
struct ResponsesFunctionAcc {
    call_id: String,
    name: String,
    arguments: String,
}

fn parse_responses_sse_events<S: UiSink>(
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
        .collect::<std::collections::BTreeSet<_>>();
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

#[derive(Default)]
struct ChatToolCallAcc {
    id: String,
    r#type: String,
    name: String,
    arguments: String,
}

fn parse_chat_completions_sse_events<S: UiSink>(
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

fn output_index(value: &Value) -> Option<usize> {
    value
        .get("output_index")
        .or_else(|| value.get("item_index"))
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

impl ModelClient for OpenAiModelClient {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse> {
        if self.api_kind.is_chat_completions() {
            return self.respond_chat_completions(request).await;
        }
        self.respond_responses(request).await
    }

    async fn respond_streaming<S: UiSink + Send>(
        &self,
        request: ModelRequest,
        sink: &mut S,
    ) -> Result<ModelResponse> {
        if self.api_kind.is_chat_completions() {
            return self.respond_chat_completions_streaming(request, sink).await;
        }
        self.respond_responses_streaming(request, sink).await
    }
}

impl OpenAiModelClient {
    async fn respond_responses(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = json!({
            "model": request.model,
            "input": request.input,
            "tools": request.tools,
            "instructions": request.instructions,
            "parallel_tool_calls": request.parallel_tool_calls,
        });

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("send Responses API request")?;

        let status = response.status();
        let text = response.text().await.context("read Responses API body")?;
        if !status.is_success() {
            return Err(anyhow!("Responses API error {status}: {text}"));
        }

        let api: ResponsesApiResponse =
            serde_json::from_str(&text).context("parse Responses API response")?;
        Ok(ModelResponse::from_output(api.output))
    }

    async fn respond_responses_streaming<S: UiSink + Send>(
        &self,
        request: ModelRequest,
        sink: &mut S,
    ) -> Result<ModelResponse> {
        let body = json!({
            "model": request.model,
            "input": request.input,
            "tools": request.tools,
            "instructions": request.instructions,
            "parallel_tool_calls": request.parallel_tool_calls,
            "stream": true,
        });

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("send streaming Responses API request")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.context("read Responses API body")?;
            return Err(anyhow!("Responses API error {status}: {text}"));
        }

        let events = read_sse_events(response).await?;
        parse_responses_sse_events(&events, sink)
    }

    async fn respond_chat_completions(&self, request: ModelRequest) -> Result<ModelResponse> {
        let mut messages = vec![json!({
            "role": "system",
            "content": request.instructions,
        })];
        messages.extend(responses_input_to_chat_messages(&request.input));

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "tools": responses_tools_to_chat_tools(&request.tools),
            "tool_choice": "auto",
        });
        if let Some(thinking) = request.thinking {
            body["thinking"] = json!({ "type": thinking });
        }
        if let Some(reasoning_effort) = request.reasoning_effort {
            body["reasoning_effort"] = json!(reasoning_effort);
        }

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("send Chat Completions request")?;

        let status = response.status();
        let text = response
            .text()
            .await
            .context("read Chat Completions body")?;
        if !status.is_success() {
            return Err(anyhow!("Chat Completions API error {status}: {text}"));
        }

        let api: ChatCompletionsResponse =
            serde_json::from_str(&text).context("parse Chat Completions response")?;
        Ok(api.into_model_response())
    }

    async fn respond_chat_completions_streaming<S: UiSink + Send>(
        &self,
        request: ModelRequest,
        sink: &mut S,
    ) -> Result<ModelResponse> {
        let mut messages = vec![json!({
            "role": "system",
            "content": request.instructions,
        })];
        messages.extend(responses_input_to_chat_messages(&request.input));

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "tools": responses_tools_to_chat_tools(&request.tools),
            "tool_choice": "auto",
            "stream": true,
        });
        if let Some(thinking) = request.thinking {
            body["thinking"] = json!({ "type": thinking });
        }
        if let Some(reasoning_effort) = request.reasoning_effort {
            body["reasoning_effort"] = json!(reasoning_effort);
        }

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("send streaming Chat Completions request")?;

        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .context("read Chat Completions body")?;
            return Err(anyhow!("Chat Completions API error {status}: {text}"));
        }

        let events = read_sse_events(response).await?;
        parse_chat_completions_sse_events(&events, sink)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponsesApiResponse {
    #[serde(default)]
    output: Vec<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatCompletionsResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(default)]
    r#type: String,
    function: ChatFunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

impl ChatCompletionsResponse {
    fn into_model_response(self) -> ModelResponse {
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

fn responses_input_to_chat_messages(input: &[Value]) -> Vec<Value> {
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

fn responses_tools_to_chat_tools(tools: &[Value]) -> Vec<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{AgentEvent, UiSink};

    #[derive(Default)]
    struct CaptureUi {
        assistant: String,
        reasoning: String,
    }

    impl UiSink for CaptureUi {
        fn on_event(&mut self, event: AgentEvent) -> Result<()> {
            match event {
                AgentEvent::AssistantDelta { text } => self.assistant.push_str(&text),
                AgentEvent::ReasoningDelta { text } => self.reasoning.push_str(&text),
                _ => {}
            }
            Ok(())
        }
    }

    #[test]
    fn parses_function_calls_and_text() {
        let response = ModelResponse::from_output(vec![
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello"}]
            }),
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "list_files",
                "arguments": "{\"path\":\".\"}"
            }),
        ]);

        assert_eq!(response.assistant_text, vec!["hello"]);
        assert_eq!(
            response.function_calls,
            vec![ModelFunctionCall {
                call_id: "call_1".into(),
                name: "list_files".into(),
                arguments: "{\"path\":\".\"}".into(),
            }]
        );
    }

    #[test]
    fn converts_responses_items_to_chat_messages() {
        let messages = responses_input_to_chat_messages(&[
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "read license"}]
            }),
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"LICENSE\"}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "{\"success\":true}"
            }),
        ]);

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "read license");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn parses_chat_completions_tool_calls() {
        let api: ChatCompletionsResponse = serde_json::from_value(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_content": "Need to inspect files.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "list_files",
                            "arguments": "{\"path\":\".\"}"
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let response = api.into_model_response();
        assert_eq!(response.function_calls[0].name, "list_files");
        assert_eq!(response.function_calls[0].call_id, "call_1");
        assert_eq!(
            response.output[0]["reasoning_content"],
            "Need to inspect files."
        );
        assert_eq!(response.output[1]["chat_message_present"], true);
    }

    #[test]
    fn replays_chat_tool_call_reasoning_without_duplicate_function_call() {
        let response = ChatCompletionsResponse {
            choices: vec![ChatChoice {
                message: ChatMessage {
                    content: Some("I'll inspect the repo.".into()),
                    reasoning_content: Some("Need a directory listing first.".into()),
                    tool_calls: vec![ChatToolCall {
                        id: "call_1".into(),
                        r#type: "function".into(),
                        function: ChatFunctionCall {
                            name: "list_files".into(),
                            arguments: "{\"path\":\".\"}".into(),
                        },
                    }],
                },
            }],
        }
        .into_model_response();

        let mut transcript = response.output.clone();
        transcript.push(json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "{\"success\":true}"
        }));
        let messages = responses_input_to_chat_messages(&transcript);

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0]["reasoning_content"],
            "Need a directory listing first."
        );
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["name"],
            "list_files"
        );
        assert_eq!(messages[1]["role"], "tool");
    }

    #[test]
    fn parses_responses_sse_text_delta_and_completed_output() {
        let events = parse_sse_text(
            r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hel"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"lo"}

event: response.completed
data: {"type":"response.completed","response":{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}]}}

"#,
        );
        let mut ui = CaptureUi::default();
        let response = parse_responses_sse_events(&events, &mut ui).unwrap();

        assert_eq!(ui.assistant, "hello");
        assert_eq!(response.assistant_text, vec!["hello"]);
    }

    #[test]
    fn parses_responses_sse_function_arguments_delta() {
        let events = parse_sse_text(
            r#"event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"read_file","arguments":""}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\":"}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"README.md\"}"}

event: response.function_call_arguments.done
data: {"type":"response.function_call_arguments.done","output_index":0}

"#,
        );
        let mut ui = CaptureUi::default();
        let response = parse_responses_sse_events(&events, &mut ui).unwrap();

        assert_eq!(
            response.function_calls,
            vec![ModelFunctionCall {
                call_id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"README.md\"}".into(),
            }]
        );
    }

    #[test]
    fn parses_chat_completions_sse_content_reasoning_and_tool_args() {
        let events = parse_sse_text(
            r#"data: {"choices":[{"delta":{"reasoning_content":"Need "}}]}

data: {"choices":[{"delta":{"reasoning_content":"file."}}]}

data: {"choices":[{"delta":{"content":"Reading"}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]}}]}

data: [DONE]

"#,
        );
        let mut ui = CaptureUi::default();
        let response = parse_chat_completions_sse_events(&events, &mut ui).unwrap();

        assert_eq!(ui.reasoning, "Need file.");
        assert_eq!(ui.assistant, "Reading");
        assert_eq!(response.assistant_text, vec!["Reading"]);
        assert_eq!(response.function_calls[0].name, "read_file");
        assert_eq!(
            response.function_calls[0].arguments,
            "{\"path\":\"README.md\"}"
        );
        assert_eq!(response.output[0]["reasoning_content"], "Need file.");
    }
}
