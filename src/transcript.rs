//! Rendering saved transcripts as text for restoring conversations.
//!
//! Sessions restored with `Session::from_transcript` can't call tools, and
//! `Session::set_transcript` needs the macOS 27 SDK. So a conversation is
//! restored by starting a fresh session (with tools) whose instructions
//! contain the earlier conversation, rendered here with speaker labels.

use serde_json::Value;

/// First line of instructions that carry a restored conversation.
pub const RESTORED_HEADER: &str = "Earlier conversation:";

/// Renders a Foundation Models transcript JSON as labeled lines. Existing
/// instructions (an earlier restore or a summary) are kept at the top.
/// Falls back to `fm_rs`'s unlabeled rendering if the format is unexpected.
pub fn render(transcript_json: &str) -> String {
    match serde_json::from_str::<Value>(transcript_json) {
        Ok(value) if entries(&value).is_some() => render_entries(entries(&value).unwrap()),
        _ => fm_rs::transcript_to_text(transcript_json).unwrap_or_default(),
    }
}

/// Builds instructions that restore a conversation, or `None` if there is
/// nothing to restore.
pub fn restored_instructions(transcript_json: &str) -> Option<String> {
    let text = render(transcript_json);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Don't stack headers when a restored conversation is restored again.
    let body = text.strip_prefix(RESTORED_HEADER).unwrap_or(text).trim_start();
    Some(format!("{RESTORED_HEADER}\n{body}"))
}

fn entries(value: &Value) -> Option<&Vec<Value>> {
    value.pointer("/transcript/entries")?.as_array()
}

fn render_entries(entries: &[Value]) -> String {
    let mut lines = Vec::new();
    for entry in entries {
        let text = entry_text(entry);
        match entry["role"].as_str() {
            Some("instructions") if !text.is_empty() => lines.push(text),
            Some("user") if !text.is_empty() => lines.push(format!("[user] {text}")),
            Some("response") => {
                for name in tool_call_names(entry) {
                    lines.push(format!("[assistant called tool {name}]"));
                }
                if !text.is_empty() {
                    lines.push(format!("[assistant] {text}"));
                }
            }
            Some("tool") if !text.is_empty() => lines.push(format!("[tool result] {text}")),
            _ => {}
        }
    }
    lines.join("\n")
}

fn entry_text(entry: &Value) -> String {
    entry["contents"]
        .as_array()
        .map(|contents| {
            contents
                .iter()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Names of the tools a response called. `fm_rs` routes every call through
/// one generic `invoke_tool` function, with the real name in its arguments.
fn tool_call_names(entry: &Value) -> Vec<String> {
    let Some(calls) = entry["toolCalls"].as_array() else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let args = call["arguments"].as_str()?;
            let args: Value = serde_json::from_str(args).ok()?;
            args["toolName"]
                .as_str()
                .or_else(|| call["name"].as_str())
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape of a real transcript with a tool call, trimmed down.
    const TRANSCRIPT: &str = r#"{
      "type": "FoundationModels.Transcript",
      "version": "1.1",
      "transcript": { "entries": [
        { "role": "instructions", "contents": [{ "type": "text", "text": "Conversation summary:\nThe user has a dog named Rex." }], "tools": [] },
        { "role": "user", "contents": [{ "type": "text", "text": "ada: what's my secret number?" }] },
        { "role": "response", "toolCalls": [{ "name": "invoke_tool", "arguments": "{\"toolName\": \"getSecretNumber\", \"arguments\": []}" }] },
        { "role": "tool", "toolName": "invoke_tool", "contents": [{ "type": "text", "text": "7319" }] },
        { "role": "response", "contents": [{ "type": "text", "text": "It's 7319." }] }
      ] }
    }"#;

    #[test]
    fn labels_each_speaker() {
        assert_eq!(
            render(TRANSCRIPT),
            "Conversation summary:\nThe user has a dog named Rex.\n\
             [user] ada: what's my secret number?\n\
             [assistant called tool getSecretNumber]\n\
             [tool result] 7319\n\
             [assistant] It's 7319."
        );
    }

    #[test]
    fn restored_instructions_start_with_header() {
        let instructions = restored_instructions(TRANSCRIPT).unwrap();
        assert!(instructions.starts_with("Earlier conversation:\nConversation summary:"));
    }

    #[test]
    fn restoring_twice_does_not_stack_headers() {
        let restored = TRANSCRIPT.replace(
            "Conversation summary:",
            "Earlier conversation:\\nConversation summary:",
        );
        let instructions = restored_instructions(&restored).unwrap();
        assert_eq!(instructions.matches(RESTORED_HEADER).count(), 1, "{instructions}");
    }

    #[test]
    fn empty_transcript_restores_nothing() {
        let empty = r#"{"transcript": {"entries": []}}"#;
        assert_eq!(restored_instructions(empty), None);
    }
}
