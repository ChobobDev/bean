//! A Discord reply that fills in while the model is still generating.

use std::sync::Arc;

use serenity::all::{
    ChannelId, CreateAllowedMentions, CreateMessage, EditMessage, Http, Message, MessageReference,
};
use tracing::warn;

use super::pages::split_pages;

/// Page size, kept below Discord's 2000 character limit for safety.
const PAGE_CHARS: usize = 1900;

/// Accumulates streamed text and mirrors it into one or more Discord
/// messages. The first page replies to the user's message; later pages
/// continue in the same channel.
pub struct StreamingReply {
    http: Arc<Http>,
    channel: ChannelId,
    reply_to: MessageReference,
    text: String,
    /// Messages posted so far, with the content each currently shows.
    sent: Vec<(Message, String)>,
    dirty: bool,
    failed: bool,
}

impl StreamingReply {
    pub fn new(http: Arc<Http>, reply_to: &Message) -> Self {
        Self {
            http,
            channel: reply_to.channel_id,
            reply_to: MessageReference::from(reply_to),
            text: String::new(),
            sent: Vec::new(),
            dirty: false,
            failed: false,
        }
    }

    pub fn push(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        self.dirty = true;
    }

    /// Adds a note below the reply, or makes it the whole reply if nothing
    /// was generated.
    pub fn push_note(&mut self, note: &str) {
        if !self.text.trim().is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(note);
        self.dirty = true;
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Whether anything is visible in Discord yet.
    pub fn has_posted(&self) -> bool {
        !self.sent.is_empty()
    }

    /// Brings Discord up to date with the text received so far. Only pages
    /// whose content changed are sent or edited.
    pub async fn flush(&mut self) {
        if !self.dirty || self.failed || self.is_empty() {
            return;
        }
        self.dirty = false;

        for (i, page) in split_pages(&self.text, PAGE_CHARS).into_iter().enumerate() {
            let result = match self.sent.get_mut(i) {
                Some((_, shown)) if *shown == page => continue,
                Some((message, shown)) => {
                    let edit = EditMessage::new()
                        .content(&page)
                        .allowed_mentions(no_pings());
                    let result = message.edit(&self.http, edit).await;
                    *shown = page;
                    result
                }
                None => {
                    let mut create = CreateMessage::new()
                        .content(&page)
                        .allowed_mentions(no_pings());
                    if i == 0 {
                        create = create.reference_message(self.reply_to.clone());
                    }
                    self.channel
                        .send_message(&self.http, create)
                        .await
                        .map(|message| {
                            self.sent.push((message, page));
                        })
                }
            };
            if let Err(e) = result {
                // Usually missing permissions; retrying every second would
                // only spam the logs.
                warn!(channel = %self.channel, "failed to update reply, giving up: {e}");
                self.failed = true;
                return;
            }
        }
    }
}

/// Lets replies reference the user's message without pinging anyone, so
/// model output can never trigger @everyone or role pings.
pub fn no_pings() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
}
