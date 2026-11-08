//! Discord event handling: chat messages and slash commands.

use std::sync::Arc;

use serenity::all::{
    Command, CommandInteraction, Context, CreateAllowedMentions, CreateCommand, CreateMessage,
    EditInteractionResponse, EventHandler, Interaction, Message, Ready,
};
use serenity::async_trait;
use tracing::{debug, error, info, warn};

use crate::conversation::ConversationStore;
use crate::prompt::{build_prompt, clean_prompt, user_facing_error};

/// Discord's hard limit on message length.
const DISCORD_MESSAGE_LIMIT: usize = 2000;

pub struct Handler {
    store: Arc<ConversationStore>,
}

impl Handler {
    pub fn new(store: Arc<ConversationStore>) -> Self {
        Self { store }
    }

    /// Answers `prompt` in `msg`'s channel, using that channel's memory.
    async fn reply(&self, ctx: &Context, msg: &Message, prompt: String) {
        let typing = msg.channel_id.start_typing(&ctx.http);

        // Waits here if this channel is already being answered.
        let conversation = self.store.conversation(msg.channel_id.get()).lock_owned().await;
        let engine = self.store.engine();
        let result = tokio::task::spawn_blocking(move || {
            let mut conversation = conversation;
            conversation.respond(&engine, &prompt)
        })
        .await;
        typing.stop();

        let mut text = match result {
            Ok(Ok((text, outcome))) => {
                let mut text = if text.trim().is_empty() { "(no response)".into() } else { text };
                if outcome.memory_lost {
                    text.push_str("\n-# I ran out of memory and had to start this conversation over.");
                } else if outcome.compacted {
                    text.push_str("\n-# Older messages were summarized to make room.");
                }
                text
            }
            Ok(Err(e)) => {
                warn!("model error: {e}");
                user_facing_error(&e)
            }
            Err(e) => {
                error!("generation task failed: {e}");
                "Something went wrong on my end.".into()
            }
        };
        if text.chars().count() > DISCORD_MESSAGE_LIMIT {
            text = text.chars().take(DISCORD_MESSAGE_LIMIT).collect();
        }

        let message = CreateMessage::new()
            .content(text)
            .reference_message(msg)
            .allowed_mentions(no_pings());
        if let Err(e) = msg.channel_id.send_message(&ctx.http, message).await {
            warn!("failed to send reply: {e}");
        }
    }

    async fn run_command(&self, ctx: &Context, command: &CommandInteraction) {
        if let Err(e) = command.defer_ephemeral(&ctx.http).await {
            warn!("failed to acknowledge /{}: {e}", command.data.name);
            return;
        }

        // Taking the lock waits for any reply in progress, so a reset can't
        // be undone by a reply that finishes afterwards.
        let conversation = self.store.conversation(command.channel_id.get()).lock_owned().await;
        let engine = self.store.engine();
        let name = command.data.name.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut conversation = conversation;
            match name.as_str() {
                "reset" => match conversation.reset() {
                    Ok(()) => "Done. I've forgotten everything in this channel.".to_string(),
                    Err(e) => {
                        warn!("reset failed: {e}");
                        "I couldn't clear my memory here. Check the logs.".to_string()
                    }
                },
                "memory" => match conversation.status(&engine) {
                    Ok(status) => {
                        let percent = status.estimated_tokens * 100 / status.available_tokens.max(1);
                        format!(
                            "I'm using about {percent}% of my memory in this channel \
                             (~{} of {} tokens). Older messages have been summarized {} time(s).",
                            status.estimated_tokens, status.available_tokens, status.compactions
                        )
                    }
                    Err(e) => user_facing_error(&e),
                },
                other => format!("Unknown command /{other}."),
            }
        })
        .await
        .unwrap_or_else(|e| {
            error!("command task failed: {e}");
            "Something went wrong on my end.".into()
        });

        let response = EditInteractionResponse::new().content(result);
        if let Err(e) = command.edit_response(&ctx.http, response).await {
            warn!("failed to answer /{}: {e}", command.data.name);
        }
    }
}

/// Lets replies reference the user's message without pinging anyone, so
/// model output can never trigger @everyone or role pings.
fn no_pings() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let me = ctx.cache.current_user().id;
        // Members often pick the bot's auto-created role from autocomplete
        // instead of the bot itself, so treat that as a mention too.
        let mut my_roles = Vec::new();
        if let Some(guild_id) = msg.guild_id
            && !msg.mention_roles.is_empty()
            && let Ok(member) = guild_id.member(&ctx.http, me).await
        {
            my_roles = member.roles.iter().map(|r| r.get()).collect();
        }
        let role_mentioned = msg.mention_roles.iter().any(|r| my_roles.contains(&r.get()));
        let in_guild = msg.guild_id.is_some();
        let addressed = !in_guild || msg.mentions_user_id(me) || role_mentioned;
        debug!(author = %msg.author.name, channel = %msg.channel_id, addressed, "message");
        if !addressed {
            return;
        }

        let text = clean_prompt(&msg.content, me.get(), &my_roles);
        if text.is_empty() {
            return;
        }
        let prompt = build_prompt(msg.author.display_name(), &text, in_guild);
        self.reply(&ctx, &msg, prompt).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            self.run_command(&ctx, &command).await;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected", ready.user.name);
        let commands = vec![
            CreateCommand::new("reset").description("Make Bean forget this channel's conversation"),
            CreateCommand::new("memory").description("Show how much of Bean's memory this channel uses"),
        ];
        if let Err(e) = Command::set_global_commands(&ctx.http, commands).await {
            error!("failed to register slash commands: {e}");
        }
    }
}
