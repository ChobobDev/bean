//! Tools the model can call while answering.
//!
//! Each tool is a [`BeanTool`] in its own file, listed in `TOOLS` below. To
//! add one, implement the trait and add it to the list; schemas, argument
//! recovery, logging, and error handling are shared.

mod args;
mod context;
mod currency;
mod geo;
mod messages;
mod reminder;
mod server;
mod song;
mod time;
mod tool;
mod weather;

use std::sync::Arc;

use tokio::runtime::Handle;

pub use args::{Args, parse_duration};
pub use context::{DiscordChannel, ToolContext, TurnReport};
pub use tool::{BeanTool, Param, ParamKind, ToolError, ToolResult};

use crate::reminders::Reminders;
use crate::spotify::Spotify;

/// Every tool Bean has. Each channel gets the ones available to it.
static TOOLS: &[&dyn BeanTool] = &[
    &time::CurrentTime,
    &weather::Weather,
    &currency::ConvertCurrency,
    &song::RecommendSong,
    &messages::RecentMessages,
    &server::ServerInfo,
    &reminder::SetReminder,
    &reminder::ListReminders,
];

/// The HTTP client for outside services, shared so connections are reused.
pub fn web_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(context::TOOL_TIMEOUT)
        .user_agent(concat!("bean-discord-bot/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client config is valid")
}

/// Services shared by every channel's tools.
pub struct Toolkit {
    runtime: Handle,
    web: reqwest::Client,
    reminders: Arc<Reminders>,
    spotify: Option<Arc<Spotify>>,
}

impl Toolkit {
    pub fn new(
        runtime: Handle,
        web: reqwest::Client,
        reminders: Arc<Reminders>,
        spotify: Option<Arc<Spotify>>,
    ) -> Self {
        Self {
            runtime,
            web,
            reminders,
            spotify,
        }
    }

    /// Builds the tools for one channel. Without a Discord channel (e.g. in
    /// tests) only the tools that don't need Discord are included.
    pub fn channel_tools(&self, discord: Option<DiscordChannel>) -> ChannelTools {
        let ctx = Arc::new(ToolContext::new(
            self.runtime.clone(),
            self.web.clone(),
            Arc::clone(&self.reminders),
            self.spotify.clone(),
            discord,
        ));
        let tools = TOOLS
            .iter()
            .filter(|tool| tool.available(&ctx))
            .map(|&tool| {
                let ctx = Arc::clone(&ctx);
                Arc::new(tool::Adapter { tool, ctx }) as Arc<dyn fm_rs::Tool>
            })
            .collect();
        ChannelTools { ctx, tools }
    }
}

/// One channel's tools, plus what they did during the current turn.
pub struct ChannelTools {
    ctx: Arc<ToolContext>,
    tools: Vec<Arc<dyn fm_rs::Tool>>,
}

impl ChannelTools {
    pub fn tools(&self) -> &[Arc<dyn fm_rs::Tool>] {
        &self.tools
    }

    /// Records who is speaking, so tools like reminders act for them.
    pub fn begin_turn(&self, author_id: Option<u64>) {
        self.ctx.begin_turn(author_id);
    }

    /// Reports what the tools did this turn.
    pub fn finish_turn(&self) -> TurnReport {
        self.ctx.finish_turn()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn tool_names_are_unique() {
        let names: HashSet<_> = TOOLS.iter().map(|t| t.name()).collect();
        assert_eq!(names.len(), TOOLS.len());
    }

    #[test]
    fn offline_channels_only_get_tools_that_need_no_discord() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("bean-toolkit-{}", std::process::id()));
        let reminders = Reminders::open(dir.join("r.json"), runtime.handle().clone()).unwrap();
        let toolkit = Toolkit::new(runtime.handle().clone(), web_client(), reminders, None);

        let tools = toolkit.channel_tools(None);
        let names: Vec<_> = tools.tools().iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, ["getCurrentTime", "getWeather", "convertCurrency"]);
    }
}
