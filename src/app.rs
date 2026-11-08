//! Wires the pieces together and runs the bot.

use std::sync::Arc;

use anyhow::{Context, Result};
use serenity::Client;
use serenity::all::GatewayIntents;
use tracing::{info, warn};

use crate::ai::{ConversationStore, Engine};
use crate::config::Config;
use crate::daily::Daily;
use crate::discord::{Handler, Services};
use crate::reminders::Reminders;
use crate::spotify::Spotify;
use crate::tools::{Toolkit, web_client};

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
    let web = web_client();
    let spotify = config
        .spotify
        .clone()
        .map(|credentials| Arc::new(Spotify::new(credentials, web.clone())));
    match &spotify {
        Some(_) => info!("Spotify is set up; song recommendations are on"),
        None => info!("Spotify isn't set up; song recommendations are off"),
    }

    let reminders = Reminders::open(config.reminders_path(), runtime.clone())
        .context("failed to load reminders")?;
    let toolkit = Toolkit::new(
        runtime.clone(),
        web.clone(),
        Arc::clone(&reminders),
        spotify.clone(),
    );
    let store = ConversationStore::open(engine, toolkit, config.conversations_dir())
        .context("failed to create the conversations directory")?;
    let daily = Daily::open(config.daily_path(), runtime, web, spotify, store.engine())
        .context("failed to load the daily post schedule")?;

    let services = Services {
        store: Arc::new(store),
        reminders,
        daily,
    };
    let mut client = Client::builder(&config.discord_token, INTENTS)
        .event_handler(Handler::new(services))
        .await
        .context("failed to create the Discord client")?;
    client
        .start()
        .await
        .context("lost the connection to Discord")
}
