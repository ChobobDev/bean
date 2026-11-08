use fm_rs::{GenerationOptions, Session, SystemLanguageModel};
use serenity::all::{Context, EventHandler, GatewayIntents, Message, Ready};
use serenity::{Client, async_trait};

/// Runs a single prompt through Apple's on-device Foundation Model.
fn ask_model(prompt: &str) -> Result<String, fm_rs::Error> {
    let model = SystemLanguageModel::new()?;
    model.ensure_available()?;
    let session = Session::new(&model)?;
    let options = GenerationOptions::builder()
        .temperature(0.7)
        .max_response_tokens(500)
        .build();
    Ok(session.respond(prompt, &options)?.into_content())
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        // Respond when mentioned or in DMs.
        let me = ctx.cache.current_user().id;
        // Also count mentions of the bot's own role (e.g. the auto-created "Bean" role).
        let mut my_roles = Vec::new();
        if let Some(guild_id) = msg.guild_id {
            if !msg.mention_roles.is_empty() {
                if let Ok(member) = guild_id.member(&ctx.http, me).await {
                    my_roles = member.roles;
                }
            }
        }
        let role_mentioned = msg.mention_roles.iter().any(|r| my_roles.contains(r));
        let mentioned = msg.mentions_user_id(me) || role_mentioned;
        println!(
            "msg from {} in {:?}: {:?} (mentioned: {mentioned})",
            msg.author.name, msg.guild_id, msg.content
        );
        if !mentioned && msg.guild_id.is_some() {
            return;
        }

        let mut prompt = msg
            .content
            .replace(&format!("<@{me}>"), "")
            .replace(&format!("<@!{me}>"), "");
        for role in &my_roles {
            prompt = prompt.replace(&format!("<@&{role}>"), "");
        }
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }

        let typing = msg.channel_id.start_typing(&ctx.http);
        let result = tokio::task::spawn_blocking(move || ask_model(&prompt)).await;
        typing.stop();

        let reply = match result {
            Ok(Ok(text)) => text,
            Ok(Err(e)) => format!("Model error: {e}"),
            Err(e) => format!("Internal error: {e}"),
        };
        let reply: String = reply.chars().take(2000).collect();

        if let Err(e) = msg.reply(&ctx.http, reply).await {
            eprintln!("Failed to send reply: {e}");
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[tokio::main]
async fn main() {
    let token = std::env::var("DISCORD_TOKEN").expect("set DISCORD_TOKEN env var");
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .await
        .expect("failed to create client");

    if let Err(e) = client.start().await {
        eprintln!("Client error: {e}");
    }
}
