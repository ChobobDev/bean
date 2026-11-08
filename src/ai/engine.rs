use std::sync::Arc;

use fm_rs::{CompactionConfig, ContextLimit, GenerationOptions, Session, SystemLanguageModel};
use tracing::warn;

use crate::tools::ChannelTools;

/// Upper bound on the length of a single reply.
pub const MAX_RESPONSE_TOKENS: u32 = 500;

/// The on-device model plus the settings shared by every conversation.
pub struct Engine {
    pub(super) model: SystemLanguageModel,
    pub(super) limit: ContextLimit,
    pub(super) options: GenerationOptions,
    pub(super) compaction: CompactionConfig,
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

    /// Answers a single prompt, with no memory and no tools. Blocks.
    pub fn ask_once(&self, prompt: &str) -> fm_rs::Result<String> {
        let session = self.session_without_tools(None)?;
        Ok(session.respond(prompt, &self.options)?.into_content())
    }

    /// A session that can call `tools`.
    pub(super) fn session_with_tools(
        &self,
        tools: &ChannelTools,
        instructions: Option<&str>,
    ) -> fm_rs::Result<Session> {
        let mut builder = Session::builder(&self.model);
        if let Some(instructions) = instructions {
            builder = builder.instructions(instructions);
        }
        for tool in tools.tools() {
            builder = builder.tool(Arc::clone(tool));
        }
        builder.build()
    }

    /// A session that can't call any tools.
    pub(super) fn session_without_tools(
        &self,
        instructions: Option<&str>,
    ) -> fm_rs::Result<Session> {
        match instructions {
            Some(instructions) => Session::with_instructions(&self.model, instructions),
            None => Session::new(&self.model),
        }
    }
}
