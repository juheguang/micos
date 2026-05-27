use crate::prompt::COMPACT_SUMMARY_FORMAT;
use serde_json::Value;

pub(super) const COMPACT_TAIL_DEFAULT_MESSAGES: usize = 8;
pub(super) const COMPACT_SUMMARY_FORMAT_VERSION: usize = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompactPlan {
    pub(super) summary_input: Vec<Value>,
    pub(super) retained_tail: Vec<Value>,
    pub(super) messages_replaced: usize,
    pub(super) retained_messages: usize,
    pub(super) latest_user_text: Option<String>,
}

impl CompactPlan {
    pub(super) fn from_transcript(transcript: &[Value]) -> Self {
        let retained_tail = select_compact_tail(transcript, COMPACT_TAIL_DEFAULT_MESSAGES);
        let retained_messages = retained_tail.len();
        let messages_replaced = transcript.len().saturating_sub(retained_messages);
        Self {
            summary_input: transcript.to_vec(),
            retained_tail,
            messages_replaced,
            retained_messages,
            latest_user_text: latest_user_text(transcript),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompactValidation {
    pub(super) passed: bool,
    pub(super) status: String,
    pub(super) message: String,
}

pub(super) fn validate_compact_summary(
    summary: &str,
    latest_user_text: Option<&str>,
) -> CompactValidation {
    let summary = normalize_compact_headings(summary);
    let summary = summary.trim();
    if summary.is_empty() {
        return CompactValidation::failed("empty_summary");
    }

    let mut previous = 0usize;
    for heading in COMPACT_SUMMARY_FORMAT {
        let marker = format!("## {heading}");
        let Some(position) = summary[previous..]
            .find(&marker)
            .map(|offset| previous + offset)
        else {
            return CompactValidation::failed(format!("missing_heading:{heading}"));
        };
        previous = position + marker.len();
    }

    if let Some(latest) = latest_user_text.and_then(latest_user_validation_excerpt) {
        if !normalize_compact_text(summary).contains(&normalize_compact_text(&latest)) {
            return CompactValidation::failed("missing_latest_user_request");
        }
    }

    if !contains_verification_status(summary) {
        return CompactValidation::failed("missing_verification_status");
    }

    CompactValidation {
        passed: true,
        status: "passed".into(),
        message: "passed".into(),
    }
}

pub(super) fn normalize_compact_summary(summary: &str) -> String {
    normalize_compact_headings(summary).trim().to_string()
}

pub(super) fn compression_ratio_percent(before_tokens: usize, after_tokens: usize) -> usize {
    if before_tokens == 0 {
        return 0;
    }
    before_tokens
        .saturating_sub(after_tokens)
        .saturating_mul(100)
        / before_tokens
}

fn select_compact_tail(transcript: &[Value], max_messages: usize) -> Vec<Value> {
    if transcript.is_empty() || max_messages == 0 {
        return Vec::new();
    }

    let mut start = transcript.len().saturating_sub(max_messages);
    if let Some(latest_user_index) = transcript.iter().rposition(is_user_message) {
        start = start.min(latest_user_index);
    }
    start = expand_tail_for_tool_pairs(transcript, start);
    transcript[start..].to_vec()
}

fn expand_tail_for_tool_pairs(transcript: &[Value], mut start: usize) -> usize {
    loop {
        let mut adjusted = start;
        for item in &transcript[start..] {
            if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
                continue;
            }
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                continue;
            };
            let has_call_in_tail = transcript[start..]
                .iter()
                .any(|candidate| is_function_call_with_id(candidate, call_id));
            if has_call_in_tail {
                continue;
            }
            if let Some(call_index) = transcript[..start]
                .iter()
                .rposition(|candidate| is_function_call_with_id(candidate, call_id))
            {
                adjusted = adjusted.min(call_index);
            }
        }
        if adjusted == start {
            return start;
        }
        start = adjusted;
    }
}

fn is_function_call_with_id(item: &Value, call_id: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some("function_call")
        && item.get("call_id").and_then(Value::as_str) == Some(call_id)
}

fn is_user_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
}

fn latest_user_text(transcript: &[Value]) -> Option<String> {
    transcript
        .iter()
        .rev()
        .find(|item| is_user_message(item))
        .and_then(message_text)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn message_text(item: &Value) -> Option<&str> {
    item.get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
}

fn latest_user_validation_excerpt(text: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    Some(compact.chars().take(80).collect())
}

fn normalize_compact_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn contains_verification_status(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|word| {
            matches!(
                word.as_str(),
                "verification"
                    | "verified"
                    | "verify"
                    | "test"
                    | "tests"
                    | "check"
                    | "checks"
                    | "unverified"
            )
        })
}

