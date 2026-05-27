use crate::ui::UiSink;
use anyhow::{Context, Result};
use futures_util::StreamExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SseEvent {
    pub(super) event: Option<String>,
    pub(super) data: String,
}

pub(super) async fn read_sse_events_streaming<S, F>(
    response: reqwest::Response,
    sink: &mut S,
    mut on_event: F,
) -> Result<Vec<SseEvent>>
where
    S: UiSink,
    F: FnMut(&SseEvent, &mut S) -> Result<()>,
{
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
                on_event(&event, sink)?;
                events.push(event);
            }
        }
    }

    if let Some(event) = parse_sse_frame(&buffer) {
        on_event(&event, sink)?;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
pub(super) fn parse_sse_text(text: &str) -> Vec<SseEvent> {
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
