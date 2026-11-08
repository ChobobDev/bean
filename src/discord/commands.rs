//! Slash commands. To add one, add a [`SlashCommand`] variant.

use serenity::all::{Command, CommandInteraction, Context, CreateCommand, EditInteractionResponse};
use tracing::{error, warn};

use super::Handler;
use super::errors::user_facing_error;
use crate::ai::{Conversation, Engine, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommand {
    Reset,
    Memory,
}

impl SlashCommand {
    const ALL: [Self; 2] = [Self::Reset, Self::Memory];

    fn name(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Memory => "memory",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Reset => "Make Bean forget this channel's conversation",
            Self::Memory => "Show how much of Bean's memory this channel uses",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.name() == name)
    }

    /// Runs the command on the channel's conversation and returns the
    /// reply. Blocks; call it from a blocking thread.
    fn run(self, conversation: &mut Conversation, engine: &Engine) -> String {
        match self {
            Self::Reset => match conversation.reset() {
                Ok(()) => "Done. I've forgotten everything in this channel.".into(),
                Err(e) => {
                    warn!("reset failed: {e}");
                    "I couldn't clear my memory here. Check the logs.".into()
                }
            },
            Self::Memory => match conversation.status(engine) {
                Ok(status) => memory_report(status),
                Err(e) => user_facing_error(&e),
            },
        }
    }
}

fn memory_report(status: Status) -> String {
    let percent = status.estimated_tokens * 100 / status.available_tokens.max(1);
    format!(
        "I'm using about {percent}% of my memory in this channel (~{} of {} tokens). \
         Older messages have been summarized {} time(s).",
        status.estimated_tokens, status.available_tokens, status.compactions
    )
}

/// Registers every [`SlashCommand`] with Discord.
pub(super) async fn register(ctx: &Context) {
    let commands = SlashCommand::ALL
        .into_iter()
        .map(|command| CreateCommand::new(command.name()).description(command.description()))
        .collect();
    if let Err(e) = Command::set_global_commands(&ctx.http, commands).await {
        error!("failed to register slash commands: {e}");
    }
}

impl Handler {
    pub(super) async fn on_command(&self, ctx: &Context, interaction: &CommandInteraction) {
        let name = &interaction.data.name;
        let Some(command) = SlashCommand::from_name(name) else {
            warn!("unknown command /{name}");
            return;
        };
        if let Err(e) = interaction.defer_ephemeral(&ctx.http).await {
            warn!("failed to acknowledge /{name}: {e}");
            return;
        }

        // Taking the lock waits for any reply in progress, so a reset can't
        // be undone by a reply that finishes afterwards.
        let conversation = self
            .conversation(ctx, interaction.channel_id, interaction.guild_id)
            .lock_owned()
            .await;
        let engine = self.store.engine();
        let reply = tokio::task::spawn_blocking(move || {
            let mut conversation = conversation;
            command.run(&mut conversation, &engine)
        })
        .await
        .unwrap_or_else(|e| {
            error!("/{name} failed: {e}");
            "Something went wrong on my end.".into()
        });

        let response = EditInteractionResponse::new().content(reply);
        if let Err(e) = interaction.edit_response(&ctx.http, response).await {
            warn!("failed to answer /{name}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for command in SlashCommand::ALL {
            assert_eq!(SlashCommand::from_name(command.name()), Some(command));
        }
        assert_eq!(SlashCommand::from_name("nope"), None);
    }

    #[test]
    fn reports_memory_use_as_a_percentage() {
        let status = Status {
            estimated_tokens: 1800,
            available_tokens: 7200,
            compactions: 2,
        };
        let report = memory_report(status);
        assert!(report.starts_with("I'm using about 25%"), "{report}");
        assert!(report.contains("summarized 2 time(s)"), "{report}");
    }

    #[test]
    fn empty_budget_does_not_divide_by_zero() {
        let status = Status {
            estimated_tokens: 5,
            available_tokens: 0,
            compactions: 0,
        };
        assert!(memory_report(status).contains("500%"));
    }
}
