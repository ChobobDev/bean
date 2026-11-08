use std::sync::Arc;

use bean::conversation::{ConversationStore, Engine};
use bean::handler::Handler;
use serenity::Client;
use serenity::all::GatewayIntents;
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("bean=info,warn")),
        )
        .init();

    let token = std::env::var("DISCORD_TOKEN").expect("set DISCORD_TOKEN env var");
    let data_dir = std::env::var("BEAN_DATA_DIR").unwrap_or_else(|_| "data".into());

    let engine = Engine::new().expect("failed to load the Foundation Models framework");
    if let Err(e) = engine.ensure_available() {
        warn!("model unavailable, replies will report errors: {e}");
    }
    let store = ConversationStore::open(engine, format!("{data_dir}/conversations"))
        .expect("failed to create data directory");

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(&token, intents)
        .event_handler(Handler::new(Arc::new(store)))
        .await
        .expect("failed to create client");

    if let Err(e) = client.start().await {
        error!("client error: {e}");
    }
}
