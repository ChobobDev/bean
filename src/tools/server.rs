//! Facts about the Discord server Bean is talking in.

use std::collections::HashMap;

use serenity::all::{ChannelId, ChannelType, GuildChannel, GuildId, PartialGuild};

use super::{Args, BeanTool, ToolContext, ToolError, ToolResult};

/// Most roles or channels listed, to keep the output short.
const MAX_LISTED: usize = 25;

pub struct ServerInfo;

impl BeanTool for ServerInfo {
    fn name(&self) -> &'static str {
        "getServerInfo"
    }

    fn description(&self) -> &'static str {
        "Returns facts about this Discord server: its name, member count, how many members are \
         online, when it was created, its roles, and its channels. Use it for questions about the \
         server itself."
    }

    fn available(&self, ctx: &ToolContext) -> bool {
        ctx.discord().is_some()
    }

    fn run(&self, _args: &Args, ctx: &ToolContext) -> ToolResult {
        let discord = ctx.require_discord()?;
        let Some(guild_id) = discord.guild_id else {
            return Ok("This is a direct message, not a server.".into());
        };
        ctx.record("Looked up the server");

        let http = &discord.http;
        let (guild, channels) = ctx
            .block_on(async {
                let guild = guild_id.to_partial_guild_with_counts(http).await?;
                let channels = guild_id.channels(http).await?;
                Ok::<_, serenity::Error>((guild, channels))
            })?
            .map_err(|e| ToolError::new(format!("couldn't look up the server: {e}")))?;
        Ok(describe(guild_id, &guild, &channels))
    }
}

fn describe(
    guild_id: GuildId,
    guild: &PartialGuild,
    channels: &HashMap<ChannelId, GuildChannel>,
) -> String {
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
    lines.push(format!(
        "Created: {}",
        guild_id.created_at().format("%-d %B %Y")
    ));

    // Skip @everyone (same ID as the server) and roles managed by bots or
    // integrations.
    let mut roles: Vec<_> = guild
        .roles
        .values()
        .filter(|r| r.id.get() != guild_id.get() && !r.managed)
        .collect();
    roles.sort_by(|a, b| b.position.cmp(&a.position));
    let roles: Vec<_> = roles
        .iter()
        .take(MAX_LISTED)
        .map(|r| r.name.as_str())
        .collect();
    if !roles.is_empty() {
        lines.push(format!("Roles: {}", roles.join(", ")));
    }

    let mut channels: Vec<_> = channels.values().collect();
    channels.sort_by_key(|c| c.position);
    for (label, kind) in [
        ("Text channels", ChannelType::Text),
        ("Voice channels", ChannelType::Voice),
    ] {
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
    lines.join("\n")
}
