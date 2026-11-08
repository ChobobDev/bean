//! Per-channel conversation memory backed by Apple's Foundation Models.
//!
//! Each Discord channel gets its own model [`Session`], so the model sees the
//! whole conversation instead of a single message. Sessions are persisted to
//! disk as transcripts after every turn and restored lazily on first use, so
//! memory survives restarts.
//!
//! The on-device model has a small context window (~4k tokens). Before each
//! turn we estimate how full it is; when the next prompt would not fit, the
//! transcript is summarized and the conversation continues from a fresh
//! session seeded with that summary.
//!
//! Every session is created with the channel's tools. Restoring a saved
//! transcript directly would drop them, so a restored conversation is
//! replayed into a new session's instructions instead (see
//! [`crate::transcript`]).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fm_rs::{
    CompactionConfig, ContextLimit, Error as FmError, GenerationOptions, Session,
    SystemLanguageModel, compact_transcript, compacted_instructions, estimate_tokens,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::tools::{ChannelTools, DiscordChannel, Toolkit};
use crate::transcript;

/// Upper bound on the length of a single reply.
pub const MAX_RESPONSE_TOKENS: u32 = 500;

/// Bump when [`SavedConversation`] changes shape.
const FORMAT_VERSION: u32 = 1;

/// Shared model state used by every conversation.
pub struct Engine {
    model: SystemLanguageModel,
    limit: ContextLimit,
    options: GenerationOptions,
    compaction: CompactionConfig,
}

impl Engine {
    /// Creates an engine sized to the model's real context window.
    pub fn new() -> fm_rs::Result<Self> {
        let model = SystemLanguageModel::new()?;
        let limit = ContextLimit::for_model(&model).unwrap_or_else(|e| {
            warn!("could not read model context size ({e}); using default");
            ContextLimit::default_on_device()
        });
        Ok(Self::with_limit(
            model,
            limit.with_reserved_response_tokens(MAX_RESPONSE_TOKENS as usize),
        ))
    }

    /// Creates an engine with an explicit context budget.
    pub fn with_limit(model: SystemLanguageModel, limit: ContextLimit) -> Self {
        Self {
            model,
            limit,
            options: GenerationOptions::builder()
                .temperature(0.7)
                .max_response_tokens(MAX_RESPONSE_TOKENS)
                .build(),
            compaction: CompactionConfig::default(),
        }
    }

    /// Returns an error describing why the model can't be used, if it can't.
    pub fn ensure_available(&self) -> fm_rs::Result<()> {
        self.model.ensure_available()
    }

    pub fn context_limit(&self) -> &ContextLimit {
        &self.limit
    }

    fn new_session(&self, tools: &ChannelTools, instructions: Option<&str>) -> fm_rs::Result<Session> {
        let mut builder = Session::builder(&self.model);
        if let Some(instructions) = instructions {
            builder = builder.instructions(instructions);
        }
        for tool in tools.tools() {
            builder = builder.tool(Arc::clone(tool));
        }
        builder.build()
    }
}

/// What happened while producing a reply, besides the text itself.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// Older history was summarized to make room.
    pub compacted: bool,
    /// Summarizing failed, so the conversation was restarted from scratch.
    pub memory_lost: bool,
    /// A short description of each tool call, in order.
    pub tools_used: Vec<String>,
}

/// Snapshot of how much of the context window a conversation uses.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub estimated_tokens: usize,
    pub available_tokens: usize,
    pub compactions: u32,
}

#[derive(Serialize, Deserialize)]
struct SavedConversation {
    version: u32,
    transcript: String,
    compactions: u32,
}

/// One channel's conversation. Lock it before use; the model session can
/// only serve one request at a time.
pub struct Conversation {
    path: PathBuf,
    session: Option<Session>,
    compactions: u32,
    tools: ChannelTools,
}

impl Conversation {
    fn new(path: PathBuf, tools: ChannelTools) -> Self {
        Self {
            path,
            session: None,
            compactions: 0,
            tools,
        }
    }

    /// Sends `prompt` from `author_id` and streams the reply to `on_chunk`
    /// as it is generated. Returns the full reply. Blocks; call it from a
    /// blocking thread.
    pub fn respond<F>(
        &mut self,
        engine: &Engine,
        author_id: Option<u64>,
        prompt: &str,
        on_chunk: F,
    ) -> fm_rs::Result<(String, Outcome)>
    where
        F: FnMut(&str) + Send,
    {
        self.tools.begin_turn(author_id);
        let result = self.respond_inner(engine, prompt, on_chunk);
        let tools_used = self.tools.finish_turn();
        result.map(|(text, outcome)| (text, Outcome { tools_used, ..outcome }))
    }

