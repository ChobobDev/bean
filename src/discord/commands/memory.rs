//! `/reset` and `/memory`: managing a channel's conversation.

use serenity::all::{CommandInteraction, Context};
use tracing::{error, warn};

use super::SlashCommand;
use crate::ai::{Conversation, Engine, Status};
use crate::discord::Handler;
use crate::discord::errors::user_facing_error;

impl Handler {
    pub(super) async fn run_memory_command(
        &self,
        ctx: &Context,
        interaction: &CommandInteraction,
        command: SlashCommand,
    ) -> String {
        // Taking the lock waits for any reply in progress, so a reset can't
        // be undone by a reply that finishes afterwards.
        let conversation = self
            .conversation(ctx, interaction.channel_id, interaction.guild_id)
            .lock_owned()
            .await;
        let engine = self.services.store.engine();
        tokio::task::spawn_blocking(move || {
            let mut conversation = conversation;
            run(command, &mut conversation, &engine)
        })
        .await
        .unwrap_or_else(|e| {
            error!("/{} failed: {e}", command.name());
            "Something went wrong on my end.".into()
        })
    }
}

/// Runs `command` on the channel's conversation. Blocks.
fn run(command: SlashCommand, conversation: &mut Conversation, engine: &Engine) -> String {
    match command {
        SlashCommand::Reset => match conversation.reset() {
            Ok(()) => "Done. I've forgotten everything in this channel.".into(),
            Err(e) => {
                warn!("reset failed: {e}");
                "I couldn't clear my memory here. Check the logs.".into()
            }
        },
        SlashCommand::Memory => match conversation.status(engine) {
            Ok(status) => memory_report(status),
            Err(e) => user_facing_error(&e),
        },
        SlashCommand::Daily => unreachable!("/daily doesn't use the conversation"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
