//! Reading the channel Bean is talking in.

use chrono::{DateTime, Local};
use serenity::all::{GetMessages, Message};

use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

const DEFAULT_MESSAGES: u64 = 15;
/// Keep in sync with the `count` description below.
const MAX_MESSAGES: u64 = 30;
/// Per-message and total caps keep the output from flooding the model's
/// context window.
const MAX_MESSAGE_CHARS: usize = 300;
const MAX_OUTPUT_CHARS: usize = 3000;

pub struct RecentMessages;

impl BeanTool for RecentMessages {
    fn name(&self) -> &'static str {
        "readRecentMessages"
    }

    fn description(&self) -> &'static str {
        "Reads the most recent messages in this Discord channel, including ones not sent to you. \
         Use it when someone asks what was discussed, wants a summary of the chat, or refers to \
         a message you haven't seen."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[Param::integer(
            "count",
            "How many messages to read, 1 to 30",
        )];
        PARAMS
    }

    fn available(&self, ctx: &ToolContext) -> bool {
        ctx.discord().is_some()
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let discord = ctx.require_discord()?;
        let count = args
            .integer("count")
            .unwrap_or(DEFAULT_MESSAGES)
            .clamp(1, MAX_MESSAGES);
        ctx.record(format!("Read the last {count} messages"));

        let request = GetMessages::new().limit(count as u8);
        let messages = ctx
            .block_on(discord.channel_id.messages(&discord.http, request))?
            .map_err(|e| ToolError::new(format!("couldn't read the channel: {e}")))?;
        if messages.is_empty() {
            return Ok("The channel has no messages.".into());
        }
        // Discord returns newest first.
        Ok(format_messages(messages.iter()))
    }
}

/// Formats messages (newest first) as oldest-first lines, dropping the
/// oldest ones if the total gets too long.
fn format_messages<'a>(newest_first: impl Iterator<Item = &'a Message>) -> String {
    let mut lines = Vec::new();
    let mut total = 0;
    for message in newest_first {
        let line = format_message(message);
        total += line.chars().count() + 1;
        if total > MAX_OUTPUT_CHARS && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

fn format_message(message: &Message) -> String {
    let mut content = message.content.clone();
    for user in &message.mentions {
        let name = format!("@{}", user.display_name());
        content = content
            .replace(&format!("<@{}>", user.id), &name)
            .replace(&format!("<@!{}>", user.id), &name);
    }
    if content.chars().count() > MAX_MESSAGE_CHARS {
        content = content.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "…";
    }
    if !message.attachments.is_empty() {
        content.push_str(&format!(" [{} attachment(s)]", message.attachments.len()));
    }
    if content.trim().is_empty() {
        content = "[no text]".into();
    }
    let time = DateTime::from_timestamp(message.timestamp.unix_timestamp(), 0)
        .map(|t| t.with_timezone(&Local).format("%b %-d %H:%M").to_string())
        .unwrap_or_default();
    format!("[{time}] {}: {content}", message.author.display_name())
}