    fn respond_inner<F>(
        &mut self,
        engine: &Engine,
        prompt: &str,
        mut on_chunk: F,
    ) -> fm_rs::Result<(String, Outcome)>
    where
        F: FnMut(&str) + Send,
    {
        self.ensure_loaded(engine)?;
        let mut outcome = Outcome::default();

        if self.needs_compaction(engine, prompt)? {
            self.compact(engine, &mut outcome)?;
        }

        let mut text = String::new();
        let mut result = stream(self.session(), prompt, &engine.options, &mut text, &mut on_chunk);
        // Token counts are estimates, so the model may still reject the
        // prompt. If nothing was shown yet, compact once and retry rather
        // than failing the turn.
        if matches!(result, Err(FmError::ContextSizeExceeded(_)))
            && text.is_empty()
            && !(outcome.compacted || outcome.memory_lost)
        {
            self.compact(engine, &mut outcome)?;
            result = stream(self.session(), prompt, &engine.options, &mut text, &mut on_chunk);
        }
        // The model sometimes calls a tool that doesn't exist, which fails
        // the whole turn. Answer without tools instead.
        if let Err(e) = &result
            && text.is_empty()
            && is_tool_bridge_error(e)
        {
            warn!(path = %self.path.display(), "tool call failed, retrying without tools: {e}");
            result = self.answer_without_tools(engine, prompt, &mut text, &mut on_chunk);
        }

        if let Err(e) = self.save() {
            warn!(path = %self.path.display(), "failed to save conversation: {e}");
        }
        result.map(|()| (text, outcome))
    }

    /// Answers `prompt` in a temporary session that has no tools, then
    /// carries the conversation (including this turn) back into a session
    /// that does. `ToolCallingMode::Disallowed` would be simpler, but it
    /// needs the macOS 27 SDK.
    fn answer_without_tools<F>(
        &mut self,
        engine: &Engine,
        prompt: &str,
        text: &mut String,
        on_chunk: &mut F,
    ) -> fm_rs::Result<()>
    where
        F: FnMut(&str) + Send,
    {
        let history = transcript::restored_instructions(&self.session().transcript_json()?);
        let plain = match &history {
            Some(history) => Session::with_instructions(&engine.model, history)?,
            None => Session::new(&engine.model)?,
        };
        let result = stream(&plain, prompt, &engine.options, text, on_chunk);

        let history = transcript::restored_instructions(&plain.transcript_json()?);
        self.session = Some(engine.new_session(&self.tools, history.as_deref())?);
        result
    }

    /// Forgets the conversation, including its saved transcript.
    pub fn reset(&mut self) -> io::Result<()> {
        self.session = None;
        self.compactions = 0;
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Reports context usage, loading the conversation if needed. Blocks.
    pub fn status(&mut self, engine: &Engine) -> fm_rs::Result<Status> {
        self.ensure_loaded(engine)?;
        let usage = self.session().context_usage(&engine.limit)?;
        Ok(Status {
            estimated_tokens: usage.estimated_tokens,
            available_tokens: usage.available_tokens,
            compactions: self.compactions,
        })
    }

    fn session(&self) -> &Session {
        self.session
            .as_ref()
            .expect("session is loaded by ensure_loaded")
    }

    fn ensure_loaded(&mut self, engine: &Engine) -> fm_rs::Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let (instructions, compactions) = match read_saved(&self.path) {
            Some(saved) => (transcript::restored_instructions(&saved.transcript), saved.compactions),
            None => (None, 0),
        };
        self.session = Some(engine.new_session(&self.tools, instructions.as_deref())?);
        self.compactions = compactions;
        Ok(())
    }

    fn needs_compaction(&self, engine: &Engine, prompt: &str) -> fm_rs::Result<bool> {
        let usage = self.session().context_usage(&engine.limit)?;
        let prompt_tokens = estimate_tokens(prompt, engine.limit.chars_per_token);
        Ok(usage.estimated_tokens + prompt_tokens > usage.available_tokens)
    }

