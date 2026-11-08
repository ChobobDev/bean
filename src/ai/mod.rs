//! Talking to Apple's on-device Foundation Models: per-channel memory,
//! summarizing, and restoring saved conversations.

mod conversation;
mod engine;
mod store;
pub mod transcript;

#[cfg(test)]
mod tests;

pub use conversation::{Conversation, Outcome, Status};
pub use engine::{Engine, MAX_RESPONSE_TOKENS};
pub use store::ConversationStore;
