use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serenity::all::{ChannelId, GuildId, Http};
use tokio::runtime::Handle;

use super::ToolError;
use crate::reminders::Reminders;

/// Longest a single tool call may wait on the network.
pub(super) const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// The Discord channel a set of tools acts on.
pub struct DiscordChannel {
    pub http: Arc<Http>,
    pub channel_id: ChannelId,
    pub guild_id: Option<GuildId>,
}

/// Everything a tool can use: shared services, the channel it's running
/// in, and who is speaking this turn. There's one per channel.
pub struct ToolContext {
    runtime: Handle,
    web: reqwest::Client,
    reminders: Arc<Reminders>,
    discord: Option<DiscordChannel>,
    turn: Mutex<Turn>,
}

#[derive(Default)]
struct Turn {
    author_id: Option<u64>,
    activity: Vec<String>,
}

impl ToolContext {
    pub(super) fn new(
        runtime: Handle,
        web: reqwest::Client,
        reminders: Arc<Reminders>,
        discord: Option<DiscordChannel>,
    ) -> Self {
        Self {
            runtime,
            web,
            reminders,
            discord,
            turn: Mutex::default(),
        }
    }

    pub fn web(&self) -> &reqwest::Client {
        &self.web
    }

    pub fn reminders(&self) -> &Arc<Reminders> {
        &self.reminders
    }

    pub fn discord(&self) -> Option<&DiscordChannel> {
        self.discord.as_ref()
    }

    /// The Discord channel, for tools that are only registered with one.
    pub fn require_discord(&self) -> Result<&DiscordChannel, ToolError> {
        self.discord()
            .ok_or_else(|| ToolError::new("this only works in Discord"))
    }

    /// Who sent the message being answered.
    pub fn author_id(&self) -> Result<u64, ToolError> {
        self.lock_turn()
            .author_id
            .ok_or_else(|| ToolError::new("I don't know who is asking"))
    }

    /// Notes what a tool did, to show under the reply.
    pub fn record(&self, activity: impl Into<String>) {
        self.lock_turn().activity.push(activity.into());
    }

    /// Runs `future` to completion from a tool's thread.
    ///
    /// Foundation Models calls tools synchronously on a dispatch thread
    /// outside the tokio runtime, so async work (Discord and HTTP calls) is
    /// driven from here, with a timeout.
    pub fn block_on<F: Future>(&self, future: F) -> Result<F::Output, ToolError> {
        self.runtime
            .block_on(async { tokio::time::timeout(TOOL_TIMEOUT, future).await })
            .map_err(|_| ToolError::new("the request timed out"))
    }

    pub(super) fn begin_turn(&self, author_id: Option<u64>) {
        let mut turn = self.lock_turn();
        turn.author_id = author_id;
        turn.activity.clear();
    }

    pub(super) fn finish_turn(&self) -> Vec<String> {
        let mut turn = self.lock_turn();
        turn.author_id = None;
        std::mem::take(&mut turn.activity)
    }

    fn lock_turn(&self) -> std::sync::MutexGuard<'_, Turn> {
        self.turn.lock().expect("turn lock poisoned")
    }
}
