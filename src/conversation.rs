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

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fm_rs::{
    CompactionConfig, ContextLimit, Error as FmError, GenerationOptions, Session,
    SystemLanguageModel, compact_transcript, estimate_tokens, session_from_summary,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

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
}

/// What happened to memory while producing a reply.
#[derive(Debug, Default, Clone, Copy)]
pub struct Outcome {
    /// Older history was summarized to make room.
    pub compacted: bool,
    /// Summarizing failed, so the conversation was restarted from scratch.
    pub memory_lost: bool,
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
}

impl Conversation {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            session: None,
            compactions: 0,
        }
    }

    /// Sends `prompt` and returns the model's reply. Blocks; call it from
    /// a blocking thread.
    pub fn respond(&mut self, engine: &Engine, prompt: &str) -> fm_rs::Result<(String, Outcome)> {
        self.ensure_loaded(engine)?;
        let mut outcome = Outcome::default();

        if self.needs_compaction(engine, prompt)? {
            self.compact(engine, &mut outcome)?;
        }

        let mut result = self.session().respond(prompt, &engine.options);
        // Token counts are estimates, so the model may still reject the
        // prompt. Compact once and retry rather than failing the turn.
        if matches!(result, Err(FmError::ContextSizeExceeded(_)))
            && !(outcome.compacted || outcome.memory_lost)
        {
            self.compact(engine, &mut outcome)?;
            result = self.session().respond(prompt, &engine.options);
        }

        if let Err(e) = self.save() {
            warn!(path = %self.path.display(), "failed to save conversation: {e}");
        }
        result.map(|response| (response.into_content(), outcome))
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
        let restored = read_saved(&self.path).and_then(|saved| {
            match Session::from_transcript(&engine.model, &saved.transcript) {
                Ok(session) => Some((session, saved.compactions)),
                Err(e) => {
                    warn!(path = %self.path.display(), "discarding unreadable transcript: {e}");
                    None
                }
            }
        });
        let (session, compactions) = match restored {
            Some(restored) => restored,
            None => (Session::new(&engine.model)?, 0),
        };
        self.session = Some(session);
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
            .and_then(|summary| session_from_summary(&engine.model, None, &summary));

        match compacted {
            Ok(session) => {
                self.session = Some(session);
                self.compactions += 1;
                outcome.compacted = true;
                info!(path = %self.path.display(), "compacted conversation");
            }
            Err(e) => {
                warn!(path = %self.path.display(), "compaction failed, starting fresh: {e}");
                self.session = Some(Session::new(&engine.model)?);
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
    dir: PathBuf,
    conversations: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<Conversation>>>>,
}

impl ConversationStore {
    pub fn open(engine: Engine, dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            engine: Arc::new(engine),
            dir,
            conversations: Mutex::new(HashMap::new()),
        })
    }

    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }

    /// Returns the conversation for `channel_id`. Messages in the same
    /// channel queue up on its lock and are answered in order.
    pub fn conversation(&self, channel_id: u64) -> Arc<tokio::sync::Mutex<Conversation>> {
        let mut conversations = self.conversations.lock().expect("store lock poisoned");
        let conversation = conversations.entry(channel_id).or_insert_with(|| {
            let path = self.dir.join(format!("{channel_id}.json"));
            Arc::new(tokio::sync::Mutex::new(Conversation::new(path)))
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

    fn ask(store: &ConversationStore, channel: u64, prompt: &str) -> (String, Outcome) {
        let conversation = store.conversation(channel);
        let mut conversation = conversation.try_lock().expect("uncontended");
        conversation.respond(&store.engine(), prompt).expect("response")
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn remembers_across_turns_and_restarts() {
        let dir = temp_dir("restart");

        let store = ConversationStore::open(engine(), &dir).unwrap();
        ask(&store, 1, "Remember this: my cat is named Biscuit. Just reply OK.");
        drop(store);

        let store = ConversationStore::open(engine(), &dir).unwrap();
        let (reply, _) = ask(&store, 1, "What is my cat's name? Answer with just the name.");
        assert!(reply.to_lowercase().contains("biscuit"), "reply was {reply:?}");

        let (reply, _) = ask(&store, 2, "What is my cat's name? If you don't know, say UNKNOWN.");
        assert!(!reply.to_lowercase().contains("biscuit"), "channels leaked: {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn compacts_when_context_fills_up() {
        let dir = temp_dir("compact");
        let model = SystemLanguageModel::new().unwrap();
        let tiny = ContextLimit::new(500).with_reserved_response_tokens(100);
        let store = ConversationStore::open(Engine::with_limit(model, tiny), &dir).unwrap();

        ask(&store, 1, "My name is Juniper and I live in Lisbon. Reply OK.");
        let mut compacted = false;
        for topic in ["volcanoes", "octopuses", "the Roman empire", "jazz"] {
            let (_, outcome) = ask(&store, 1, &format!("Write a detailed paragraph about {topic}."));
            compacted |= outcome.compacted;
        }
        assert!(compacted, "expected the tiny context to force a compaction");

        let status = {
            let conversation = store.conversation(1);
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
        let store = ConversationStore::open(engine(), &dir).unwrap();
        ask(&store, 1, "Remember the code word: PELICAN. Reply OK.");
        assert!(dir.join("1.json").exists());

        store.conversation(1).try_lock().unwrap().reset().unwrap();
        assert!(!dir.join("1.json").exists());

        let (reply, _) = ask(&store, 1, "What was the code word? If you don't know, say UNKNOWN.");
        assert!(!reply.to_uppercase().contains("PELICAN"), "reply was {reply:?}");

        fs::remove_dir_all(dir).unwrap();
    }
}