    /// Replaces the session with one seeded by a summary of the history.
    /// If summarizing fails, starts over instead of leaving the channel stuck.
    fn compact(&mut self, engine: &Engine, outcome: &mut Outcome) -> fm_rs::Result<()> {
        let transcript = self.session().transcript_json()?;
        let compacted = compact_transcript(&engine.model, &transcript, &engine.compaction)
            .and_then(|summary| {
                let instructions = compacted_instructions(None, &summary);
                engine.new_session(&self.tools, instructions.as_deref())
            });

        match compacted {
            Ok(session) => {
                self.session = Some(session);
                self.compactions += 1;
                outcome.compacted = true;
                info!(path = %self.path.display(), "compacted conversation");
            }
            Err(e) => {
                warn!(path = %self.path.display(), "compaction failed, starting fresh: {e}");
                self.session = Some(engine.new_session(&self.tools, None)?);
                self.compactions = 0;
                outcome.memory_lost = true;
            }
        }
        Ok(())
    }

    fn save(&self) -> io::Result<()> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let saved = SavedConversation {
            version: FORMAT_VERSION,
            transcript: session.transcript_json().map_err(io::Error::other)?,
            compactions: self.compactions,
        };
        // Write then rename so a crash never leaves a half-written file.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&saved)?)?;
        fs::rename(tmp, &self.path)
    }
}

fn stream<F>(
    session: &Session,
    prompt: &str,
    options: &GenerationOptions,
    text: &mut String,
    on_chunk: &mut F,
) -> fm_rs::Result<()>
where
    F: FnMut(&str) + Send,
{
    session.stream_response(prompt, options, |chunk| {
        text.push_str(chunk);
        on_chunk(chunk);
    })
}

/// Whether `error` came from the tool bridge rather than the model itself,
/// e.g. the model calling a tool that doesn't exist. Our own tools never
/// return errors, so this is the only way a tool call fails a turn.
fn is_tool_bridge_error(error: &FmError) -> bool {
    matches!(error, FmError::GenerationError(message) if message.contains("invoking the tool"))
}

fn read_saved(path: &Path) -> Option<SavedConversation> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!(path = %path.display(), "failed to read conversation: {e}");
            return None;
        }
    };
    match serde_json::from_slice::<SavedConversation>(&bytes) {
        Ok(saved) if saved.version == FORMAT_VERSION => Some(saved),
        Ok(saved) => {
            warn!(path = %path.display(), version = saved.version, "unknown conversation format");
            None
        }
        Err(e) => {
            warn!(path = %path.display(), "corrupt conversation file: {e}");
            None
        }
    }
}

/// Hands out one [`Conversation`] per channel, creating them on demand.
pub struct ConversationStore {
    engine: Arc<Engine>,
    toolkit: Toolkit,
    dir: PathBuf,
    conversations: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<Conversation>>>>,
}

impl ConversationStore {
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

#[cfg(test)]
mod tests {
    //! These tests talk to the real on-device model, so they need a Mac with
    //! Apple Intelligence enabled. Run them with `cargo test -- --ignored`.

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bean-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn engine() -> Engine {
        let engine = Engine::new().expect("model");
        engine.ensure_available().expect("Apple Intelligence available");
        engine
    }

    /// Tools call `Handle::block_on` from a dispatch thread, so the runtime
    /// must drive its own timers and IO: a multi-threaded one.
    fn runtime() -> &'static tokio::runtime::Runtime {
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
    }

    fn open_store(engine: Engine, dir: &Path) -> ConversationStore {
        let handle = runtime().handle().clone();
        let reminders = crate::reminders::Reminders::open(dir.join("reminders.json"), handle.clone()).unwrap();
        ConversationStore::open(engine, Toolkit::new(handle, reminders), dir).unwrap()
    }

