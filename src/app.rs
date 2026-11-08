//! Wires the pieces together and runs the bot.

use std::sync::Arc;

use anyhow::{Context, Result};
use serenity::Client;
use serenity::all::GatewayIntents;
use tracing::warn;

use crate::ai::{ConversationStore, Engine};
use crate::config::Config;
use crate::discord::Handler;
use crate::reminders::Reminders;
use crate::tools::Toolkit;

/// Message Content is a privileged intent; enable it in the Developer
/// Portal or Discord refuses the connection.
const INTENTS: GatewayIntents = GatewayIntents::GUILD_MESSAGES
    .union(GatewayIntents::DIRECT_MESSAGES)
    .union(GatewayIntents::MESSAGE_CONTENT);

/// Connects to Discord and answers messages until the connection ends.
pub async fn run(config: Config) -> Result<()> {
    let engine = Engine::new().context("failed to load the Foundation Models framework")?;
    if let Err(e) = engine.ensure_available() {
        warn!("model unavailable, replies will report errors: {e}");
    }

    let runtime = tokio::runtime::Handle::current();
    let reminders = Reminders::open(config.reminders_path(), runtime.clone())
        .context("failed to load reminders")?;
    let toolkit = Toolkit::new(runtime, Arc::clone(&reminders));
    let store = ConversationStore::open(engine, toolkit, config.conversations_dir())
        .context("failed to create the conversations directory")?;

    let mut client = Client::builder(&config.discord_token, INTENTS)
        .event_handler(Handler::new(Arc::new(store), reminders))
        .await
        .context("failed to create the Discord client")?;
    client
        .start()
        .await
        .context("lost the connection to Discord")
}
