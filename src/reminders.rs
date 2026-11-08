//! Reminders that ping a user in a channel after a delay.
//!
//! Reminders are saved to disk when created and removed once delivered, so
//! they survive restarts. Any that came due while Bean was offline are
//! delivered as soon as it reconnects.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serenity::all::{ChannelId, CreateAllowedMentions, CreateMessage, Http, UserId};
use tokio::runtime::Handle;
use tracing::{info, warn};

use crate::storage;

/// Longest delay a reminder can have.
pub const MAX_DELAY: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Most reminders a single user can have pending at once.
pub const MAX_PER_USER: usize = 10;
/// Longest reminder text, in characters.
pub const MAX_TEXT_CHARS: usize = 300;

/// Reminders delivered later than this are marked as late.
const LATE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: u64,
    pub channel_id: u64,
    pub user_id: u64,
    /// Seconds since the Unix epoch.
    pub due_at: u64,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReminderError {
    TooSoon,
    TooFar,
    EmptyText,
    TooManyPending,
    Storage(String),
}

impl std::fmt::Display for ReminderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSoon => write!(f, "reminders must be at least one minute away"),
            Self::TooFar => write!(f, "reminders can be at most 7 days away"),
            Self::EmptyText => write!(f, "the reminder needs some text"),
            Self::TooManyPending => {
                write!(f, "this user already has {MAX_PER_USER} reminders pending")
            }
            Self::Storage(e) => write!(f, "couldn't save the reminder: {e}"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    next_id: u64,
    pending: Vec<Reminder>,
}

pub struct Reminders {
    path: PathBuf,
    runtime: Handle,
    state: Mutex<State>,
    /// Set once Discord is connected; reminders are only scheduled after.
    http: OnceLock<Arc<Http>>,
}

