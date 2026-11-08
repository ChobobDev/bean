//! Everything that talks to Discord: events, chat, slash commands, and
//! rendering replies.

mod chat;
mod commands;
mod errors;
mod handler;
mod mentions;
mod pages;
mod reply;

pub use handler::{Handler, Services};
