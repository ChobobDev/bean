//! Bean, a Discord bot powered by Apple's on-device Foundation Models.
//!
//! - [`discord`] receives messages and slash commands and streams replies.
//! - [`ai`] keeps each channel's conversation with the model.
//! - [`tools`] are the actions the model can take while answering.
//! - [`reminders`] stores and delivers reminders set through a tool.

pub mod ai;
pub mod app;
pub mod config;
pub mod discord;
pub mod reminders;
pub mod storage;
pub mod ticker;
pub mod tools;