impl Reminders {
    /// Loads saved reminders from `path`. Nothing is delivered until
    /// [`start`](Self::start) is called.
    pub fn open(path: impl Into<PathBuf>, runtime: Handle) -> io::Result<Arc<Self>> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let state = match storage::read_json(&path) {
            Ok(state) => state.unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                warn!(path = %path.display(), "ignoring corrupt reminders file: {e}");
                State::default()
            }
            Err(e) => return Err(e),
        };
        Ok(Arc::new(Self {
            path,
            runtime,
            state: Mutex::new(state),
            http: OnceLock::new(),
        }))
    }

    /// Starts delivering reminders, including any saved ones. Calling it
    /// again (e.g. after a reconnect) does nothing.
    pub fn start(self: &Arc<Self>, http: Arc<Http>) {
        if self.http.set(http).is_err() {
            return;
        }
        let pending = self.state.lock().expect("reminders lock").pending.clone();
        info!(count = pending.len(), "scheduling saved reminders");
        for reminder in pending {
            self.schedule(reminder);
        }
    }

    /// Creates a reminder for `user_id` in `channel_id`, `delay` from now.
    pub fn add(
        self: &Arc<Self>,
        channel_id: u64,
        user_id: u64,
        delay: Duration,
        text: &str,
    ) -> Result<Reminder, ReminderError> {
        if delay < Duration::from_secs(60) {
            return Err(ReminderError::TooSoon);
        }
        if delay > MAX_DELAY {
            return Err(ReminderError::TooFar);
        }
        let text: String = text.trim().chars().take(MAX_TEXT_CHARS).collect();
        if text.is_empty() {
            return Err(ReminderError::EmptyText);
        }

        let reminder = {
            let mut state = self.state.lock().expect("reminders lock");
            if state
                .pending
                .iter()
                .filter(|r| r.user_id == user_id)
                .count()
                >= MAX_PER_USER
            {
                return Err(ReminderError::TooManyPending);
            }
            state.next_id += 1;
            let reminder = Reminder {
                id: state.next_id,
                channel_id,
                user_id,
                due_at: unix_now() + delay.as_secs(),
                text,
            };
            state.pending.push(reminder.clone());
            if let Err(e) = storage::write_json(&self.path, &*state) {
                state.pending.pop();
                return Err(ReminderError::Storage(e.to_string()));
            }
            reminder
        };

        if self.http.get().is_some() {
            self.schedule(reminder.clone());
        }
        Ok(reminder)
    }

    /// Pending reminders for `user_id`, soonest first.
    pub fn pending_for(&self, user_id: u64) -> Vec<Reminder> {
        let state = self.state.lock().expect("reminders lock");
        let mut reminders: Vec<_> = state
            .pending
            .iter()
            .filter(|r| r.user_id == user_id)
            .cloned()
            .collect();
        reminders.sort_by_key(|r| r.due_at);
        reminders
    }

    fn schedule(self: &Arc<Self>, reminder: Reminder) {
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let wait = reminder.due_at.saturating_sub(unix_now());
            tokio::time::sleep(Duration::from_secs(wait)).await;
            this.deliver(&reminder).await;
        });
    }

    async fn deliver(&self, reminder: &Reminder) {
        let Some(http) = self.http.get() else { return };
        let late = unix_now().saturating_sub(reminder.due_at) > LATE_AFTER.as_secs();
        let mut content = format!("⏰ <@{}> {}", reminder.user_id, reminder.text);
        if late {
            content.push_str("\n-# Sorry this is late, I was offline when it was due.");
        }
        // Ping the person who asked, and nobody else.
        let mentions = CreateAllowedMentions::new().users([UserId::new(reminder.user_id)]);
        let message = CreateMessage::new()
            .content(content)
            .allowed_mentions(mentions);
        if let Err(e) = ChannelId::new(reminder.channel_id)
            .send_message(http, message)
            .await
        {
            // Usually the channel is gone or Bean lost access. Retrying
            // wouldn't help, so drop it.
            warn!(
                id = reminder.id,
                "failed to deliver reminder, dropping it: {e}"
            );
        }

        let mut state = self.state.lock().expect("reminders lock");
        state.pending.retain(|r| r.id != reminder.id);
        if let Err(e) = storage::write_json(&self.path, &*state) {
            warn!("failed to save reminders: {e}");
        }
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bean-reminders-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("reminders.json")
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn persists_across_reopen() {
        let rt = runtime();
        let path = temp_path("persist");
        let reminders = Reminders::open(&path, rt.handle().clone()).unwrap();
        let added = reminders.add(1, 42, HOUR, "  stretch  ").unwrap();
        assert_eq!(added.text, "stretch");
        drop(reminders);

        let reopened = Reminders::open(&path, rt.handle().clone()).unwrap();
        assert_eq!(reopened.pending_for(42), vec![added]);
        assert!(reopened.pending_for(7).is_empty());
    }

    #[test]
    fn ids_stay_unique_after_reopen() {
        let rt = runtime();
        let path = temp_path("ids");
        let first = Reminders::open(&path, rt.handle().clone())
            .unwrap()
            .add(1, 1, HOUR, "a")
            .unwrap();
        let second = Reminders::open(&path, rt.handle().clone())
            .unwrap()
            .add(1, 1, HOUR, "b")
            .unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn rejects_out_of_range_delays_and_empty_text() {
        let rt = runtime();
        let reminders = Reminders::open(temp_path("limits"), rt.handle().clone()).unwrap();
        assert_eq!(
            reminders.add(1, 1, Duration::from_secs(5), "x"),
            Err(ReminderError::TooSoon)
        );
        assert_eq!(
            reminders.add(1, 1, MAX_DELAY + HOUR, "x"),
            Err(ReminderError::TooFar)
        );
        assert_eq!(
            reminders.add(1, 1, HOUR, "   "),
            Err(ReminderError::EmptyText)
        );
    }

    #[test]
    fn caps_pending_reminders_per_user() {
        let rt = runtime();
        let reminders = Reminders::open(temp_path("cap"), rt.handle().clone()).unwrap();
        for i in 0..MAX_PER_USER {
            reminders.add(1, 1, HOUR, &format!("r{i}")).unwrap();
        }
        assert_eq!(
            reminders.add(1, 1, HOUR, "one more"),
            Err(ReminderError::TooManyPending)
        );
        // Other users aren't affected.
        assert!(reminders.add(1, 2, HOUR, "fine").is_ok());
    }

    #[test]
    fn truncates_long_text() {
        let rt = runtime();
        let reminders = Reminders::open(temp_path("long"), rt.handle().clone()).unwrap();
        let added = reminders.add(1, 1, HOUR, &"a".repeat(1000)).unwrap();
        assert_eq!(added.text.chars().count(), MAX_TEXT_CHARS);
    }
}