fn normalize_compact_headings(summary: &str) -> String {
    summary
        .lines()
        .map(|line| match compact_heading_for_line(line) {
            Some((heading, rest)) if rest.is_empty() => format!("## {heading}"),
            Some((heading, rest)) => format!("## {heading}\n{rest}"),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_heading_for_line(line: &str) -> Option<(&'static str, String)> {
    let candidate = heading_candidate(line);
    for heading in COMPACT_SUMMARY_FORMAT {
        let heading_key = normalize_heading_line(heading);
        let candidate_key = normalize_heading_line(&candidate);
        if candidate_key == heading_key {
            return Some((*heading, String::new()));
        }
        let Some((prefix, rest)) = candidate.split_once(':') else {
            continue;
        };
        if normalize_heading_line(prefix) == heading_key {
            return Some((*heading, rest.trim().to_string()));
        }
    }
    None
}

fn heading_candidate(line: &str) -> String {
    let trimmed = line.trim();
    let trimmed = trimmed.trim_start_matches('#').trim();
    let trimmed = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .unwrap_or(trimmed)
        .trim();
    let trimmed = trimmed
        .split_once('.')
        .filter(|(prefix, _)| prefix.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(_, rest)| rest.trim())
        .unwrap_or(trimmed);
    let trimmed = trimmed.trim_matches('*').trim_matches('_').trim();
    trimmed.to_string()
}

fn normalize_heading_line(line: &str) -> String {
    let trimmed = line.trim().trim_end_matches(':').trim();
    trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

impl CompactValidation {
    fn failed(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            passed: false,
            status: format!("failed:{message}"),
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_tail_expands_to_preserve_tool_call_output_pair() {
        let transcript = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"older"}]}),
            json!({"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"middle"}]}),
            json!({"type":"function_call_output","call_id":"call_1","output":"done"}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"latest"}]}),
        ];

        let tail = select_compact_tail(&transcript, 2);

        assert_eq!(tail.len(), 4);
        assert_eq!(
            tail[0].get("type").and_then(Value::as_str),
            Some("function_call")
        );
        assert_eq!(
            tail[2].get("type").and_then(Value::as_str),
            Some("function_call_output")
        );
    }

    #[test]
    fn compact_summary_validation_checks_headings_latest_user_and_verification() {
        let valid = "## Primary Request and Intent\nHandle latest user request compile errors.\n\n## Key Technical Concepts\nCompact governance.\n\n## Files and Code Sections\nsrc/agent.rs.\n\n## Errors and Fixes\nNone.\n\n## Decisions Made\nRetain tail.\n\n## Pending Tasks\nRun tests.\n\n## Current Work\nValidation.\n\n## Next Step\nRun verification.";
        assert!(validate_compact_summary(valid, Some("latest user request compile errors")).passed);

        assert_eq!(
            validate_compact_summary("", Some("latest")).message,
            "empty_summary"
        );
        assert_eq!(
            validate_compact_summary("## Primary Request and Intent\nlatest", Some("latest"))
                .message,
            "missing_heading:Key Technical Concepts"
        );
        let missing_latest = valid.replace("latest user request compile errors", "other request");
        assert_eq!(
            validate_compact_summary(&missing_latest, Some("latest user request compile errors"))
                .message,
            "missing_latest_user_request"
        );
        let missing_verification = valid.replace("Run tests.", "Do next work.");
        let missing_verification = missing_verification.replace("Run verification.", "Continue.");
        assert_eq!(
            validate_compact_summary(
                &missing_verification,
                Some("latest user request compile errors")
            )
            .message,
            "missing_verification_status"
        );
    }

    #[test]
    fn compact_summary_validation_accepts_common_heading_variants() {
        let summary = "Primary Request and Intent: Handle latest user request compile errors.\n\n### Key Technical Concepts\nCompact governance.\n\n**Files and Code Sections**\nsrc/agent.rs.\n\n4. Errors and Fixes\nNone.\n\n- Decisions Made\nRetain tail.\n\n## Pending Tasks\nRun tests.\n\nCurrent Work\nValidation.\n\nNext Step\nRun verification.";

        let normalized = normalize_compact_summary(summary);

        assert!(
            validate_compact_summary(summary, Some("latest user request compile errors")).passed
        );
        assert!(normalized.contains("## Primary Request and Intent"));
        assert!(normalized.contains("Handle latest user request compile errors."));
        assert!(normalized.contains("## Files and Code Sections"));
        assert!(normalized.contains("## Next Step"));
    }
}
