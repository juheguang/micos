mod chat;
mod openai;
mod responses;
mod sse;
mod types;

pub use chat::ChatCompletionsResponse;
pub use types::{
    responses_input_to_chat_messages, responses_tools_to_chat_tools, ModelClient,
    ModelFunctionCall, ModelRequest, ModelResponse, OpenAiModelClient,
};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{AgentEvent, UiSink};
    use anyhow::Result;
    use serde_json::json;

    use super::chat::{
        emit_chat_completions_sse_delta, parse_chat_completions_sse_events, ChatChoice,
        ChatFunctionCall, ChatMessage, ChatToolCall,
    };
    use super::responses::parse_responses_sse_events;
    use super::sse::parse_sse_text;

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

    #[test]
    fn emits_chat_completions_delta_as_events_arrive() {
        let mut events = parse_sse_text(
            r#"data: {"choices":[{"delta":{"reasoning_content":"think","content":"hi"}}]}

"#,
        );
        let mut ui = CaptureUi::default();
        emit_chat_completions_sse_delta(&events.remove(0), &mut ui).unwrap();
        assert_eq!(ui.assistant, "hi");
        assert_eq!(ui.reasoning, "think");
    }
}
