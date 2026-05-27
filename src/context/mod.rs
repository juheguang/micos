use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 200_000;
pub const DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS: usize = 1_000_000;

pub fn default_context_window_tokens_for_model(model: &str, base_url: &str) -> usize {
    if base_url.contains("api.deepseek.com") && model.starts_with("deepseek-v4-") {
        DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    } else {
        DEFAULT_CONTEXT_WINDOW_TOKENS
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextStats {
    pub total_tokens_estimate: usize,
    pub max_tokens: usize,
    pub usage_percent: usize,
    pub categories: Vec<ContextCategory>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelContext {
    pub input: Vec<Value>,
    pub instructions: String,
    pub tools: Vec<Value>,
    pub stats: ContextStats,
}

#[derive(Clone, Debug)]
pub struct ContextBuilder {
    max_tokens: usize,
}

impl ContextBuilder {
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }

    pub fn build(
        &self,
        input: Vec<Value>,
        instructions: String,
        tools: Vec<Value>,
    ) -> ModelContext {
        let stats = estimate_context_stats(&input, &instructions, &tools, self.max_tokens);
        ModelContext {
            input,
            instructions,
            tools,
            stats,
        }
    }
}

pub fn estimate_context_stats(
    input: &[Value],
    instructions: &str,
    tools: &[Value],
    max_tokens: usize,
) -> ContextStats {
    let mut categories = vec![
        ContextCategory::new("instructions", estimate_text_tokens(instructions)),
        ContextCategory::new("tool_schemas", estimate_json_values_tokens(tools, 2)),
    ];
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut compacted_summary = 0usize;
    let mut tool_calls = 0usize;
    let mut tool_outputs = 0usize;
    let mut other = 0usize;

    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => match item.get("role").and_then(Value::as_str) {
                Some("user") if is_compacted_summary_message(item) => {
                    compacted_summary += estimate_json_tokens(item, 4)
                }
                Some("user") => user_messages += estimate_json_tokens(item, 4),
                Some("assistant") => assistant_messages += estimate_json_tokens(item, 4),
                _ => other += estimate_json_tokens(item, 4),
            },
            Some("function_call") => tool_calls += estimate_json_tokens(item, 2),
            Some("function_call_output") => tool_outputs += estimate_json_tokens(item, 2),
            _ => other += estimate_json_tokens(item, 4),
        }
    }

    categories.extend([
        ContextCategory::new("user_messages", user_messages),
        ContextCategory::new("assistant_messages", assistant_messages),
        ContextCategory::new("compacted_summary", compacted_summary),
        ContextCategory::new("tool_calls", tool_calls),
        ContextCategory::new("tool_outputs", tool_outputs),
        ContextCategory::new("other", other),
    ]);

    let used_tokens = categories
        .iter()
        .map(|category| category.tokens)
        .sum::<usize>();
    let free_tokens = max_tokens.saturating_sub(used_tokens);
    categories.push(ContextCategory::new("free_space", free_tokens));

    let usage_percent = if max_tokens == 0 {
        0
    } else {
        ((used_tokens as f64 / max_tokens as f64) * 100.0).round() as usize
    };

    ContextStats {
        total_tokens_estimate: used_tokens,
        max_tokens,
        usage_percent: usage_percent.min(100),
        categories,
    }
}

pub fn estimate_text_tokens(text: &str) -> usize {
    estimate_len_tokens(text.chars().count(), 4)
}

fn estimate_json_values_tokens(values: &[Value], chars_per_token: usize) -> usize {
    values
        .iter()
        .map(|value| estimate_json_tokens(value, chars_per_token))
        .sum()
}

fn estimate_json_tokens(value: &Value, chars_per_token: usize) -> usize {
    estimate_len_tokens(value.to_string().chars().count(), chars_per_token)
}

pub fn compacted_summary_message(summary: &str) -> Value {
    serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": format!("This is a compacted summary of earlier model-visible context.\n\n{summary}")
        }]
    })
}

fn is_compacted_summary_message(item: &Value) -> bool {
    item.get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .is_some_and(|text| {
            text.starts_with("This is a compacted summary of earlier model-visible context.")
        })
}

fn estimate_len_tokens(chars: usize, chars_per_token: usize) -> usize {
    if chars == 0 {
        return 0;
    }
    let divisor = chars_per_token.max(1);
    chars.div_ceil(divisor).max(1)
}

impl ContextCategory {
    fn new(name: impl Into<String>, tokens: usize) -> Self {
        Self {
            name: name.into(),
            tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimates_text_tokens_with_ceiling() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abc"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
    }

    #[test]
    fn deepseek_v4_defaults_to_one_million_context_tokens() {
        assert_eq!(
            default_context_window_tokens_for_model(
                "deepseek-v4-flash",
                "https://api.deepseek.com/chat/completions"
            ),
            DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(
            default_context_window_tokens_for_model(
                "deepseek-chat",
                "https://api.deepseek.com/chat/completions"
            ),
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
    }

    #[test]
    fn classifies_context_categories() {
        let input = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}),
            compacted_summary_message("summary"),
            json!({"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"command\":\"cargo test\"}"}),
            json!({"type":"function_call_output","call_id":"call_1","output":"large output"}),
        ];
        let tools = vec![json!({"name":"shell","description":"run shell"})];
        let stats = estimate_context_stats(&input, "system", &tools, 1_000);

        let category = |name: &str| {
            stats
                .categories
                .iter()
                .find(|category| category.name == name)
                .map(|category| category.tokens)
                .unwrap_or_default()
        };
        assert!(category("instructions") > 0);
        assert!(category("tool_schemas") > 0);
        assert!(category("user_messages") > 0);
        assert!(category("assistant_messages") > 0);
        assert!(category("compacted_summary") > 0);
        assert!(category("tool_calls") > 0);
        assert!(category("tool_outputs") > 0);
        assert!(category("free_space") > 0);
        assert_eq!(
            stats.total_tokens_estimate + category("free_space"),
            stats.max_tokens
        );
    }
}