    fn ask(store: &ConversationStore, channel: u64, prompt: &str) -> (String, Outcome) {
        let conversation = store.conversation(channel, None);
        let mut conversation = conversation.try_lock().expect("uncontended");
        conversation.respond(&store.engine(), Some(42), prompt, |_| {}).expect("response")
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn remembers_across_turns_and_restarts() {
        let dir = temp_dir("restart");

        let store = open_store(engine(), &dir);
        ask(&store, 1, "Remember this: my cat is named Biscuit. Just reply OK.");
        drop(store);

        let store = open_store(engine(), &dir);
        let (reply, _) = ask(&store, 1, "What is my cat's name? Answer with just the name.");
        assert!(reply.to_lowercase().contains("biscuit"), "reply was {reply:?}");

        let (reply, _) = ask(&store, 2, "What is my cat's name? If you don't know, say UNKNOWN.");
        assert!(!reply.to_lowercase().contains("biscuit"), "channels leaked: {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn streams_reply_in_chunks() {
        let dir = temp_dir("stream");
        let store = open_store(engine(), &dir);
        let conversation = store.conversation(1, None);
        let mut conversation = conversation.try_lock().unwrap();

        let mut chunks = Vec::new();
        let (text, _) = conversation
            .respond(&store.engine(), None, "Write three sentences about the sea.", |chunk| {
                chunks.push(chunk.to_owned());
            })
            .unwrap();

        assert!(chunks.len() > 1, "expected incremental chunks, got {chunks:?}");
        assert_eq!(chunks.concat(), text);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn calls_tools_and_keeps_them_after_restart() {
        let dir = temp_dir("tools");

        let store = open_store(engine(), &dir);
        let (reply, outcome) = ask(&store, 1, "What is today's date?");
        assert_eq!(outcome.tools_used, ["Checked the time"], "reply was {reply:?}");
        drop(store);

        // A restored conversation must still be able to call tools.
        let store = open_store(engine(), &dir);
        let (reply, outcome) = ask(&store, 1, "What's the weather like in Seoul right now?");
        assert_eq!(outcome.tools_used, ["Checked the weather in Seoul"], "reply was {reply:?}");
        assert!(reply.contains('°'), "reply was {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn answering_without_tools_keeps_memory_and_tools() {
        let dir = temp_dir("no-tools");
        let store = open_store(engine(), &dir);
        ask(&store, 1, "Remember this: my cat is named Biscuit. Just reply OK.");

        let reply = {
            let conversation = store.conversation(1, None);
            let mut conversation = conversation.try_lock().unwrap();
            let mut text = String::new();
            conversation
                .answer_without_tools(&store.engine(), "What is my cat's name?", &mut text, &mut |_| {})
                .unwrap();
            text
        };
        assert!(reply.to_lowercase().contains("biscuit"), "reply was {reply:?}");

        let (reply, outcome) = ask(&store, 1, "What is today's date?");
        assert_eq!(outcome.tools_used, ["Checked the time"], "reply was {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn sets_reminders_for_the_speaker() {
        let dir = temp_dir("reminder");
        let handle = runtime().handle().clone();
        let reminders = crate::reminders::Reminders::open(dir.join("reminders.json"), handle.clone()).unwrap();
        let store = ConversationStore::open(engine(), Toolkit::new(handle, Arc::clone(&reminders)), &dir).unwrap();
        // The reminder tool doesn't touch Discord until delivery, so a
        // client that never connects is enough.
        let discord = DiscordChannel {
            http: Arc::new(serenity::all::Http::new("")),
            channel_id: serenity::all::ChannelId::new(5),
            guild_id: None,
        };
        let conversation = store.conversation(5, Some(discord));
        let mut conversation = conversation.try_lock().unwrap();
        let (reply, outcome) = conversation
            .respond(&store.engine(), Some(42), "Remind me in 2 hours to stretch.", |_| {})
            .unwrap();

        let pending = reminders.pending_for(42);
        assert_eq!(pending.len(), 1, "reply was {reply:?}, tools {:?}", outcome.tools_used);
        assert!(pending[0].text.to_lowercase().contains("stretch"), "{pending:?}");
        let delay = pending[0].due_at - crate::reminders::unix_now();
        assert!((110 * 60..=120 * 60).contains(&delay), "due in {delay}s");
        assert_eq!(pending[0].channel_id, 5);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn compacts_when_context_fills_up() {
        let dir = temp_dir("compact");
        let model = SystemLanguageModel::new().unwrap();
        let tiny = ContextLimit::new(500).with_reserved_response_tokens(100);
        let store = open_store(Engine::with_limit(model, tiny), &dir);

        ask(&store, 1, "My name is Juniper and I live in Lisbon. Reply OK.");
        let mut compacted = false;
        for topic in ["volcanoes", "octopuses", "the Roman empire", "jazz"] {
            let (_, outcome) = ask(&store, 1, &format!("Write a detailed paragraph about {topic}."));
            compacted |= outcome.compacted;
        }
        assert!(compacted, "expected the tiny context to force a compaction");

        let status = {
            let conversation = store.conversation(1, None);
            let mut conversation = conversation.try_lock().unwrap();
            conversation.status(&store.engine()).unwrap()
        };
        assert!(status.compactions > 0);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn reset_forgets_everything() {
        let dir = temp_dir("reset");
        let store = open_store(engine(), &dir);
        ask(&store, 1, "Remember the code word: PELICAN. Reply OK.");
        assert!(dir.join("1.json").exists());

        store.conversation(1, None).try_lock().unwrap().reset().unwrap();
        assert!(!dir.join("1.json").exists());

        let (reply, _) = ask(&store, 1, "What was the code word? If you don't know, say UNKNOWN.");
        assert!(!reply.to_uppercase().contains("PELICAN"), "reply was {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }
}
