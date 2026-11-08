//! Slash commands. To add one, add a [`SlashCommand`] variant.

mod daily;
mod memory;

use serenity::all::{Command, CommandInteraction, Context, CreateCommand, EditInteractionResponse};
use tracing::{error, warn};

use super::Handler;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommand {
    Reset,
    Memory,
    Daily,
}

impl SlashCommand {
    const ALL: [Self; 3] = [Self::Reset, Self::Memory, Self::Daily];

    fn name(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Memory => "memory",
            Self::Daily => "daily",
        }
    }

    fn definition(self) -> CreateCommand {
        match self {
            Self::Reset => CreateCommand::new(self.name())
                .description("Make Bean forget this channel's conversation"),
            Self::Memory => CreateCommand::new(self.name())
                .description("Show how much of Bean's memory this channel uses"),
            Self::Daily => daily::definition(self.name()),
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.name() == name)
    }
}

/// Registers every [`SlashCommand`] with Discord.
pub(super) async fn register(ctx: &Context) {
    let commands = SlashCommand::ALL
        .into_iter()
        .map(SlashCommand::definition)
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
        // Replies are only visible to the person who ran the command.
        if let Err(e) = interaction.defer_ephemeral(&ctx.http).await {
            warn!("failed to acknowledge /{name}: {e}");
            return;
        }

        let reply = match command {
            SlashCommand::Reset | SlashCommand::Memory => {
                self.run_memory_command(ctx, interaction, command).await
            }
            SlashCommand::Daily => self.run_daily_command(ctx, interaction).await,
        };

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
}
