use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{Conversation, Engine};
use crate::tools::{DiscordChannel, Toolkit};

/// Hands out one [`Conversation`] per channel, creating them on demand.
pub struct ConversationStore {
    engine: Arc<Engine>,
    toolkit: Toolkit,
    dir: PathBuf,
    conversations: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<Conversation>>>>,
}

impl ConversationStore {
    /// Opens a store that saves conversations in `dir`.
    pub fn open(engine: Engine, toolkit: Toolkit, dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            engine: Arc::new(engine),
            toolkit,
            dir,
            conversations: Mutex::new(HashMap::new()),
        })
    }

    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }

    /// Returns the conversation for `channel_id`. Messages in the same
    /// channel queue up on its lock and are answered in order.
    ///
    /// `discord` gives the conversation's tools access to the channel. It's
    /// only used the first time a channel is seen.
    pub fn conversation(
        &self,
        channel_id: u64,
        discord: Option<DiscordChannel>,
    ) -> Arc<tokio::sync::Mutex<Conversation>> {
        let mut conversations = self.conversations.lock().expect("store lock poisoned");
        let conversation = conversations.entry(channel_id).or_insert_with(|| {
            let path = self.dir.join(format!("{channel_id}.json"));
            let tools = self.toolkit.channel_tools(discord);
            Arc::new(tokio::sync::Mutex::new(Conversation::new(path, tools)))
        });
        Arc::clone(conversation)
    }
}
