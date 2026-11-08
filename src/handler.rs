//! Discord event handling: chat messages and slash commands.

use std::sync::Arc;
use std::time::Duration;

use serenity::all::{
    ChannelId, Command, CommandInteraction, Context, CreateCommand, EditInteractionResponse,
    EventHandler, GuildId, Interaction, Message, Ready, Typing,
};
use serenity::async_trait;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::conversation::{Conversation, ConversationStore};
use crate::prompt::{build_prompt, clean_prompt, user_facing_error};
use crate::reminders::Reminders;
use crate::reply::StreamingReply;
use crate::tools::DiscordChannel;

/// How often a streaming reply is edited. Discord allows roughly five
/// edits per five seconds per channel.
const EDIT_INTERVAL: Duration = Duration::from_millis(1000);

pub struct Handler {
    store: Arc<ConversationStore>,
    reminders: Arc<Reminders>,
}

impl Handler {
    pub fn new(store: Arc<ConversationStore>, reminders: Arc<Reminders>) -> Self {
        Self { store, reminders }
    }

    fn conversation(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        guild_id: Option<GuildId>,
    ) -> Arc<tokio::sync::Mutex<Conversation>> {
        let discord = DiscordChannel {
            http: Arc::clone(&ctx.http),
            channel_id,
            guild_id,
        };
        self.store.conversation(channel_id.get(), Some(discord))
    }

    /// Answers `prompt` in `msg`'s channel, using that channel's memory.
    /// The reply is posted as soon as the model starts writing and edited
    /// as more text arrives.
    async fn reply(&self, ctx: &Context, msg: &Message, prompt: String) {
        let mut typing = Some(msg.channel_id.start_typing(&ctx.http));

        // Waits here if this channel is already being answered.
        let conversation = self
            .conversation(ctx, msg.channel_id, msg.guild_id)
            .lock_owned()
            .await;
        let engine = self.store.engine();
        let author_id = msg.author.id.get();
        let (chunks_tx, mut chunks) = mpsc::unbounded_channel::<String>();
        let generation = tokio::task::spawn_blocking(move || {
            let mut conversation = conversation;
            conversation.respond(&engine, Some(author_id), &prompt, move |chunk| {
                // The receiver only goes away if the handler task died.
                let _ = chunks_tx.send(chunk.to_owned());
            })
        });

        let mut reply = StreamingReply::new(Arc::clone(&ctx.http), msg);
        let mut tick = tokio::time::interval(EDIT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                chunk = chunks.recv() => match chunk {
                    Some(chunk) => reply.push(&chunk),
                    // The sender is dropped when generation ends.
                    None => break,
                },
                _ = tick.tick() => {
                    reply.flush().await;
                    if reply.has_posted() {
                        typing.take().map(Typing::stop);
                    }
                }
            }
        }

        match generation.await {
            Ok(Ok((_, outcome))) => {
                if reply.is_empty() {
                    reply.push_note("(no response)");
                }
                if !outcome.tools_used.is_empty() {
                    reply.push_note(&format!("-# 🔧 {}", outcome.tools_used.join(" · ")));
                }
                if outcome.memory_lost {
                    reply.push_note("-# I ran out of memory and had to start this conversation over.");
                } else if outcome.compacted {
                    reply.push_note("-# Older messages were summarized to make room.");
                }
            }
            Ok(Err(e)) => {
                warn!("model error: {e}");
                reply.push_note(&format!("-# {}", user_facing_error(&e)));
            }
            Err(e) => {
                error!("generation task failed: {e}");
                reply.push_note("-# Something went wrong on my end.");
            }
        }
        reply.flush().await;
        typing.take().map(Typing::stop);
    }

    async fn run_command(&self, ctx: &Context, command: &CommandInteraction) {
        if let Err(e) = command.defer_ephemeral(&ctx.http).await {
            warn!("failed to acknowledge /{}: {e}", command.data.name);
            return;
        }

        // Taking the lock waits for any reply in progress, so a reset can't
        // be undone by a reply that finishes afterwards.
        let conversation = self
            .conversation(ctx, command.channel_id, command.guild_id)
            .lock_owned()
            .await;
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
        self.reminders.start(Arc::clone(&ctx.http));
        let commands = vec![
            CreateCommand::new("reset").description("Make Bean forget this channel's conversation"),
            CreateCommand::new("memory").description("Show how much of Bean's memory this channel uses"),
        ];
        if let Err(e) = Command::set_global_commands(&ctx.http, commands).await {
            error!("failed to register slash commands: {e}");
        }
    }
}
