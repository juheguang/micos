use super::chat::{
    emit_chat_completions_sse_delta, parse_chat_completions_sse_events, ChatCompletionsResponse,
    NoopUi, ResponsesApiResponse,
};
use super::responses::{emit_responses_sse_delta, parse_responses_sse_events};
use super::sse::read_sse_events_streaming;
use super::types::{
    responses_input_to_chat_messages, responses_tools_to_chat_tools, ModelClient, ModelRequest,
    ModelResponse, OpenAiModelClient,
};
use crate::ui::UiSink;
use anyhow::{anyhow, Context, Result};
use serde_json::json;

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

        let events = read_sse_events_streaming(response, sink, emit_responses_sse_delta).await?;
        let mut noop = NoopUi;
        parse_responses_sse_events(&events, &mut noop)
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

        let events =
            read_sse_events_streaming(response, sink, emit_chat_completions_sse_delta).await?;
        let mut noop = NoopUi;
        parse_chat_completions_sse_events(&events, &mut noop)
    }
}
