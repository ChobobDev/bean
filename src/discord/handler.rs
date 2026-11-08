//! Routes Discord events to the code that handles them.

use std::sync::Arc;

use serenity::all::{ChannelId, Context, EventHandler, GuildId, Interaction, Message, Ready};
use serenity::async_trait;
use tracing::info;

use super::commands;
use crate::ai::{Conversation, ConversationStore};
use crate::reminders::Reminders;
use crate::tools::DiscordChannel;

pub struct Handler {
    pub(super) store: Arc<ConversationStore>,
    pub(super) reminders: Arc<Reminders>,
}

impl Handler {
    pub fn new(store: Arc<ConversationStore>, reminders: Arc<Reminders>) -> Self {
        Self { store, reminders }
    }

    /// The conversation for a channel, with tools bound to that channel.
    pub(super) fn conversation(
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
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        self.on_message(&ctx, &msg).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            self.on_command(&ctx, &command).await;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected", ready.user.name);
        self.reminders.start(Arc::clone(&ctx.http));
        commands::register(&ctx).await;
    }
}
