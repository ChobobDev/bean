//! Tools the model can call while answering.
//!
//! Foundation Models invokes tools synchronously on a dispatch thread
//! outside the tokio runtime, so tools bridge into async code (Discord and
//! HTTP calls) with [`Handle::block_on`], bounded by a timeout.
//!
//! A tool returning `Err` aborts the whole reply, so tools report problems
//! the model can explain ("couldn't find that city") as ordinary output.
//!
//! Schemas deliberately don't mark arguments as `required`. The bridge
//! rejects calls missing a required argument before they reach Rust, and the
//! on-device model often mislabels arguments, so it would retry the same
//! bad call over and over. Tools validate arguments themselves instead and
//! fall back to the only plausible value (see [`arg_str`]).

mod discord;
mod reminder;
mod time;
mod weather;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fm_rs::{Tool, ToolOutput};
use serde_json::Value;
use serenity::all::{ChannelId, GuildId, Http};
use tokio::runtime::Handle;

use crate::reminders::Reminders;

/// Longest a single tool call may wait on the network.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Dependencies shared by every channel's tools.
pub struct Toolkit {
    runtime: Handle,
    web: reqwest::Client,
    reminders: Arc<Reminders>,
}

impl Toolkit {
    pub fn new(runtime: Handle, reminders: Arc<Reminders>) -> Self {
        let web = reqwest::Client::builder()
            .timeout(TOOL_TIMEOUT)
            .user_agent(concat!("bean-discord-bot/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client config is valid");
        Self { runtime, web, reminders }
    }

    /// Builds the tools for one channel. Without a Discord channel (e.g. in
    /// tests) only the tools that don't need Discord are included.
    pub fn channel_tools(&self, discord: Option<DiscordChannel>) -> ChannelTools {
        let turn = Arc::new(TurnState::default());
        let shared = Shared {
            runtime: self.runtime.clone(),
            turn: Arc::clone(&turn),
        };

        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(time::CurrentTime::new(shared.clone())),
            Arc::new(weather::Weather::new(shared.clone(), self.web.clone())),
        ];
        if let Some(discord) = discord {
            let discord = Arc::new(discord);
            tools.push(Arc::new(discord::RecentMessages::new(shared.clone(), Arc::clone(&discord))));
            tools.push(Arc::new(discord::ServerInfo::new(shared.clone(), Arc::clone(&discord))));
            let reminders = Arc::clone(&self.reminders);
            tools.push(Arc::new(reminder::SetReminder::new(
                shared.clone(),
                discord.channel_id,
                Arc::clone(&reminders),
            )));
            tools.push(Arc::new(reminder::ListReminders::new(shared, reminders)));
        }
        ChannelTools { tools, turn }
    }
}

/// The Discord channel a set of tools acts on.
pub struct DiscordChannel {
    pub http: Arc<Http>,
    pub channel_id: ChannelId,
    pub guild_id: Option<GuildId>,
}

/// One channel's tools, plus what they did during the current turn.
pub struct ChannelTools {
    tools: Vec<Arc<dyn Tool>>,
    turn: Arc<TurnState>,
}

impl ChannelTools {
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Records who is speaking, so tools like reminders act for them.
    pub fn begin_turn(&self, author_id: Option<u64>) {
        let mut turn = self.turn.0.lock().expect("turn lock");
        turn.author_id = author_id;
        turn.activity.clear();
    }

    /// Returns a short description of each tool call made this turn.
    pub fn finish_turn(&self) -> Vec<String> {
        let mut turn = self.turn.0.lock().expect("turn lock");
        turn.author_id = None;
        std::mem::take(&mut turn.activity)
    }
}

#[derive(Default)]
struct TurnState(Mutex<Turn>);

#[derive(Default)]
struct Turn {
    author_id: Option<u64>,
    activity: Vec<String>,
}

/// What every tool gets: a way into async code and the current turn.
#[derive(Clone)]
struct Shared {
    runtime: Handle,
    turn: Arc<TurnState>,
}

impl Shared {
    fn author_id(&self) -> Option<u64> {
        self.turn.0.lock().expect("turn lock").author_id
    }

    /// Notes a tool call so it can be shown under the reply.
    fn record(&self, activity: impl Into<String>) {
        self.turn.0.lock().expect("turn lock").activity.push(activity.into());
    }

    /// Runs `future` to completion from a tool's (non-async) thread.
    fn block_on<F: Future>(&self, future: F) -> Result<F::Output, String> {
        self.runtime
            .block_on(async { tokio::time::timeout(TOOL_TIMEOUT, future).await })
            .map_err(|_| "the request timed out".to_string())
    }
}

fn output(text: impl Into<String>) -> fm_rs::Result<ToolOutput> {
    Ok(ToolOutput::new(text))
}

fn failure(message: impl std::fmt::Display) -> fm_rs::Result<ToolOutput> {
    output(format!("Error: {message}"))
}

/// Reads a text argument.
///
/// The on-device model often mislabels arguments (it has been seen passing
/// the tool's own name as the argument name), so when `name` is missing
/// and the call has exactly one text value, that value is used instead.
fn arg_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    let text = |v: &'a Value| v.as_str().map(str::trim).filter(|s| !s.is_empty());
    args.get(name).and_then(text).or_else(|| only(args, text))
}

/// Reads a whole-number argument, falling back like [`arg_str`]. The model
/// sometimes sends numbers as strings or floats, so all three are accepted.
fn arg_u64(args: &Value, name: &str) -> Option<u64> {
    args.get(name).and_then(as_u64).or_else(|| only(args, as_u64))
}

fn as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().filter(|f| *f >= 0.0).map(|f| f.round() as u64))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

/// The single argument value `parse` accepts, if there is exactly one.
fn only<'a, T>(args: &'a Value, parse: impl Fn(&'a Value) -> Option<T>) -> Option<T> {
    let mut matches = args.as_object()?.values().filter_map(parse);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_numbers_in_any_shape() {
        for value in [json!(5), json!(5.4), json!(" 5 ")] {
            assert_eq!(arg_u64(&json!({ "n": value }), "n"), Some(5), "{value}");
        }
        assert_eq!(arg_u64(&json!({ "n": -3, "m": "nope" }), "n"), None);
    }

    #[test]
    fn blank_strings_count_as_missing() {
        let args = json!({ "a": "  hi ", "b": "   " });
        assert_eq!(arg_str(&args, "a"), Some("hi"));
        assert_eq!(arg_str(&json!({ "b": "   " }), "b"), None);
    }

    #[test]
    fn falls_back_to_the_only_value_of_the_right_type() {
        // Seen from the real model: the tool name used as the argument name.
        let args = json!({ "getWeather": "Seoul" });
        assert_eq!(arg_str(&args, "city"), Some("Seoul"));

        let args = json!({ "delay": "30", "text": "stretch" });
        assert_eq!(arg_u64(&args, "minutes"), Some(30));
    }

    #[test]
    fn does_not_guess_between_several_values() {
        let args = json!({ "x": "Seoul", "y": "Paris" });
        assert_eq!(arg_str(&args, "city"), None);
        let args = json!({ "x": 1, "y": 2 });
        assert_eq!(arg_u64(&args, "minutes"), None);
    }
}
