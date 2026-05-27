use crate::tools::ToolResult;
use serde_json::{json, Value};

pub(super) const MODEL_VISIBLE_TOOL_OUTPUT_LIMIT: usize = 12 * 1024;

pub(super) fn function_call_output(call_id: &str, result: &ToolResult) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": serde_json::to_string(&project_tool_result_for_model(result)).unwrap()
    })
}

fn project_tool_result_for_model(result: &ToolResult) -> Value {
    let output = truncate_model_visible_output(&result.output, MODEL_VISIBLE_TOOL_OUTPUT_LIMIT);
    let original_bytes = if result.truncated {
        result.original_bytes
    } else {
        output.original_bytes
    };
    let truncated = result.truncated || output.truncated;
    let preview_bytes = if output.truncated {
        output.preview_bytes
    } else if result.truncated {
        result.preview_bytes
    } else {
        output.preview_bytes
    };
    let omitted_bytes = original_bytes.saturating_sub(preview_bytes);
    json!({
        "success": result.success,
        "output": output.preview,
        "error": result.error,
        "truncated": truncated,
        "original_bytes": original_bytes,
        "preview_bytes": preview_bytes,
        "omitted_bytes": omitted_bytes,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelVisibleOutput {
    preview: String,
    truncated: bool,
    original_bytes: usize,
    preview_bytes: usize,
    omitted_bytes: usize,
}

fn truncate_model_visible_output(text: &str, limit: usize) -> ModelVisibleOutput {
    if text.len() <= limit {
        return ModelVisibleOutput {
            preview: text.to_string(),
            truncated: false,
            original_bytes: text.len(),
            preview_bytes: text.len(),
            omitted_bytes: 0,
        };
    }

    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let preview = text[..end].to_string();
    ModelVisibleOutput {
        preview,
        truncated: true,
        original_bytes: text.len(),
        preview_bytes: end,
        omitted_bytes: text.len() - end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_visible_tool_output_projection_keeps_small_output() {
        let result = ToolResult::ok("small output");
        let value = project_tool_result_for_model(&result);

        assert_eq!(value["success"], true);
        assert_eq!(value["output"], "small output");
        assert_eq!(value["truncated"], false);
        assert_eq!(value["original_bytes"], 12);
        assert_eq!(value["preview_bytes"], 12);
        assert_eq!(value["omitted_bytes"], 0);
    }

    #[test]
    fn model_visible_tool_output_projection_truncates_large_output() {
        let output = format!("{}你好", "x".repeat(MODEL_VISIBLE_TOOL_OUTPUT_LIMIT + 8));
        let result = ToolResult::ok(output.clone());
        let value = project_tool_result_for_model(&result);
        let preview = value["output"].as_str().unwrap();

        assert_eq!(value["success"], true);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["original_bytes"], output.len());
        assert!(preview.len() <= MODEL_VISIBLE_TOOL_OUTPUT_LIMIT);
        assert_eq!(
            value["preview_bytes"].as_u64().unwrap() as usize,
            preview.len()
        );
        assert_eq!(
            value["omitted_bytes"].as_u64().unwrap() as usize,
            output.len() - preview.len()
        );
        assert!(output.starts_with(preview));
    }
}
