//! Routes Discord events to the code that handles them.

use std::sync::Arc;

use serenity::all::{ChannelId, Context, EventHandler, GuildId, Interaction, Message, Ready};
use serenity::async_trait;
use tracing::info;

use super::commands;
use crate::ai::{Conversation, ConversationStore};
use crate::daily::Daily;
use crate::reminders::Reminders;
use crate::tools::DiscordChannel;

/// Everything the Discord side uses.
pub struct Services {
    pub store: Arc<ConversationStore>,
    pub reminders: Arc<Reminders>,
    pub daily: Arc<Daily>,
}

pub struct Handler {
    pub(super) services: Services,
}

impl Handler {
    pub fn new(services: Services) -> Self {
        Self { services }
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
        self.services
            .store
            .conversation(channel_id.get(), Some(discord))
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
        self.services.reminders.start(Arc::clone(&ctx.http));
        self.services.daily.start(Arc::clone(&ctx.http));
        commands::register(&ctx).await;
    }
}
