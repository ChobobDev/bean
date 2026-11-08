//! Tools that look at the Discord channel or server Bean is talking in.

use std::sync::Arc;

use chrono::{DateTime, Local};
use fm_rs::{Tool, ToolOutput};
use serde_json::{Value, json};
use serenity::all::{ChannelType, GetMessages, Message};

use super::{DiscordChannel, Shared, arg_u64, failure, output};

const DEFAULT_MESSAGES: u64 = 15;
const MAX_MESSAGES: u64 = 30;
/// Per-message and total caps keep the tool output from flooding the
/// model's small context window.
const MAX_MESSAGE_CHARS: usize = 300;
const MAX_OUTPUT_CHARS: usize = 3000;
const MAX_LISTED: usize = 25;

pub struct RecentMessages {
    shared: Shared,
    discord: Arc<DiscordChannel>,
}

impl RecentMessages {
    pub(super) fn new(shared: Shared, discord: Arc<DiscordChannel>) -> Self {
        Self { shared, discord }
    }
}

impl Tool for RecentMessages {
    fn name(&self) -> &str {
        "readRecentMessages"
    }

    fn description(&self) -> &str {
        "Reads the most recent messages in this Discord channel, including ones not sent to you. \
         Use it when someone asks what was discussed, wants a summary of the chat, or refers to \
         a message you haven't seen."
    }

    fn arguments_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "description": format!("How many messages to read, 1 to {MAX_MESSAGES}")
                }
            }
        })
    }

    fn call(&self, arguments: Value) -> fm_rs::Result<ToolOutput> {
        let count = arg_u64(&arguments, "count")
            .unwrap_or(DEFAULT_MESSAGES)
            .clamp(1, MAX_MESSAGES);
        self.shared.record(format!("Read the last {count} messages"));

        let request = GetMessages::new().limit(count as u8);
        let messages = match self
            .shared
            .block_on(self.discord.channel_id.messages(&self.discord.http, request))
        {
            Ok(Ok(messages)) => messages,
            Ok(Err(e)) => return failure(format!("couldn't read the channel: {e}")),
            Err(e) => return failure(e),
        };
        if messages.is_empty() {
            return output("The channel has no messages.");
        }
        // Discord returns newest first.
        output(format_messages(messages.iter()))
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

pub struct ServerInfo {
    shared: Shared,
    discord: Arc<DiscordChannel>,
}

impl ServerInfo {
    pub(super) fn new(shared: Shared, discord: Arc<DiscordChannel>) -> Self {
        Self { shared, discord }
    }
}

impl Tool for ServerInfo {
    fn name(&self) -> &str {
        "getServerInfo"
    }

    fn description(&self) -> &str {
        "Returns facts about this Discord server: its name, member count, how many members are \
         online, when it was created, its roles, and its channels. Use it for questions about the \
         server itself."
    }

    fn arguments_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn call(&self, _arguments: Value) -> fm_rs::Result<ToolOutput> {
        let Some(guild_id) = self.discord.guild_id else {
            return output("This is a direct message, not a server.");
        };
        self.shared.record("Looked up the server");
        let http = &self.discord.http;

        let fetched = self.shared.block_on(async {
            let guild = guild_id.to_partial_guild_with_counts(http).await?;
            let channels = guild_id.channels(http).await?;
            Ok::<_, serenity::Error>((guild, channels))
        });
        let (guild, channels) = match fetched {
            Ok(Ok(fetched)) => fetched,
            Ok(Err(e)) => return failure(format!("couldn't look up the server: {e}")),
            Err(e) => return failure(e),
        };

        let mut lines = vec![format!("Server: {}", guild.name)];
        if let Some(description) = guild.description.as_deref().filter(|d| !d.is_empty()) {
            lines.push(format!("Description: {description}"));
        }
        if let Some(members) = guild.approximate_member_count {
            let online = guild
                .approximate_presence_count
                .map(|n| format!(" ({n} online)"))
                .unwrap_or_default();
            lines.push(format!("Members: about {members}{online}"));
        }
        lines.push(format!("Created: {}", guild_id.created_at().format("%-d %B %Y")));

        // Skip @everyone (same ID as the server) and roles managed by bots
        // or integrations.
        let mut roles: Vec<_> = guild
            .roles
            .values()
            .filter(|r| r.id.get() != guild_id.get() && !r.managed)
            .collect();
        roles.sort_by(|a, b| b.position.cmp(&a.position));
        let roles: Vec<_> = roles.iter().take(MAX_LISTED).map(|r| r.name.as_str()).collect();
        if !roles.is_empty() {
            lines.push(format!("Roles: {}", roles.join(", ")));
        }

        let mut channels: Vec<_> = channels.values().collect();
        channels.sort_by_key(|c| c.position);
        for (label, kind) in [("Text channels", ChannelType::Text), ("Voice channels", ChannelType::Voice)] {
            let names: Vec<_> = channels
                .iter()
                .filter(|c| c.kind == kind)
                .take(MAX_LISTED)
                .map(|c| format!("#{}", c.name))
                .collect();
            if !names.is_empty() {
                lines.push(format!("{label}: {}", names.join(", ")));
            }
        }
        output(lines.join("\n"))
    }
}
