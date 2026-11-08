//! Answering chat messages.

use std::sync::Arc;
use std::time::Duration;

use serenity::all::{Context, Message, Typing};
use tokio::sync::{OwnedMutexGuard, mpsc};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, warn};

use super::Handler;
use super::errors::user_facing_error;
use super::mentions::{build_prompt, clean_prompt};
use super::reply::StreamingReply;
use crate::ai::{Conversation, Engine, Outcome};

/// How often a streaming reply is edited. Discord allows roughly five
/// edits per five seconds per channel.
const EDIT_INTERVAL: Duration = Duration::from_millis(1000);

type Generation = JoinHandle<fm_rs::Result<(String, Outcome)>>;

impl Handler {
    pub(super) async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        if let Some(prompt) = self.prompt_for(ctx, msg).await {
            self.answer(ctx, msg, prompt).await;
        }
    }

    /// The prompt for `msg`, or `None` if it isn't meant for Bean. Bean
    /// answers every DM, and messages that mention it in servers.
    async fn prompt_for(&self, ctx: &Context, msg: &Message) -> Option<String> {
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
        let role_mentioned = msg
            .mention_roles
            .iter()
            .any(|r| my_roles.contains(&r.get()));
        let in_guild = msg.guild_id.is_some();
        let addressed = !in_guild || msg.mentions_user_id(me) || role_mentioned;
        debug!(author = %msg.author.name, channel = %msg.channel_id, addressed, "message");
        if !addressed {
            return None;
        }

        let text = clean_prompt(&msg.content, me.get(), &my_roles);
        (!text.is_empty()).then(|| build_prompt(msg.author.display_name(), &text, in_guild))
    }

    /// Answers `prompt` in `msg`'s channel, using that channel's memory.
    /// The reply is posted as soon as the model starts writing and edited
    /// as more text arrives.
    async fn answer(&self, ctx: &Context, msg: &Message, prompt: String) {
        let mut typing = Some(msg.channel_id.start_typing(&ctx.http));

        // Waits here if this channel is already being answered.
        let conversation = self
            .conversation(ctx, msg.channel_id, msg.guild_id)
            .lock_owned()
            .await;
        let (generation, mut chunks) = generate(
            conversation,
            self.services.store.engine(),
            msg.author.id.get(),
            prompt,
        );

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
                for note in notes(&outcome) {
                    reply.push_note(&note);
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
}

/// Starts answering `prompt` on a blocking thread. Returns the task and a
/// stream of reply chunks, which ends when the task does.
fn generate(
    conversation: OwnedMutexGuard<Conversation>,
    engine: Arc<Engine>,
    author_id: u64,
    prompt: String,
) -> (Generation, mpsc::UnboundedReceiver<String>) {
    let (chunks_tx, chunks) = mpsc::unbounded_channel();
    let generation = tokio::task::spawn_blocking(move || {
        let mut conversation = conversation;
        conversation.respond(&engine, Some(author_id), &prompt, move |chunk| {
            // The receiver only goes away if the handler task died.
            let _ = chunks_tx.send(chunk.to_owned());
        })
    });
    (generation, chunks)
}

/// What goes under a reply: links the tools attached, then small print
/// about which tools ran and what happened to memory.
fn notes(outcome: &Outcome) -> Vec<String> {
    let mut notes = outcome.attachments.clone();
    if !outcome.tools_used.is_empty() {
        notes.push(format!("-# 🔧 {}", outcome.tools_used.join(" · ")));
    }
    if outcome.memory_lost {
        notes.push("-# I ran out of memory and had to start this conversation over.".into());
    } else if outcome.compacted {
        notes.push("-# Older messages were summarized to make room.".into());
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_replies_have_no_notes() {
        assert!(notes(&Outcome::default()).is_empty());
    }

    #[test]
    fn lists_tools_in_order() {
        let outcome = Outcome {
            tools_used: vec![
                "Checked the time".into(),
                "Checked the weather in Seoul".into(),
            ],
            ..Outcome::default()
        };
        assert_eq!(
            notes(&outcome),
            ["-# 🔧 Checked the time · Checked the weather in Seoul"]
        );
    }

    #[test]
    fn attached_links_come_before_the_small_print() {
        let outcome = Outcome {
            tools_used: vec!["Found a jazz song on Spotify".into()],
            attachments: vec!["https://open.spotify.com/track/abc".into()],
            ..Outcome::default()
        };
        assert_eq!(
            notes(&outcome),
            [
                "https://open.spotify.com/track/abc",
                "-# 🔧 Found a jazz song on Spotify"
            ]
        );
    }

    #[test]
    fn losing_memory_outranks_summarizing() {
        let outcome = Outcome {
            compacted: true,
            memory_lost: true,
            ..Outcome::default()
        };
        let notes = notes(&outcome);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("start this conversation over"));
    }
}
