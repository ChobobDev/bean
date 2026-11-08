//! One channel's conversation with the model.
//!
//! The model sees the whole conversation, not a single message. After every
//! turn the transcript is saved to disk, and it's restored the first time
//! the channel is used again, so memory survives restarts.
//!
//! Before each turn we estimate how full the context window is. When the
//! next prompt wouldn't fit, the history is summarized and the conversation
//! continues from a fresh session seeded with that summary.
//!
//! Every session is created with the channel's tools. Restoring a saved
//! transcript directly would drop them, so a restored conversation is
//! replayed into a new session's instructions instead (see
//! [`transcript`]).

use std::io;
use std::path::PathBuf;

use fm_rs::{
    Error as FmError, GenerationOptions, Session, compact_transcript, compacted_instructions,
    estimate_tokens,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::Engine;
use super::transcript;
use crate::storage;
use crate::tools::ChannelTools;

/// Bump when [`SavedConversation`] changes shape.
const FORMAT_VERSION: u32 = 1;

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

/// How much of the context window a conversation uses.
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

/// One channel's conversation. Lock it before use; a model session can
/// only serve one request at a time.
pub struct Conversation {
    path: PathBuf,
    session: Option<Session>,
    compactions: u32,
    tools: ChannelTools,
}

impl Conversation {
    pub(super) fn new(path: PathBuf, tools: ChannelTools) -> Self {
        Self {
            path,
            session: None,
            compactions: 0,
            tools,
        }
    }

    /// Sends `prompt` from `author_id` and streams the reply to `on_chunk`
    /// as it's generated. Returns the full reply. Blocks; call it from a
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
        let result = self.run_turn(engine, prompt, on_chunk);
        let tools_used = self.tools.finish_turn();
        result.map(|(text, outcome)| {
            (
                text,
                Outcome {
                    tools_used,
                    ..outcome
                },
            )
        })
    }

    /// Forgets the conversation, including its saved transcript.
    pub fn reset(&mut self) -> io::Result<()> {
        self.session = None;
        self.compactions = 0;
        storage::remove(&self.path)
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

    fn run_turn<F>(
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
        let mut result = stream(
            self.session(),
            prompt,
            &engine.options,
            &mut text,
            &mut on_chunk,
        );
        // Token counts are estimates, so the model may still reject the
        // prompt. If nothing was shown yet, compact once and retry rather
        // than failing the turn.
        if matches!(result, Err(FmError::ContextSizeExceeded(_)))
            && text.is_empty()
            && !(outcome.compacted || outcome.memory_lost)
        {
            self.compact(engine, &mut outcome)?;
            result = stream(
                self.session(),
                prompt,
                &engine.options,
                &mut text,
                &mut on_chunk,
            );
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
    pub(super) fn answer_without_tools<F>(
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
        let plain = engine.session_without_tools(history.as_deref())?;
        let result = stream(&plain, prompt, &engine.options, text, on_chunk);

        let history = transcript::restored_instructions(&plain.transcript_json()?);
        self.session = Some(engine.session_with_tools(&self.tools, history.as_deref())?);
        result
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
        let (instructions, compactions) = match self.load() {
            Some(saved) => (
                transcript::restored_instructions(&saved.transcript),
                saved.compactions,
            ),
            None => (None, 0),
        };
        self.session = Some(engine.session_with_tools(&self.tools, instructions.as_deref())?);
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
                engine.session_with_tools(&self.tools, instructions.as_deref())
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
                self.session = Some(engine.session_with_tools(&self.tools, None)?);
                self.compactions = 0;
                outcome.memory_lost = true;
            }
        }
        Ok(())
    }

    /// Reads the saved conversation, if there's a usable one.
    fn load(&self) -> Option<SavedConversation> {
        match storage::read_json::<SavedConversation>(&self.path) {
            Ok(Some(saved)) if saved.version == FORMAT_VERSION => Some(saved),
            Ok(Some(saved)) => {
                warn!(path = %self.path.display(), version = saved.version, "unknown conversation format");
                None
            }
            Ok(None) => None,
            Err(e) => {
                warn!(path = %self.path.display(), "unreadable conversation file: {e}");
                None
            }
        }
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
        storage::write_json(&self.path, &saved)
    }
}

/// Streams a response to `prompt`, appending it to `text` and forwarding
/// each chunk to `on_chunk`.
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
